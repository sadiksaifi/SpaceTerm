use super::*;
use spaceterm_core::{
    identity::{ClientId, DeviceId, PairingId, ServerId},
    pairing::{CredentialReference, LocalPairingGeneration},
};
use spaceterm_protocol::authentication::ServerPin;
use std::{
    fs,
    os::unix::{
        fs::{MetadataExt, PermissionsExt},
        net::UnixListener,
    },
    path::Path,
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::UnixListener as AsyncListener,
};

// Harness waits include native process startup and reaping under build load.
// They do not change the production dial deadline or pending-operation checks.
const HARNESS_TIMEOUT: Duration = Duration::from_secs(30);

struct Directory(PathBuf);
impl Directory {
    fn new() -> Self {
        let mut bytes = [0; 8];
        SystemRandom::new().fill(&mut bytes).unwrap();
        let path = std::env::temp_dir().join(format!("ct-{}", URL_SAFE_NO_PAD.encode(bytes)));
        fs::create_dir(&path).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        Self(path)
    }
    fn authority(&self) -> PrivateIpcDirectory {
        PrivateIpcDirectory::open(&self.0).unwrap()
    }
}
impl Drop for Directory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn target(route: RouteDescription) -> ConnectionTarget {
    let server = ServerId::new([1; 16]);
    ConnectionTarget::new(
        ServerPin {
            identity: server.bytes(),
            public_key: [2; 32],
        },
        DeviceId::new([3; 16]),
        PairingId::new(server, 1).unwrap(),
        ClientId::new(server, 1).unwrap(),
        route,
        CredentialReference::new([4; 16]),
        LocalPairingGeneration::new(1).unwrap(),
    )
    .unwrap()
}
fn entries(path: &Path) -> Vec<OsString> {
    let mut entries: Vec<_> = fs::read_dir(path)
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    entries.sort();
    entries
}

fn native_registration_fixture() -> (
    tempfile::TempDir,
    spaceterm_host::account_service::test_support::AccountServiceFixture,
    spaceterm_host::account_service::RegisteredServer,
) {
    use spaceterm_host::{
        account_service::test_support::AccountServiceFixture,
        startup::{BuildIdentity, DesiredServerState},
    };
    #[cfg(target_os = "macos")]
    let parent = "/private/tmp";
    #[cfg(not(target_os = "macos"))]
    let parent = "/var/tmp";
    let directory = tempfile::tempdir_in(parent).unwrap();
    let fixture = AccountServiceFixture::new(directory.path(), BuildIdentity::Development).unwrap();
    let registration = fixture.initialize(DesiredServerState::Running).unwrap();
    (directory, fixture, registration)
}

#[tokio::test]
async fn restricted_local_renewal_requires_original_pin_route_and_build_registration() {
    use spaceterm_host::{
        account_service::test_support::AccountServiceFixture, startup::BuildIdentity,
    };
    let (directory, fixture, registration) = native_registration_fixture();
    let endpoint = registration.endpoint().unwrap();
    let original = LocalRouteAuthority::new(endpoint.clone()).unwrap();
    let route = original.description().clone();
    let other = LocalRouteAuthority::new(
        PrivateIpcDirectory::open(endpoint.path().parent().unwrap())
            .unwrap()
            .endpoint("other")
            .unwrap(),
    )
    .unwrap();
    let other_route = other.description().clone();
    let listener = UnixListener::bind(endpoint.path()).unwrap();
    fs::set_permissions(endpoint.path(), fs::Permissions::from_mode(0o600)).unwrap();
    listener.set_nonblocking(true).unwrap();
    let connector = NativeRouteConnector::new(vec![original, other], None)
        .unwrap()
        .with_restricted_local_discovery(crate::NativeClientProvider::from_supervisor(
            fixture.reopen(),
        ));
    let wrong_pin = ServerPin {
        public_key: [2; 32],
        ..registration.server_pin()
    };
    assert!(matches!(
        connector
            .open_restricted_pairing_route(wrong_pin, &route, None)
            .await,
        Err(error) if error.class() == ClientFailureClass::PinMismatch
    ));
    assert!(matches!(
        connector
            .open_restricted_pairing_route(registration.server_pin(), &other_route, None)
            .await,
        Err(error) if error.class() == ClientFailureClass::StaleAuthority
    ));
    // A missing registration for another build cannot borrow the supplied
    // static endpoint, even though it has the original route and owning UID.
    let foreign = AccountServiceFixture::new(directory.path(), BuildIdentity::Preflight).unwrap();
    let foreign_connector =
        NativeRouteConnector::new(vec![LocalRouteAuthority::new(endpoint).unwrap()], None)
            .unwrap()
            .with_restricted_local_discovery(crate::NativeClientProvider::from_supervisor(
                foreign.reopen(),
            ));
    assert!(matches!(
        foreign_connector
            .open_restricted_pairing_route(registration.server_pin(), &route, None)
            .await,
        Err(error) if error.class() == ClientFailureClass::Unavailable
    ));
    assert!(matches!(
        listener.accept(),
        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock
    ));
}

#[tokio::test]
async fn restricted_local_renewal_rejects_unsafe_runtime_without_repair_or_ordinary_admission() {
    let (_directory, fixture, registration) = native_registration_fixture();
    let endpoint = registration.endpoint().unwrap();
    let runtime = endpoint.path().parent().unwrap();
    let authority = LocalRouteAuthority::new(endpoint.clone()).unwrap();
    let route = authority.description().clone();
    let listener = UnixListener::bind(endpoint.path()).unwrap();
    fs::set_permissions(endpoint.path(), fs::Permissions::from_mode(0o600)).unwrap();
    listener.set_nonblocking(true).unwrap();
    let native = crate::NativeClientProvider::from_supervisor(fixture.reopen());
    let connector = NativeRouteConnector::new(vec![authority], None)
        .unwrap()
        .with_restricted_local_discovery(native.clone());
    fs::set_permissions(runtime, fs::Permissions::from_mode(0o755)).unwrap();
    assert!(matches!(
        connector
            .open_restricted_pairing_route(registration.server_pin(), &route, None)
            .await,
        Err(error) if error.class() == ClientFailureClass::UnsafeEndpoint
    ));
    assert_eq!(fs::metadata(runtime).unwrap().mode() & 0o7777, 0o755);
    assert!(matches!(
        listener.accept(),
        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock
    ));

    // Only the fixture repairs its directory. No invalid admission is cached,
    // and the read-only renewal does not grant ordinary or enrollment access.
    fs::set_permissions(runtime, fs::Permissions::from_mode(0o700)).unwrap();
    let empty = NativeRouteConnector::new(vec![], None)
        .unwrap()
        .with_restricted_local_discovery(native);
    let server = ServerId::new(registration.server_pin().identity);
    let target = ConnectionTarget::new(
        registration.server_pin(),
        DeviceId::new([3; 16]),
        PairingId::new(server, 1).unwrap(),
        ClientId::new(server, 1).unwrap(),
        route.clone(),
        CredentialReference::new([4; 16]),
        LocalPairingGeneration::new(1).unwrap(),
    )
    .unwrap();
    assert!(matches!(
        empty.open(&target, ConnectionPurpose::Ordinary).await,
        Err(error) if error.class() == ClientFailureClass::UnsafeEndpoint
    ));
    assert!(matches!(
        empty.open_pairing_route(registration.server_pin(), &route, None).await,
        Err(error) if error.class() == ClientFailureClass::UnsafeEndpoint
    ));
    assert!(matches!(
        listener.accept(),
        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock
    ));
    let connected = connector
        .open_restricted_pairing_route(registration.server_pin(), &route, None)
        .await
        .unwrap();
    assert!(connected.peer.local_account_verified);
    let (_stream, _) = listener.accept().unwrap();
    connected.sender.close().await.unwrap();
}

#[tokio::test]
async fn missing_or_refused_endpoint_is_unavailable_and_never_mutated() {
    let directory = Directory::new();
    let endpoint = directory.authority().endpoint("server").unwrap();
    let authority = LocalRouteAuthority::new(endpoint.clone()).unwrap();
    let target = target(authority.description().clone());
    let connector = NativeRouteConnector::new(vec![authority], None).unwrap();
    let before = entries(&directory.0);
    let error = match connector.open(&target, ConnectionPurpose::Ordinary).await {
        Ok(_) => panic!("missing endpoint connected"),
        Err(error) => error,
    };
    assert_eq!(error.class(), ClientFailureClass::Unavailable);
    assert_eq!(entries(&directory.0), before);
    let listener = UnixListener::bind(endpoint.path()).unwrap();
    let inode = fs::symlink_metadata(endpoint.path()).unwrap().ino();
    drop(listener);
    let before = entries(&directory.0);
    let error = match connector.open(&target, ConnectionPurpose::Ordinary).await {
        Ok(_) => panic!("refused endpoint connected"),
        Err(error) => error,
    };
    assert_eq!(error.class(), ClientFailureClass::Unavailable);
    assert_eq!(fs::symlink_metadata(endpoint.path()).unwrap().ino(), inode);
    assert_eq!(entries(&directory.0), before);
}

#[tokio::test]
async fn unsafe_endpoint_is_not_retried_or_repaired() {
    let directory = Directory::new();
    let endpoint = directory.authority().endpoint("server").unwrap();
    fs::write(endpoint.path(), b"private file").unwrap();
    let inode = fs::symlink_metadata(endpoint.path()).unwrap().ino();
    let authority = LocalRouteAuthority::new(endpoint.clone()).unwrap();
    let target = target(authority.description().clone());
    let connector = NativeRouteConnector::new(vec![authority], None).unwrap();
    let error = match connector.open(&target, ConnectionPurpose::Ordinary).await {
        Ok(_) => panic!("unsafe endpoint connected"),
        Err(error) => error,
    };
    assert_eq!(error.class(), ClientFailureClass::UnsafeEndpoint);
    assert_eq!(fs::symlink_metadata(endpoint.path()).unwrap().ino(), inode);
    assert_eq!(fs::read(endpoint.path()).unwrap(), b"private file");
}

#[tokio::test]
async fn route_data_without_explicit_local_authority_cannot_open_ipc() {
    let directory = Directory::new();
    let endpoint = directory.authority().endpoint("server").unwrap();
    let authority = LocalRouteAuthority::new(endpoint).unwrap();
    let target = target(authority.description().clone());
    let connector = NativeRouteConnector::new(vec![], None).unwrap();
    let error = match connector.open(&target, ConnectionPurpose::Ordinary).await {
        Ok(_) => panic!("unapproved local endpoint connected"),
        Err(error) => error,
    };
    assert_eq!(error.class(), ClientFailureClass::UnsafeEndpoint);
}

#[tokio::test]
async fn native_route_receive_cancellation_preserves_partial_framing_and_full_duplex() {
    let directory = Directory::new();
    let endpoint = directory.authority().endpoint("server").unwrap();
    let listener = AsyncListener::bind(endpoint.path()).unwrap();
    let authority = LocalRouteAuthority::new(endpoint.clone()).unwrap();
    let target = target(authority.description().clone());
    let connector = NativeRouteConnector::new(vec![authority], None).unwrap();
    let mut route = connector
        .open(&target, ConnectionPurpose::Ordinary)
        .await
        .unwrap();
    let (mut server, _) = listener.accept().await.unwrap();
    let prefix = 6u32.to_be_bytes();
    server.write_all(&prefix[..2]).await.unwrap();
    assert!(
        tokio::time::timeout(
            std::time::Duration::from_millis(10),
            route.receiver.receive()
        )
        .await
        .is_err()
    );
    // A blocked/cancelled read does not hold the outbound half hostage.
    route.sender.send(b"request").await.unwrap();
    let mut received = [0; 11];
    server.read_exact(&mut received).await.unwrap();
    assert_eq!(&received, b"\x00\x00\x00\x07request");
    server.write_all(&prefix[2..]).await.unwrap();
    server.write_all(b"ans").await.unwrap();
    assert!(
        tokio::time::timeout(
            std::time::Duration::from_millis(10),
            route.receiver.receive()
        )
        .await
        .is_err()
    );
    server.write_all(b"wer").await.unwrap();
    assert_eq!(
        route.receiver.receive().await.unwrap(),
        Some(b"answer".to_vec())
    );
    route.sender.close().await.unwrap();
    assert!(endpoint.path().exists());
}

#[test]
fn tunnel_socket_names_are_fresh_for_every_start_including_cancelled_attempts() {
    let directory = Directory::new();
    let allocator = TunnelEndpoints::new(directory.authority()).unwrap();
    let first = allocator.allocate().unwrap();
    let occupied = UnixListener::bind(first.path()).unwrap();
    let inode = fs::symlink_metadata(first.path()).unwrap().ino();
    let mut names = std::collections::BTreeSet::new();
    names.insert(first.path().to_owned());
    for _ in 0..1000 {
        let next = allocator.allocate().unwrap();
        assert!(names.insert(next.path().to_owned()));
        assert_eq!(next.path().file_name().unwrap().len(), 16);
    }
    assert_eq!(fs::symlink_metadata(first.path()).unwrap().ino(), inode);
    drop(occupied);
}

#[test]
fn native_failures_never_retain_or_format_raw_errors() {
    for failure in [
        TransportError::Unavailable,
        TransportError::TlsIdentityRejected,
        TransportError::UnsafeEndpoint,
    ] {
        let classified = classify(failure);
        assert!(!format!("{classified:?} {classified}").contains('/'));
    }
}

#[tokio::test]
async fn native_route_tunnel_cleanup_removes_only_its_registered_socket() {
    use std::os::unix::fs::FileTypeExt;
    let directory = Directory::new();
    let executable = directory.0.join("tunnel-fixture");
    // A test-only process boundary implements just OpenSSH's local forwarding
    // endpoint. It does not contact a host, enroll, or supply credentials.
    fs::write(
        &executable,
        r#"#!/usr/bin/python3 -I
import socket
import sys
forward = sys.argv[sys.argv.index('-L') + 1]
listener = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
listener.bind(forward.split(':', 1)[0])
listener.listen(8)
while True:
    peer, _ = listener.accept()
    while peer.recv(4096):
        pass
    peer.close()
"#,
    )
    .unwrap();
    fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
    let configuration = directory.0.join("ssh-config");
    fs::write(&configuration, b"").unwrap();
    let ssh = SshRouteConfiguration::new(
        executable,
        configuration,
        directory.0.clone(),
        vec![(OsString::from("PATH"), OsString::from("/usr/bin:/bin"))],
        TunnelEndpoints::new(directory.authority()).unwrap(),
        None,
    )
    .unwrap();
    let connector = NativeRouteConnector::new(vec![], Some(ssh)).unwrap();
    let target = target(
        RouteDescription::new(RouteParts::Ssh {
            destination: "fixture-host".into(),
            remote_socket: "/private/server".into(),
        })
        .unwrap(),
    );
    let sockets = || {
        fs::read_dir(&directory.0)
            .unwrap()
            .filter_map(|entry| {
                let entry = entry.unwrap();
                entry
                    .file_type()
                    .unwrap()
                    .is_socket()
                    .then(|| entry.file_name())
            })
            .collect::<std::collections::BTreeSet<_>>()
    };
    let first = connector
        .open(&target, ConnectionPurpose::Ordinary)
        .await
        .unwrap();
    let first_sockets = sockets();
    assert_eq!(first_sockets.len(), 1);
    first.sender.close().await.unwrap();
    assert!(
        sockets().is_empty(),
        "awaited cleanup leaked the registered forwarding socket"
    );
    let second = connector
        .open(&target, ConnectionPurpose::Ordinary)
        .await
        .unwrap();
    let second_sockets = sockets();
    assert_eq!(second_sockets.difference(&first_sockets).count(), 1);
    second.sender.close().await.unwrap();
}

#[tokio::test]
async fn native_route_tunnel_cleanup_preserves_replacement_socket() {
    use std::os::unix::fs::FileTypeExt;
    let directory = Directory::new();
    let executable = directory.0.join("tunnel-fixture");
    // A test-only process boundary implements just OpenSSH's local forwarding
    // endpoint. It does not contact a host, enroll, or supply credentials.
    fs::write(
        &executable,
        r#"#!/usr/bin/python3 -I
import socket
import sys
forward = sys.argv[sys.argv.index('-L') + 1]
listener = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
listener.bind(forward.split(':', 1)[0])
listener.listen(8)
while True:
    peer, _ = listener.accept()
    while peer.recv(4096):
        pass
    peer.close()
"#,
    )
    .unwrap();
    fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
    let configuration = directory.0.join("ssh-config");
    fs::write(&configuration, b"").unwrap();
    let ssh = SshRouteConfiguration::new(
        executable,
        configuration,
        directory.0.clone(),
        vec![(OsString::from("PATH"), OsString::from("/usr/bin:/bin"))],
        TunnelEndpoints::new(directory.authority()).unwrap(),
        None,
    )
    .unwrap();
    let connector = NativeRouteConnector::new(vec![], Some(ssh)).unwrap();
    let target = target(
        RouteDescription::new(RouteParts::Ssh {
            destination: "fixture-host".into(),
            remote_socket: "/private/server".into(),
        })
        .unwrap(),
    );
    let sockets = || {
        fs::read_dir(&directory.0)
            .unwrap()
            .filter_map(|entry| {
                let entry = entry.unwrap();
                entry
                    .file_type()
                    .unwrap()
                    .is_socket()
                    .then(|| entry.file_name())
            })
            .collect::<std::collections::BTreeSet<_>>()
    };
    let first = connector
        .open(&target, ConnectionPurpose::Ordinary)
        .await
        .unwrap();
    let first_sockets = sockets();
    assert_eq!(first_sockets.len(), 1);
    let socket_path = directory.0.join(first_sockets.iter().next().unwrap());
    fs::remove_file(&socket_path).unwrap();
    let replacement = UnixListener::bind(&socket_path).unwrap();
    let identity = fs::symlink_metadata(&socket_path).unwrap().ino();
    assert_eq!(
        first.sender.close().await.unwrap_err().class(),
        ClientFailureClass::UnsafeEndpoint
    );
    assert_eq!(fs::symlink_metadata(&socket_path).unwrap().ino(), identity);
    drop(replacement);
}

struct GatedTransport {
    native: Arc<dyn MessageTransport>,
    closing: Arc<tokio::sync::Semaphore>,
    finish: Arc<tokio::sync::Semaphore>,
}
impl MessageTransport for GatedTransport {
    fn peer_facts(&self) -> spaceterm_host::transport::PeerFacts {
        self.native.peer_facts()
    }
    fn send<'a>(&'a self, bytes: &'a [u8]) -> spaceterm_host::transport::TransportFuture<'a, ()> {
        self.native.send(bytes)
    }
    fn recv(&self) -> spaceterm_host::transport::TransportFuture<'_, Option<Vec<u8>>> {
        self.native.recv()
    }
    fn close(&self) -> spaceterm_host::transport::TransportFuture<'_, ()> {
        Box::pin(async {
            self.closing.add_permits(1);
            self.finish.acquire().await.unwrap().forget();
            self.native.close().await
        })
    }
}
struct GatedRoutes {
    ssh: SshRouteConfiguration,
    closing: Arc<tokio::sync::Semaphore>,
    finish: Arc<tokio::sync::Semaphore>,
    tunnel: std::sync::Mutex<Option<(u32, spaceterm_host::ssh_tunnel::TunnelCompletion)>>,
}
impl RouteConnector for GatedRoutes {
    fn open<'a>(
        &'a self,
        _: &'a ConnectionTarget,
        _: ConnectionPurpose,
    ) -> ClientFuture<'a, ConnectedRoute> {
        Box::pin(async {
            let (transport, tunnel) = self
                .ssh
                .open(
                    "fixture-host",
                    "/private/server",
                    MessageLimit::envelope(),
                    None,
                    None,
                )
                .await?;
            *self.tunnel.lock().unwrap() =
                Some((tunnel.process_id().unwrap(), tunnel.completion()));
            let owner = Arc::new(RouteOwner {
                transport: Arc::new(GatedTransport {
                    native: transport,
                    closing: self.closing.clone(),
                    finish: self.finish.clone(),
                }),
                tunnel: Some(tunnel),
                _configuration: None,
                aborted: AtomicBool::new(false),
                cleanup: tokio::sync::OnceCell::new(),
                cleanup_scheduled: AtomicBool::new(false),
            });
            Ok(ConnectedRoute {
                sender: Arc::new(Sender(owner.clone())),
                receiver: Box::new(Receiver(owner)),
                peer: PeerFacts {
                    route: RouteKind::PrivateIpc,
                    local_account_verified: true,
                    pinned_server_key: None,
                },
            })
        })
    }
}
struct GatedDialingRoutes {
    native: NativeRouteConnector,
    cleanup: tokio::sync::Semaphore,
    finish: tokio::sync::Semaphore,
}
impl RouteConnector for GatedDialingRoutes {
    fn open<'a>(
        &'a self,
        target: &'a ConnectionTarget,
        purpose: ConnectionPurpose,
    ) -> ClientFuture<'a, ConnectedRoute> {
        Box::pin(async move {
            let result = self.native.open(target, purpose).await;
            assert!(result.is_err(), "readiness gate unexpectedly opened");
            // A frozen-port fake gates final native cleanup acknowledgement.
            // The real SSH process and its readiness deadline remain native.
            self.cleanup.add_permits(1);
            self.finish.acquire().await.unwrap().forget();
            result
        })
    }
}

#[tokio::test]
async fn native_runtime_shutdown_during_dialing_joins_expired_open_cleanup() {
    use spaceterm_client::{
        api::{ConnectingPhase, ConnectionState, PairedServerBinding},
        limits::ClientLimits,
        ports::ClientServices,
        registry::ClientRegistry,
    };
    let directory = Directory::new();
    let executable = directory.0.join("tunnel-fixture");
    fs::write(
        &executable,
        r#"#!/usr/bin/python3 -I
import os
import signal
with open('child-pid', 'w') as pid:
    pid.write(str(os.getpid()))
# Never bind the forwarding socket: this attempt remains in Dialing.
while True:
    signal.pause()
"#,
    )
    .unwrap();
    fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
    let configuration = directory.0.join("ssh-config");
    fs::write(&configuration, b"").unwrap();
    let routes = Arc::new(GatedDialingRoutes {
        native: NativeRouteConnector::new(
            vec![],
            Some(
                SshRouteConfiguration::new(
                    executable,
                    configuration,
                    directory.0.clone(),
                    vec![(OsString::from("PATH"), OsString::from("/usr/bin:/bin"))],
                    TunnelEndpoints::new(directory.authority()).unwrap(),
                    None,
                )
                .unwrap(),
            ),
        )
        .unwrap(),
        cleanup: tokio::sync::Semaphore::new(0),
        finish: tokio::sync::Semaphore::new(0),
    });
    let binding = PairedServerBinding::new(target(
        RouteDescription::new(RouteParts::Ssh {
            destination: "fixture-host".into(),
            remote_socket: "/private/server".into(),
        })
        .unwrap(),
    ));
    let ports = Arc::new(CleanupPorts(binding.clone(), AtomicBool::new(true)));
    let (registry, runtime) = ClientRegistry::recover(ClientServices {
        routes: routes.clone(),
        credentials: ports.clone(),
        journal: ports.clone(),
        organization: ports.clone(),
        delegate: ports.clone(),
        discovery: None,
        limits: ClientLimits::default(),
        additional_capabilities: vec![],
    })
    .await
    .unwrap();
    let server = registry.server(binding.target().server()).unwrap();
    server.connect().await.unwrap();
    let pid = tokio::time::timeout(HARNESS_TIMEOUT, async {
        loop {
            if let Ok(pid) = fs::read_to_string(directory.0.join("child-pid"))
                && let Ok(pid) = pid.parse::<u32>()
            {
                break pid;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    assert!(matches!(
        *server.state().borrow(),
        ConnectionState::Connecting {
            phase: ConnectingPhase::Dialing
        }
    ));
    let mut shutdown = tokio::spawn(runtime.shutdown());
    // Native open signals only after its readiness deadline expires and the child
    // is reaped. Race that event against shutdown instead of assuming cleanup is
    // scheduled within a short interval after DIAL_DEADLINE.
    tokio::time::timeout(HARNESS_TIMEOUT, async {
        tokio::select! {
            biased;
            _ = &mut shutdown => {
                panic!("shutdown dropped the pending SSH start at the outer dial deadline");
            }
            cleanup = routes.cleanup.acquire() => cleanup.unwrap().forget(),
        }
    })
    .await
    .unwrap();
    assert!(
        tokio::time::timeout(Duration::from_millis(100), &mut shutdown)
            .await
            .is_err(),
        "shutdown dropped the pending SSH start at the outer dial deadline"
    );
    assert!(!shutdown.is_finished());
    routes.finish.add_permits(1);
    tokio::time::timeout(HARNESS_TIMEOUT, shutdown)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(
        !std::process::Command::new("/bin/kill")
            .args(["-0", &pid.to_string()])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .unwrap()
            .success(),
        "owned SSH child survived shutdown"
    );
}

struct CleanupPorts(spaceterm_client::api::PairedServerBinding, AtomicBool);
impl spaceterm_client::ports::ConnectionJournal for CleanupPorts {
    fn recover_bindings(
        &self,
    ) -> ClientFuture<'_, Vec<spaceterm_client::api::PairedServerBinding>> {
        Box::pin(async { Ok(vec![self.0.clone()]) })
    }
    fn ordinary_use_allowed<'a>(
        &'a self,
        binding: &'a spaceterm_client::api::PairedServerBinding,
    ) -> ClientFuture<'a, bool> {
        Box::pin(async move { Ok(binding == &self.0 && self.1.load(Ordering::Acquire)) })
    }
}
impl spaceterm_client::ports::CredentialSource for CleanupPorts {
    fn signing_access<'a>(
        &'a self,
        _: &'a ConnectionTarget,
        _: ConnectionPurpose,
    ) -> ClientFuture<'a, spaceterm_client::ports::SigningAccess> {
        Box::pin(std::future::pending())
    }
}
impl spaceterm_client::ports::OrganizationDecoder for CleanupPorts {
    fn decode(
        &self,
        _: spaceterm_protocol::codec::Payload,
    ) -> Result<spaceterm_client::ports::OrganizationPublication, ClientFailure> {
        Err(unavailable())
    }
}
impl spaceterm_client::ports::ConnectionDelegate for CleanupPorts {
    fn invalidate(
        &self,
        _: &spaceterm_client::api::PairedServerBinding,
        _: spaceterm_client::ports::ConnectionInvalidation,
    ) {
    }
    fn reconcile<'a>(
        &'a self,
        _: spaceterm_client::api::ConnectedInfo,
        _: &'a spaceterm_client::replica::ReplicaSnapshot,
        _: &'a mut dyn spaceterm_client::ports::ProtocolChannel,
    ) -> ClientFuture<'a, ()> {
        Box::pin(async { Err(unavailable()) })
    }
    fn publication(
        &self,
        _: spaceterm_client::api::ConnectedInfo,
        _: &spaceterm_client::replica::ReplicaSnapshot,
    ) -> Result<(), ClientFailure> {
        Err(unavailable())
    }
    fn admit(
        &self,
        _: spaceterm_client::api::ConnectedInfo,
        _: &spaceterm_protocol::codec::Payload,
    ) -> Result<spaceterm_client::ports::OutboundClass, ClientFailure> {
        Err(unavailable())
    }
    fn receive<'a>(
        &'a self,
        _: spaceterm_client::api::ConnectedInfo,
        _: spaceterm_protocol::codec::Payload,
        _: &'a mut dyn spaceterm_client::ports::ProtocolChannel,
    ) -> ClientFuture<'a, ()> {
        Box::pin(async { Err(unavailable()) })
    }
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum CleanupOwner {
    Runtime,
    Removal,
    CancelledRemoval,
}

#[tokio::test]
async fn native_runtime_shutdown_waits_for_route_cleanup_and_owned_tunnel_reaping() {
    native_cleanup_remains_runtime_owned(CleanupOwner::Runtime).await;
}

#[tokio::test]
async fn native_runtime_shutdown_waits_for_concurrent_suppression_removal() {
    native_cleanup_remains_runtime_owned(CleanupOwner::Removal).await;
}

#[tokio::test]
async fn native_runtime_shutdown_waits_for_cancelled_suppression_removal() {
    native_cleanup_remains_runtime_owned(CleanupOwner::CancelledRemoval).await;
}

async fn native_cleanup_remains_runtime_owned(cleanup_owner: CleanupOwner) {
    use spaceterm_client::{
        api::{ConnectingPhase, ConnectionState, PairedServerBinding},
        limits::ClientLimits,
        ports::ClientServices,
        registry::ClientRegistry,
    };
    let directory = Directory::new();
    let executable = directory.0.join("tunnel-fixture");
    fs::write(
        &executable,
        r#"#!/usr/bin/python3 -I
import socket
import sys
listener = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
listener.bind(sys.argv[sys.argv.index('-L') + 1].split(':', 1)[0])
listener.listen(8)
while True:
    peer, _ = listener.accept()
    while peer.recv(4096):
        pass
    peer.close()
"#,
    )
    .unwrap();
    fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
    let configuration = directory.0.join("ssh-config");
    fs::write(&configuration, b"").unwrap();
    let routes = Arc::new(GatedRoutes {
        ssh: SshRouteConfiguration::new(
            executable,
            configuration,
            directory.0.clone(),
            vec![(OsString::from("PATH"), OsString::from("/usr/bin:/bin"))],
            TunnelEndpoints::new(directory.authority()).unwrap(),
            None,
        )
        .unwrap(),
        closing: Arc::new(tokio::sync::Semaphore::new(0)),
        finish: Arc::new(tokio::sync::Semaphore::new(0)),
        tunnel: std::sync::Mutex::new(None),
    });
    let binding = PairedServerBinding::new(target(
        RouteDescription::new(RouteParts::Ssh {
            destination: "fixture-host".into(),
            remote_socket: "/private/server".into(),
        })
        .unwrap(),
    ));
    let ports = Arc::new(CleanupPorts(binding.clone(), AtomicBool::new(true)));
    let (registry, runtime) = ClientRegistry::recover(ClientServices {
        routes: routes.clone(),
        credentials: ports.clone(),
        journal: ports.clone(),
        organization: ports.clone(),
        delegate: ports.clone(),
        discovery: None,
        limits: ClientLimits::default(),
        additional_capabilities: vec![],
    })
    .await
    .unwrap();
    let server = registry.server(binding.target().server()).unwrap();
    server.connect().await.unwrap();
    let mut state = server.state();
    tokio::time::timeout(HARNESS_TIMEOUT, async {
        while !matches!(
            *state.borrow_and_update(),
            ConnectionState::Connecting {
                phase: ConnectingPhase::Authenticating
            }
        ) {
            state.changed().await.unwrap();
        }
    })
    .await
    .unwrap();
    let removal = if cleanup_owner == CleanupOwner::Runtime {
        None
    } else {
        ports.1.store(false, Ordering::Release);
        let key = binding.target().local_pairing().unwrap();
        Some(tokio::spawn(async move {
            registry.remove_suppressed(&key).await
        }))
    };
    let mut shutdown = if let Some(removal) = &removal {
        tokio::time::timeout(HARNESS_TIMEOUT, routes.closing.acquire())
            .await
            .unwrap()
            .unwrap()
            .forget();
        if cleanup_owner == CleanupOwner::CancelledRemoval {
            removal.abort();
        }
        tokio::spawn(runtime.shutdown())
    } else {
        let shutdown = tokio::spawn(runtime.shutdown());
        tokio::time::timeout(HARNESS_TIMEOUT, routes.closing.acquire())
            .await
            .unwrap()
            .unwrap()
            .forget();
        shutdown
    };
    assert!(
        tokio::time::timeout(Duration::from_millis(30), &mut shutdown)
            .await
            .is_err(),
        "shutdown returned before native cleanup"
    );
    let (pid, completion) = routes.tunnel.lock().unwrap().as_ref().unwrap().clone();
    assert!(
        tokio::time::timeout(Duration::from_millis(10), completion.wait())
            .await
            .is_err()
    );
    routes.finish.add_permits(1);
    tokio::time::timeout(HARNESS_TIMEOUT, shutdown)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    tokio::time::timeout(HARNESS_TIMEOUT, completion.wait())
        .await
        .unwrap()
        .unwrap();
    if let Some(removal) = removal {
        if cleanup_owner == CleanupOwner::CancelledRemoval {
            assert!(removal.await.unwrap_err().is_cancelled());
        } else {
            assert!(removal.await.unwrap().unwrap());
        }
    }
    assert!(
        !std::process::Command::new("/bin/kill")
            .args(["-0", &pid.to_string()])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .unwrap()
            .success()
    );
}

#[tokio::test]
async fn ordinary_route_connector_cannot_supply_restricted_revocation_authority() {
    use spaceterm_core::{
        identity::OperationStreamId,
        operations::{OperationId, OperationSequence},
    };
    let directory = Directory::new();
    let endpoint = directory.authority().endpoint("restricted").unwrap();
    let _listener = UnixListener::bind(endpoint.path()).unwrap();
    let local = LocalRouteAuthority::new(endpoint).unwrap();
    let target = target(local.description().clone());
    let operation = OperationId::new(
        OperationStreamId::new(
            spaceterm_core::identity::OperationStreamKey::new(target.server(), 1).unwrap(),
            target.pairing(),
            target.client(),
        )
        .unwrap(),
        OperationSequence::new(1).unwrap(),
    );
    let connector = NativeRouteConnector::new(vec![local], None).unwrap();
    let result = connector
        .open(
            &target,
            ConnectionPurpose::RestrictedRevocation { operation },
        )
        .await;
    assert!(
        matches!(result, Err(error) if error.class() == ClientFailureClass::AuthorizationDenied)
    );
}

#[tokio::test]
async fn captured_ssh_rejects_changed_effective_host_account_and_jump() {
    let directory = Directory::new();
    let configuration = directory.0.join("effective-config");
    let original = "Host target\n HostName 127.0.0.1\n User owner\n ProxyJump jump\nHost jump\n HostName 127.0.0.2\n User gateway\n";
    fs::write(&configuration, original).unwrap();
    let ssh = SshRouteConfiguration::new(
        PathBuf::from("/usr/bin/ssh"),
        configuration.clone(),
        directory.0.clone(),
        vec![("PATH".into(), "/usr/bin:/bin".into())],
        TunnelEndpoints::new(directory.authority()).unwrap(),
        None,
    )
    .unwrap();
    let capture = ssh.capture("target").await.unwrap();
    assert!(capture.configuration().contains("hostname 127.0.0.1"));
    assert!(capture.configuration().contains("hostname 127.0.0.2"));
    assert!(capture.configuration().contains("ProxyJump st-route-1"));
    for changed in [
        original.replace("127.0.0.1", "127.0.0.3"),
        original.replace("User owner", "User other"),
        original.replace("127.0.0.2", "127.0.0.4"),
        original.replace("User gateway", "User other"),
    ] {
        fs::write(&configuration, changed).unwrap();
        let result = ssh
            .open_captured(
                "target",
                "/server",
                MessageLimit::envelope(),
                &capture,
                None,
            )
            .await;
        assert!(
            matches!(result, Err(error) if error.class() == ClientFailureClass::StaleAuthority)
        );
    }
    // OpenSSH itself must read the generated configuration, including the jump
    // host's pinned account and endpoint, without re-reading the original file.
    let snapshot = directory.0.join("snapshot");
    fs::write(&snapshot, capture.configuration()).unwrap();
    for (alias, expected) in [
        ("st-route-0", "hostname 127.0.0.1"),
        ("st-route-1", "hostname 127.0.0.2"),
    ] {
        let output = std::process::Command::new("/usr/bin/ssh")
            .args(["-G", "-F"])
            .arg(&snapshot)
            .arg(alias)
            .output()
            .unwrap();
        assert!(output.status.success());
        assert!(String::from_utf8(output.stdout).unwrap().contains(expected));
    }
}

#[tokio::test]
async fn captured_ssh_spawn_uses_snapshot_after_original_configuration_changes() {
    let directory = Directory::new();
    let executable = directory.0.join("ssh-fixture");
    let configuration = directory.0.join("ssh-config");
    let observed = directory.0.join("observed");
    fs::write(
        &configuration,
        "host original\nhostname original.example\nuser owner\nport 22\n",
    )
    .unwrap();
    let script = format!(
        r#"#!/usr/bin/python3 -I
import socket, sys
configuration = sys.argv[sys.argv.index('-F') + 1]
if '-G' in sys.argv:
    print(open(configuration).read(), end='')
    sys.exit(0)
open({original:?}, 'w').write('host original\nhostname redirected.example\nuser attacker\nport 22\n')
captured = open(configuration).read()
open({observed:?}, 'w').write(captured)
assert 'hostname original.example' in captured
assert 'redirected.example' not in captured
forward = sys.argv[sys.argv.index('-L') + 1]
listener = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
listener.bind(forward.split(':', 1)[0])
listener.listen(8)
while True:
    peer, _ = listener.accept()
    while peer.recv(4096):
        pass
    peer.close()
"#,
        original = configuration,
        observed = observed
    );
    fs::write(&executable, script).unwrap();
    fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
    let ssh = SshRouteConfiguration::new(
        executable,
        configuration.clone(),
        directory.0.clone(),
        vec![("PATH".into(), "/usr/bin:/bin".into())],
        TunnelEndpoints::new(directory.authority()).unwrap(),
        None,
    )
    .unwrap();
    let capture = ssh.capture("original").await.unwrap();
    let connector = NativeRouteConnector::new(vec![], Some(ssh)).unwrap();
    let route = RouteDescription::new(RouteParts::Ssh {
        destination: "original".into(),
        remote_socket: "/server".into(),
    })
    .unwrap();
    let connected = connector
        .open_pairing_route(target(route.clone()).pin(), &route, Some(&capture))
        .await
        .unwrap();
    assert!(
        fs::read_to_string(&configuration)
            .unwrap()
            .contains("redirected.example")
    );
    assert_eq!(
        fs::read_to_string(observed).unwrap(),
        format!("Host *\n BatchMode yes\n{}", capture.configuration())
    );
    connected
        .sender
        .send(b"bounded-restricted-frame")
        .await
        .unwrap();
    connected.sender.close().await.unwrap();
    assert!(!fs::read_dir(&directory.0).unwrap().any(|entry| {
        use std::os::unix::fs::FileTypeExt;
        entry.unwrap().file_type().unwrap().is_socket()
    }));
}

#[tokio::test]
async fn captured_ssh_keeps_the_root_destination_literal_instead_of_parsing_jump_syntax() {
    let directory = Directory::new();
    let configuration = directory.0.join("ssh-config");
    fs::write(
        &configuration,
        "Host literal:2222\n HostName 127.0.0.1\n User owner\n Port 8022\n",
    )
    .unwrap();
    let ssh = SshRouteConfiguration::new(
        "/usr/bin/ssh".into(),
        configuration,
        directory.0.clone(),
        vec![("PATH".into(), "/usr/bin:/bin".into())],
        TunnelEndpoints::new(directory.authority()).unwrap(),
        None,
    )
    .unwrap();
    let capture = ssh.capture("literal:2222").await.unwrap();
    assert!(capture.configuration().contains("hostname 127.0.0.1"));
    assert!(capture.configuration().contains("port 8022"));
}

#[tokio::test]
async fn restricted_ssh_capture_refuses_match_exec_before_running_local_commands() {
    let directory = Directory::new();
    let configuration = directory.0.join("ssh-config");
    let marker = directory.0.join("unexpected-effect");
    let included = directory.0.join("included-config");
    fs::write(&configuration, format!("Host target\n HostName 127.0.0.1\n User owner\nMatch exec \"touch {}\"\n Compression yes\n", marker.display())).unwrap();
    let ssh = SshRouteConfiguration::new(
        "/usr/bin/ssh".into(),
        configuration,
        directory.0.clone(),
        vec![("PATH".into(), "/usr/bin:/bin".into())],
        TunnelEndpoints::new(directory.authority()).unwrap(),
        None,
    )
    .unwrap();
    assert!(ssh.capture("target").await.is_err());
    assert!(
        !marker.exists(),
        "restricted validation executed a local command"
    );
    fs::copy(&ssh.managed_config, &included).unwrap();
    fs::write(
        &ssh.managed_config,
        format!("Include \"{}\"\n", included.display()),
    )
    .unwrap();
    assert!(ssh.capture("target").await.is_err());
    assert!(
        !marker.exists(),
        "included Match exec ran during restricted validation"
    );
}

#[tokio::test]
async fn restricted_ssh_capture_binds_literal_and_glob_includes_before_execution() {
    let directory = Directory::new();
    let includes = directory.0.join("config fragments");
    fs::create_dir(&includes).unwrap();
    let included = includes.join("target.conf");
    fs::write(&included, "Host target\n HostName 127.0.0.1\n User owner\n").unwrap();
    let configuration = directory.0.join("ssh-config");
    fs::write(
        &configuration,
        format!("Include = \"{}/*.conf\"\n", includes.display()),
    )
    .unwrap();
    let ssh = SshRouteConfiguration::new(
        "/usr/bin/ssh".into(),
        configuration,
        directory.0.clone(),
        vec![("PATH".into(), "/usr/bin:/bin".into())],
        TunnelEndpoints::new(directory.authority()).unwrap(),
        None,
    )
    .unwrap();
    let capture = ssh.capture("target").await.unwrap();
    assert!(capture.configuration().contains("hostname 127.0.0.1"));
    fs::write(included, "Host target\n HostName 127.0.0.2\n User other\n").unwrap();
    let result = ssh
        .open_captured(
            "target",
            "/server",
            MessageLimit::envelope(),
            &capture,
            None,
        )
        .await;
    assert!(matches!(result, Err(error) if error.class() == ClientFailureClass::StaleAuthority));
}
