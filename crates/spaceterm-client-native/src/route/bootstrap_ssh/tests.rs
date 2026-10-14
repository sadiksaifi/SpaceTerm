//! Exercises the production fixed-command and framing adapter with a local SSH process boundary.

use super::*;

fn configuration() -> Configuration {
    let directory = tempfile::tempdir().unwrap();
    let executable = directory.path().join("ssh-boundary");
    // No remote command is executed. This executable models only multiplex ownership
    // and the raw-archive/framed-channel byte boundary.
    std::fs::write(
        &executable,
        br##"#!/bin/sh
set -efu
master=no
check=no
socket=
previous=
for argument in "$@"; do
    if [ "$previous" = -S ]; then socket=$argument; fi
    case "$argument" in -M) master=yes ;; check) check=yes ;; esac
    previous=$argument
done
if [ "$master" = yes ]; then
    : > "$socket"
    exec /bin/sleep 60
fi
if [ "$check" = yes ]; then exit 0; fi
case "$*" in
    *bootstrap-account*)
        /bin/dd bs=1 count=2 of=/dev/null 2>/dev/null
        printf '\000\000\000\002{}'
        exec /bin/cat
        ;;
    *) exit 1 ;;
esac
"##,
    )
    .unwrap();
    std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
    let file = tempfile::NamedTempFile::new().unwrap();
    Configuration {
        executable,
        home: directory.path().to_owned(),
        environment: Vec::new(),
        file,
        directory,
        askpass: None,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bootstrap_ssh_retains_master_and_frames_only_fixed_account_operations() {
    let master = Arc::new(Master::start(configuration()).unwrap());
    let command = master.command();
    let arguments = command
        .get_args()
        .map(|value| value.to_str().unwrap())
        .collect::<Vec<_>>();
    assert!(arguments.contains(&"ControlMaster=no"));
    assert!(arguments.contains(&"BatchMode=yes"));
    assert!(arguments.contains(&"ProxyCommand=/usr/bin/false"));
    assert!(arguments.contains(&"ClearAllForwardings=yes"));
    assert!(arguments.contains(&"PermitLocalCommand=no"));
    assert!(arguments.contains(&"RemoteCommand=none"));
    let staged = Arc::new(StagedArtifact {
        bytes: b"AB".to_vec(),
        entry: "./bin/spaceterm-server",
        digest: [1; 32],
    });
    let channel = master.stage(staged, 1000).await.unwrap();
    assert_eq!(channel.recv().await.unwrap().unwrap(), b"{}");
    let request = serde_json::to_vec(&AccountRequest::Inspect).unwrap();
    channel.send(&request).await.unwrap();
    assert_eq!(channel.recv().await.unwrap().unwrap(), request);
    assert_eq!(
        channel.send(&vec![0; FRAME_LIMIT + 1]).await,
        Err(TransportError::MessageTooLarge)
    );
    channel.close().await.unwrap();
    assert!(master.revalidate().is_ok());
    // Reaping the retained owner must prevent a subsequent command from borrowing
    // a replacement OpenSSH connection, even if its local socket name remains.
    master.child.lock().unwrap().retire();
    let until = Instant::now() + Duration::from_secs(2);
    while master.revalidate().is_ok() && Instant::now() < until {
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    assert!(matches!(
        master.revalidate(),
        Err(BootstrapFailure {
            class: BootstrapFailureClass::Pairing(PairingFailure::RouteChanged),
            ..
        })
    ));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bootstrap_ssh_rejects_oversized_remote_frame_before_allocation() {
    let mut command = Command::new("/bin/sh");
    command
        .args(["-c", "printf '\\377\\377\\377\\377'"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .process_group(0);
    let channel = Channel::spawn(&mut command, 1000).unwrap();
    assert_eq!(channel.recv().await, Err(TransportError::Unavailable));
}

async fn wait_for_file(path: &std::path::Path) {
    tokio::time::timeout(Duration::from_secs(3), async {
        while !path.exists() {
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    })
    .await
    .expect("controlled SSH boundary did not reach the expected byte boundary");
}

fn process_exists(pid: u32) -> bool {
    // SAFETY: signal zero only observes the controlled subprocess's existence.
    unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
}

fn stalled_channel(partial: &str, ready: &std::path::Path) -> Channel {
    let mut command = Command::new("/bin/sh");
    command
        .args([
            "-c",
            &format!("printf '{partial}'; : > \"$1\"; exec /bin/sleep 60"),
            "ssh-frame-boundary",
        ])
        .arg(ready)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .process_group(0);
    Channel::spawn(&mut command, 1000).unwrap()
}

async fn stalled_frame_times_out_and_retires(partial: &str) {
    let directory = tempfile::tempdir().unwrap();
    let ready = directory.path().join("ready");
    let channel = Arc::new(stalled_channel(partial, &ready));
    let pid = channel.child.lock().unwrap().0.as_ref().unwrap().id();
    wait_for_file(&ready).await;
    let received = tokio::time::timeout(Duration::from_millis(100), channel.recv()).await;
    assert!(
        received.is_err(),
        "the stalled receive must yield to its caller's timeout"
    );
    // Keep the channel alive. Dropping the wait, not its last Arc, must retire it.
    assert!(!process_exists(pid));
    assert_eq!(
        channel.send(b"next").await,
        Err(TransportError::Unavailable)
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bootstrap_ssh_partial_prefix_stall_yields_to_timeout_and_retires_channel() {
    stalled_frame_times_out_and_retires("\\000\\000").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bootstrap_ssh_partial_payload_stall_yields_to_timeout_and_retires_channel() {
    stalled_frame_times_out_and_retires("\\000\\000\\000\\004ab").await;
}

#[tokio::test]
async fn bootstrap_ssh_frame_prefix_and_payload_share_one_absolute_deadline() {
    let directory = tempfile::tempdir().unwrap();
    let prefix_ready = directory.path().join("prefix-ready");
    let payload_ready = directory.path().join("payload-ready");
    let mut command = Command::new("/bin/sh");
    command
        .args([
            "-c",
            "printf '\\000\\000'; : > \"$1\"; /bin/dd bs=1 count=1 of=/dev/null 2>/dev/null; printf '\\000\\004ab'; : > \"$2\"; exec /bin/sleep 60",
            "ssh-frame-deadline-boundary",
        ])
        .arg(&prefix_ready)
        .arg(&payload_ready)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .process_group(0);
    let channel = Channel::spawn(&mut command, 1000).unwrap();
    wait_for_file(&prefix_ready).await;
    let deadline = tokio::time::Instant::now() + Duration::from_millis(200);
    let release_prefix = async {
        tokio::time::sleep_until(deadline - Duration::from_millis(50)).await;
        channel.write(b"x").unwrap();
    };
    let (result, ()) = tokio::join!(
        tokio::time::timeout_at(
            deadline + Duration::from_millis(100),
            channel.recv_frame(deadline)
        ),
        release_prefix,
    );
    assert!(
        payload_ready.exists(),
        "the test must reach the payload wait"
    );
    assert!(matches!(
        result,
        Ok(Err(BootstrapFailure {
            class: BootstrapFailureClass::Pairing(PairingFailure::TimedOut),
            ..
        }))
    ));
}

// This subprocess substitutes only the external OpenSSH/remote-byte boundary.
// A local operation sequence selects its roles; remote shell text is never parsed
// or executed. Capture, master ownership, frames and the native Remote stay real.
const NATIVE_SSH_BOUNDARY: &str = r#"#!/usr/bin/python3 -I
import fcntl, os, pathlib, struct, sys, time
root = pathlib.Path(os.environ['HOME'])
if '-G' in sys.argv:
    print(pathlib.Path(sys.argv[sys.argv.index('-F') + 1]).read_text(), end='')
    sys.exit(0)
if '-M' in sys.argv:
    pathlib.Path(sys.argv[sys.argv.index('-S') + 1]).touch()
    time.sleep(60)
    sys.exit(0)
if '-O' in sys.argv:
    sys.exit(0)
with (root / 'next-operation').open('a+') as counter:
    fcntl.flock(counter, fcntl.LOCK_EX)
    counter.seek(0)
    number = int(counter.read() or '0')
    counter.seek(0)
    counter.truncate()
    counter.write(str(number + 1))
role = (root / 'operations').read_text().splitlines()[number]
if role == 'probe':
    sys.stdout.buffer.write((root / 'probe').read_bytes())
    sys.exit(0)
def read_exact(count):
    value = bytearray()
    while len(value) < count:
        chunk = sys.stdin.buffer.read(count - len(value))
        if not chunk:
            sys.exit(0)
        value.extend(chunk)
    return bytes(value)
assert read_exact(2) == b'AB'
(root / (role + '-pid')).write_text(str(os.getpid()))
while True:
    count = struct.unpack('!I', read_exact(4))[0]
    assert 0 < count <= 65536
    request = read_exact(count)
    (root / (role + '-request')).write_bytes(request)
    if role == 'owner':
        sys.stdout.buffer.write((root / 'owner-partial').read_bytes())
        sys.stdout.buffer.flush()
        (root / 'owner-ready').touch()
        time.sleep(60)
        sys.exit(0)
    (root / 'account-ready').touch()
    while not (root / 'account-reply-permit').exists():
        time.sleep(0.002)
    reply = (root / 'observation').read_bytes()
    sys.stdout.buffer.write(struct.pack('!I', len(reply)) + reply)
    sys.stdout.buffer.flush()
"#;

async fn native_remote(
    operations: &[&str],
    owner_partial: &[u8],
    account_reply_permitted: bool,
) -> (tempfile::TempDir, Arc<Remote>) {
    let directory = tempfile::tempdir().unwrap();
    let executable = directory.path().join("ssh-boundary");
    std::fs::write(&executable, NATIVE_SSH_BOUNDARY).unwrap();
    std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
    let managed = directory.path().join("ssh-config");
    std::fs::write(
        &managed,
        "host fixture\nhostname fixture.example\nuser owner\nport 22\n",
    )
    .unwrap();
    std::fs::write(directory.path().join("operations"), operations.join("\n")).unwrap();
    std::fs::write(directory.path().join("owner-partial"), owner_partial).unwrap();
    std::fs::write(
        directory.path().join("probe"),
        b"STBP2\nuid=1000\nos=Darwin\narch=arm64\nversion=26.0.0\nlibc=none\nkernel=25.0.0\nmanager=launchagent\nmanager_version=1\nmanager_reachable=yes\nlinger=none\nsudo=unavailable\n",
    )
    .unwrap();
    let observation = AccountReply::Observation(AccountObservation {
        facts: spaceterm_host::bootstrap::setup::BootstrapAccountFacts {
            key: spaceterm_host::startup::AccountBuildKey {
                account_uid: 1000,
                build_identity: spaceterm_host::startup::BuildIdentity::Development,
            },
            registration: spaceterm_host::bootstrap::setup::BootstrapAccountRegistration::Missing,
            supervisor: spaceterm_host::account_service::SupervisorState::NotInstalled,
            linger: None,
        },
        remote_socket: None,
    });
    std::fs::write(
        directory.path().join("observation"),
        serde_json::to_vec(&observation).unwrap(),
    )
    .unwrap();
    if account_reply_permitted {
        std::fs::write(directory.path().join("account-reply-permit"), b"").unwrap();
    }
    let runtime = directory.path().join("runtime");
    std::fs::create_dir(&runtime).unwrap();
    std::fs::set_permissions(&runtime, std::fs::Permissions::from_mode(0o700)).unwrap();
    let ssh = SshRouteConfiguration::new(
        executable,
        managed,
        directory.path().to_owned(),
        vec![("PATH".into(), "/usr/bin:/bin".into())],
        TunnelEndpoints::new(PrivateIpcDirectory::open(&runtime).unwrap()).unwrap(),
        None,
    )
    .unwrap();
    let original = ssh.capture("fixture").await.unwrap();
    let configuration = ssh.bootstrap_configuration(&original).unwrap();
    let routes = Arc::new(NativeRouteConnector::new(vec![], Some(ssh)).unwrap());
    let master = Arc::new(Master::start(configuration).unwrap());
    let staged = Arc::new(StagedArtifact {
        bytes: b"AB".to_vec(),
        entry: "./bin/spaceterm-server",
        digest: [1; 32],
    });
    let channel = master.stage(staged.clone(), 1000).await.unwrap();
    let route = BootstrapRoute::capture(
        [1; 32],
        hash(original.configuration(), b"account"),
        hash(original.configuration(), b"configuration"),
        1000,
        BuildChannel::Development,
    )
    .unwrap();
    let remote = Arc::new(Remote {
        master,
        route,
        destination: "fixture".into(),
        staged,
        channel: Mutex::new(Some(channel)),
        account_lock: tokio::sync::Mutex::new(()),
        original,
        foreground: Mutex::new(None),
        routes,
    });
    (directory, remote)
}

async fn owner_stall_cancels_exact_channel(partial: &[u8]) {
    let (directory, remote) = native_remote(&["account", "owner"], partial, true).await;
    let valid = Arc::new(AtomicBool::new(true));
    remote.bind_foreground_fence(valid.clone()).unwrap();
    let channel = remote.owner_channel().await.unwrap();
    wait_for_file(&directory.path().join("owner-ready")).await;
    let pid = std::fs::read_to_string(directory.path().join("owner-pid"))
        .unwrap()
        .parse::<u32>()
        .unwrap();
    let cancel = async {
        tokio::time::sleep(Duration::from_millis(20)).await;
        valid.store(false, Ordering::Release);
    };
    let (received, ()) = tokio::join!(
        tokio::time::timeout(Duration::from_secs(1), channel.recv()),
        cancel,
    );
    assert_eq!(received.unwrap(), Err(TransportError::Unavailable));
    assert!(!process_exists(pid));
    // Enrollment cancellation must not retire the account command or master.
    assert!(remote.revalidate().is_ok());
    assert!(matches!(
        remote.account(AccountRequest::Inspect, None).await,
        Ok(AccountReply::Observation(_))
    ));
}

#[tokio::test]
async fn bootstrap_ssh_owner_partial_prefix_stall_cancels_exact_channel() {
    owner_stall_cancels_exact_channel(&[0, 0]).await;
}

#[tokio::test]
async fn bootstrap_ssh_owner_partial_payload_stall_cancels_exact_channel() {
    owner_stall_cancels_exact_channel(&[0, 0, 0, 4, b'a', b'b']).await;
}

#[tokio::test]
async fn bootstrap_ssh_accepted_start_keeps_account_reply_owned_after_foreground_cancellation() {
    let (directory, remote) = native_remote(&["account"], &[], false).await;
    let valid = Arc::new(AtomicBool::new(true));
    remote.bind_foreground_fence(valid.clone()).unwrap();
    let request = AccountRequest::Start {
        expected: spaceterm_host::bootstrap::setup::BootstrapAccountRegistration::Stopped {
            pin: spaceterm_protocol::authentication::ServerPin {
                identity: [1; 16],
                public_key: [2; 32],
            },
            generation: 1,
        },
    };
    let cancel_after_acceptance = async {
        wait_for_file(&directory.path().join("account-ready")).await;
        let accepted = std::fs::read(directory.path().join("account-request")).unwrap();
        assert!(matches!(
            serde_json::from_slice::<AccountRequest>(&accepted).unwrap(),
            AccountRequest::Start { .. }
        ));
        valid.store(false, Ordering::Release);
        std::fs::write(directory.path().join("account-reply-permit"), b"").unwrap();
    };
    let (reply, ()) = tokio::join!(remote.account(request, None), cancel_after_acceptance);
    assert!(matches!(reply, Ok(AccountReply::Observation(_))));
    assert!(remote.revalidate().is_ok());
}

struct OneNativeHost(Mutex<Option<Arc<Remote>>>);
impl BootstrapHostFactory for OneNativeHost {
    fn open(
        &self,
        destination: String,
        generation: u64,
    ) -> PairingFuture<'_, Result<BootstrapHostConnection, BootstrapFailure>> {
        Box::pin(async move {
            assert_eq!(destination, "fixture");
            assert_ne!(generation, 0);
            let connection = self
                .0
                .lock()
                .unwrap()
                .take()
                .expect("only one SSH owner is opened");
            Ok(BootstrapHostConnection {
                connection,
                artifact: None,
            })
        })
    }
}

struct NoInspectionCredentials;
impl crate::credentials::adapter::NativeKeyStore for NoInspectionCredentials {
    fn read<'a>(
        &'a self,
        _: &'a spaceterm_client::pairing::credentials::CredentialScope,
        _: spaceterm_client::pairing::credentials::NativeCredentialAccess,
    ) -> PairingFuture<'a, Result<Option<Zeroizing<Vec<u8>>>, PairingFailure>> {
        Box::pin(async { panic!("read-only inspection must not read native credentials") })
    }
    fn insert<'a>(
        &'a self,
        _: &'a spaceterm_client::pairing::credentials::CredentialScope,
        _: &'a [u8],
    ) -> PairingFuture<'a, Result<(), PairingFailure>> {
        Box::pin(async { panic!("read-only inspection must not create native credentials") })
    }
    fn remove<'a>(
        &'a self,
        _: &'a spaceterm_client::pairing::credentials::CredentialScope,
        _: &'a [u8],
    ) -> PairingFuture<'a, Result<bool, PairingFailure>> {
        Box::pin(async { panic!("read-only inspection must not remove native credentials") })
    }
}

struct InspectionVerifier;
impl spaceterm_client::journal::ReceiptVerifier for InspectionVerifier {
    fn verify(
        &self,
        pin: spaceterm_protocol::authentication::ServerPin,
        receipt: &spaceterm_core::pairing::SignedPairingReceipt,
    ) -> Result<(), ClientFailure> {
        spaceterm_protocol::pairing_adapter::ReceiptCodec::new(pin)
            .verify(receipt)
            .map(|_| ())
            .map_err(|_| ClientFailure::new(ClientFailureClass::InvalidPeer))
    }
}

struct NoInspectionInvalidation;
impl spaceterm_client::pending::LocalUseInvalidator for NoInspectionInvalidation {
    fn invalidate_workspace(
        &self,
        _: &spaceterm_core::pairing::LocalPairingKey,
        _: spaceterm_core::identity::WorkspaceId,
    ) -> Result<Vec<spaceterm_core::authority::GrantedControl>, ClientFailure> {
        panic!("read-only inspection must not invalidate a workspace")
    }
    fn invalidate_attachment(
        &self,
        _: &spaceterm_core::pairing::LocalPairingKey,
        _: spaceterm_client::view::WorkspaceAttachment,
    ) -> Result<Vec<spaceterm_core::authority::GrantedControl>, ClientFailure> {
        panic!("read-only inspection must not invalidate an attachment")
    }
    fn invalidate_pairing(&self, _: &spaceterm_core::pairing::LocalPairingKey) {
        panic!("read-only inspection must not invalidate a pairing")
    }
    fn remove_pairing<'a>(
        &'a self,
        _: &'a spaceterm_core::pairing::LocalPairingKey,
    ) -> spaceterm_client::ports::ClientFuture<'a, ()> {
        Box::pin(async { panic!("read-only inspection must not remove a pairing") })
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bootstrap_ssh_foreground_inspection_cancellation_retires_master_without_provider_call() {
    use crate::{
        bootstrap::production::ProductionSshBootstrap, credentials::adapter::DeviceCredentials,
        journal::NativeClientJournal, pairing_runtime::NativePairingRuntime,
        restricted_routes::NativePairingConnector,
    };
    use spaceterm_client::journal::{JournalController, JournalLimits};
    use spaceterm_core::operation_ledger::canonical::CanonicalCodec;
    use spaceterm_protocol::authentication::HandshakeConfig;

    let (directory, remote) = native_remote(&["account", "probe"], &[], true).await;
    let master = Arc::downgrade(&remote.master);
    let master_pid = remote.master.child.lock().unwrap().0.as_ref().unwrap().id();
    let routes = remote.routes.clone();
    // Linux native durability checks intentionally reject tmpfs journals.
    let journal_directory = if cfg!(target_os = "linux") {
        tempfile::tempdir_in("/var/tmp").unwrap()
    } else {
        tempfile::tempdir().unwrap()
    };
    std::fs::set_permissions(
        journal_directory.path(),
        std::fs::Permissions::from_mode(0o700),
    )
    .unwrap();
    let codec = Arc::new(CanonicalCodec);
    let verifier = Arc::new(InspectionVerifier);
    let storage = NativeClientJournal::create(
        journal_directory.path().canonicalize().unwrap(),
        codec.clone(),
        verifier.clone(),
        JournalLimits::default(),
    )
    .await
    .unwrap();
    let journal = Arc::new(
        JournalController::recover(Arc::new(storage), codec, verifier, JournalLimits::default())
            .await
            .unwrap(),
    );
    let connector = Arc::new(NativePairingConnector::with_shared_routes(
        journal.clone(),
        routes,
    ));
    let runtime = Arc::new(NativePairingRuntime::with_adapters(
        journal,
        Arc::new(DeviceCredentials::with_store(Arc::new(
            NoInspectionCredentials,
        ))),
        connector.clone(),
        connector,
        Arc::new(NoInspectionInvalidation),
        HandshakeConfig::default(),
    ));
    // The factory relinquishes its Arc on open. Only the real provider retains
    // this native Remote once read-only foreground inspection completes.
    let provider =
        ProductionSshBootstrap::new(runtime, Arc::new(OneNativeHost(Mutex::new(Some(remote)))));
    let valid = Arc::new(AtomicBool::new(true));
    let (facts, pending) = provider
        .inspect_with_pending_foreground("fixture".into(), 1, valid.clone())
        .await
        .unwrap();
    assert!(matches!(facts.registration, BootstrapRegistration::Missing));
    assert!(pending.is_none());
    assert!(master.upgrade().is_some());
    assert!(process_exists(master_pid));
    let account_pid = std::fs::read_to_string(directory.path().join("account-pid"))
        .unwrap()
        .parse::<u32>()
        .unwrap();
    valid.store(false, Ordering::Release);
    // No subsequent provider lookup, bind, abandon or shutdown drives cleanup.
    tokio::time::timeout(Duration::from_secs(3), async {
        while master.upgrade().is_some()
            || process_exists(master_pid)
            || process_exists(account_pid)
        {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("canceling a successful inspection must retire its exact SSH owner and helper");
    drop(provider);
}

#[tokio::test]
async fn bootstrap_ssh_revalidation_rejects_changed_capture_and_retains_original_snapshot() {
    let (directory, remote) = native_remote(&["account"], &[], true).await;
    let original = remote.captured_ssh().unwrap();
    remote.revalidate_capture().await.unwrap();
    std::fs::write(
        directory.path().join("ssh-config"),
        "host fixture\nhostname redirected.example\nuser owner\nport 22\n",
    )
    .unwrap();
    assert!(matches!(
        remote.revalidate_capture().await,
        Err(BootstrapFailure {
            class: BootstrapFailureClass::Pairing(PairingFailure::RouteChanged),
            ..
        })
    ));
    assert_eq!(remote.captured_ssh().unwrap(), original);
    assert!(remote.revalidate().is_ok());
}
