use crate::{ServerError, protocol, storage::Directory};
use spaceterm_directories::ServerDirectories;
use std::{
    ffi::CStr,
    fs,
    os::{
        fd::AsRawFd,
        unix::{
            ffi::OsStrExt,
            fs::PermissionsExt,
            net::{UnixListener, UnixStream},
        },
    },
    path::{Path, PathBuf},
};

static STOP: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
type PaneHost = std::sync::Arc<std::sync::Mutex<Option<Box<dyn crate::pane::PaneService>>>>;
#[derive(Clone)]
pub(crate) struct Environment {
    pub(crate) panes: PaneHost,
    pub(crate) direct: Option<crate::direct::host::Shared>,
    pub(crate) paths: Paths,
    pub(crate) stopping: std::sync::Arc<std::sync::atomic::AtomicBool>,
}
extern "C" fn stop_signal(_: libc::c_int) {
    STOP.store(true, std::sync::atomic::Ordering::Release);
}
pub(crate) fn install_shutdown_signals() -> Result<(), ServerError> {
    STOP.store(false, std::sync::atomic::Ordering::Release);
    // SAFETY: the handler only stores an atomic flag; libc retains the function pointer.
    unsafe {
        if libc::signal(
            libc::SIGTERM,
            stop_signal as *const () as libc::sighandler_t,
        ) == libc::SIG_ERR
            || libc::signal(libc::SIGINT, stop_signal as *const () as libc::sighandler_t)
                == libc::SIG_ERR
        {
            return Err(ServerError::StartupFailed);
        }
        // Desktop background threads can block these signals. Installing a handler does not
        // change the mask inherited through exec, so the server must unblock its shutdown signals.
        let mut shutdown: libc::sigset_t = std::mem::zeroed();
        libc::sigemptyset(&mut shutdown);
        libc::sigaddset(&mut shutdown, libc::SIGTERM);
        libc::sigaddset(&mut shutdown, libc::SIGINT);
        if libc::pthread_sigmask(libc::SIG_UNBLOCK, &shutdown, std::ptr::null_mut()) != 0 {
            return Err(ServerError::StartupFailed);
        }
    }
    Ok(())
}

#[derive(Clone)]
pub(crate) struct Paths {
    pub identity: String,
    pub data: PathBuf,
    pub runtime: PathBuf,
}
impl Paths {
    pub fn for_identity(identity: &str) -> Result<Self, ServerError> {
        // SAFETY: geteuid has no preconditions.
        if unsafe { libc::geteuid() } == 0 {
            return Err(ServerError::UnsafeStorage);
        }
        // Use the owning account, never HOME/XDG/TMPDIR from a client or service environment.
        let mut record: libc::passwd = unsafe { std::mem::zeroed() };
        let mut result = std::ptr::null_mut();
        let mut buffer = vec![0; 65536];
        // SAFETY: all pointers refer to appropriately sized writable storage retained through use.
        let code = unsafe {
            libc::getpwuid_r(
                libc::geteuid(),
                &mut record,
                buffer.as_mut_ptr().cast(),
                buffer.len(),
                &mut result,
            )
        };
        if code != 0 || result.is_null() || record.pw_dir.is_null() {
            return Err(ServerError::StorageUnavailable);
        }
        // SAFETY: successful getpwuid_r stores a terminated string within buffer, retained here.
        let home = unsafe { CStr::from_ptr(record.pw_dir) };
        let home = PathBuf::from(std::ffi::OsStr::from_bytes(home.to_bytes()));
        // SAFETY: geteuid has no preconditions.
        let directories =
            ServerDirectories::for_account(&home, unsafe { libc::geteuid() }, identity)
                .map_err(|_| ServerError::StorageUnavailable)?;
        Ok(Self {
            identity: identity.to_owned(),
            data: directories.data,
            runtime: directories.runtime,
        })
    }
    pub fn socket(&self) -> PathBuf {
        self.runtime.join("s")
    }
}

pub(crate) fn check_peer(stream: &UnixStream) -> Result<(), ServerError> {
    #[cfg(target_os = "macos")]
    let user = {
        let mut user = 0;
        let mut group = 0;
        // SAFETY: getpeereid writes two uid/gid outputs for the retained socket.
        if unsafe { libc::getpeereid(stream.as_raw_fd(), &mut user, &mut group) } != 0 {
            return Err(ServerError::AuthenticationRefused);
        }
        user
    };
    #[cfg(target_os = "linux")]
    let user = {
        let mut credentials: libc::ucred = unsafe { std::mem::zeroed() };
        let mut length = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
        // SAFETY: output pointer and length describe writable ucred; the socket is retained.
        if unsafe {
            libc::getsockopt(
                stream.as_raw_fd(),
                libc::SOL_SOCKET,
                libc::SO_PEERCRED,
                (&raw mut credentials).cast(),
                &mut length,
            )
        } != 0
            || length as usize != std::mem::size_of::<libc::ucred>()
        {
            return Err(ServerError::AuthenticationRefused);
        }
        credentials.uid
    };
    check_account(user)
}
fn check_account(user: libc::uid_t) -> Result<(), ServerError> {
    // SAFETY: geteuid has no preconditions.
    if user != unsafe { libc::geteuid() } {
        return Err(ServerError::AuthenticationRefused);
    }
    Ok(())
}

pub(crate) fn connect(paths: &Paths) -> Result<UnixStream, ServerError> {
    let directory = Directory::open(&paths.runtime, false).map_err(|error| {
        if error == ServerError::NotSetUp {
            ServerError::NotRunning
        } else {
            error
        }
    })?;
    directory.verify_path(&paths.runtime)?;
    let endpoint = directory
        .socket_identity(c"s")?
        .ok_or(ServerError::NotRunning)?;
    let stream = connect_socket(&paths.socket()).map_err(|_| ServerError::NotRunning)?;
    directory.verify_path(&paths.runtime)?;
    if directory.socket_identity(c"s")? != Some(endpoint) {
        return Err(ServerError::UnsafeStorage);
    }
    check_peer(&stream)?;
    Ok(stream)
}

#[derive(Clone, Copy)]
pub(crate) enum SocketUnavailable {
    Refused,
    Unavailable,
}

pub(crate) fn connect_socket(endpoint: &Path) -> Result<UnixStream, SocketUnavailable> {
    let socket = socket2::Socket::new(socket2::Domain::UNIX, socket2::Type::STREAM, None)
        .map_err(|_| SocketUnavailable::Unavailable)?;
    let address = socket2::SockAddr::unix(endpoint).map_err(|_| SocketUnavailable::Unavailable)?;
    socket
        .connect_timeout(&address, std::time::Duration::from_secs(2))
        .map_err(|error| {
            if error.kind() == std::io::ErrorKind::ConnectionRefused {
                SocketUnavailable::Refused
            } else {
                SocketUnavailable::Unavailable
            }
        })?;
    let descriptor: std::os::fd::OwnedFd = socket.into();
    Ok(UnixStream::from(descriptor))
}

pub(crate) fn serve(paths: &Paths) -> Result<(), ServerError> {
    serve_with_panes(paths, None)
}

pub(crate) fn serve_with_panes(
    paths: &Paths,
    panes: Option<Box<dyn crate::pane::PaneService>>,
) -> Result<(), ServerError> {
    serve_runtime(paths, panes, false)
}

pub(crate) fn serve_runtime(
    paths: &Paths,
    panes: Option<Box<dyn crate::pane::PaneService>>,
    supervised: bool,
) -> Result<(), ServerError> {
    serve_runtime_ready(paths, panes, supervised, || Ok(()))
}

/// The owner receives readiness only after this process owns and publishes the endpoint.
fn serve_runtime_ready(
    paths: &Paths,
    mut panes: Option<Box<dyn crate::pane::PaneService>>,
    supervised: bool,
    ready: impl FnOnce() -> Result<(), ServerError>,
) -> Result<(), ServerError> {
    crate::service::check_runtime(paths, supervised)?;
    crate::measurement::initialize();
    let data = Directory::open(&paths.data, false)?;
    // Verify retained identity before changing any runtime state. Never generate a replacement.
    data.read()?;
    let runtime = Directory::open(&paths.runtime, true)?;
    let _lock = runtime.lock("server.lock", true)?;
    crate::service::check_runtime(paths, supervised)?;
    if let Some(panes) = panes.as_mut() {
        panes.restore(crate::organization::OrganizationStore(Directory::open(
            &paths.data,
            false,
        )?))?;
    }
    let socket = paths.socket();
    if let Some(identity) = runtime.socket_identity(c"s")? {
        runtime.verify_path(&paths.runtime)?;
        // Only a definite refusal proves staleness. Answering listeners and uncertain failures
        // remain untouched, even when their instance lock is missing or no longer held.
        if !matches!(connect_socket(&socket), Err(SocketUnavailable::Refused)) {
            return Err(ServerError::StorageUnavailable);
        }
        runtime.verify_path(&paths.runtime)?;
        runtime.remove_socket(c"s", identity)?;
    }
    // Keep the ready endpoint absent until its exact private mode has been established.
    // The staging leaf has the same length as s, preserving Unix socket path limits.
    let pending = paths.runtime.join("p");
    if let Some(identity) = runtime.socket_identity(c"p")? {
        runtime.verify_path(&paths.runtime)?;
        if !matches!(connect_socket(&pending), Err(SocketUnavailable::Refused)) {
            return Err(ServerError::StorageUnavailable);
        }
        runtime.verify_path(&paths.runtime)?;
        runtime.remove_socket(c"p", identity)?;
    }
    let listener = UnixListener::bind(&pending).map_err(|_| ServerError::StorageUnavailable)?;
    fs::set_permissions(&pending, fs::Permissions::from_mode(0o600))
        .map_err(|_| ServerError::StorageUnavailable)?;
    let socket_identity = runtime
        .socket_identity(c"p")?
        .ok_or(ServerError::UnsafeStorage)?;
    runtime.verify_path(&paths.runtime)?;
    if supervised {
        crate::service::activation::ready(paths)?;
    }
    runtime.publish_socket(c"p", socket_identity)?;
    let mut connections: Vec<(std::thread::JoinHandle<()>, UnixStream)> = Vec::new();
    use std::sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    };
    let panes = Arc::new(Mutex::new(panes));
    let workers = Arc::new(AtomicUsize::new(0));
    listener
        .set_nonblocking(true)
        .map_err(|_| ServerError::Protocol)?;
    let stopping = Arc::new(std::sync::atomic::AtomicBool::new(false));
    // A failed direct-network store never prevents owning-account admission.
    let direct = crate::direct::host::State::open(paths).ok();
    let environment = Environment {
        panes: Arc::clone(&panes),
        direct: direct.clone(),
        paths: paths.clone(),
        stopping: Arc::clone(&stopping),
    };
    let mut network_connections: Vec<(std::thread::JoinHandle<()>, std::net::TcpStream)> =
        Vec::new();
    let network_workers = Arc::new(AtomicUsize::new(0));
    let readiness = ready();
    if readiness.is_err() {
        STOP.store(true, Ordering::Release);
    }
    // The fixed admission limit bounds worker allocation; each frame has an absolute deadline.
    for connection in listener.incoming() {
        if stopping.load(Ordering::Acquire) || STOP.load(Ordering::Acquire) {
            break;
        }
        if network_workers.load(Ordering::Acquire) < 32
            && let Some(state) = &direct
        {
            let accepted = state.lock().unwrap_or_else(|e| e.into_inner()).accept();
            if let Some((tcp, generation)) = accepted {
                let cancel = tcp.try_clone().map_err(|_| ServerError::Protocol)?;
                let state = Arc::clone(state);
                let environment = environment.clone();
                let workers = Arc::clone(&network_workers);
                workers.fetch_add(1, Ordering::AcqRel);
                network_connections.retain(|(worker, _)| !worker.is_finished());
                network_connections.push((
                    std::thread::spawn(move || {
                        let _ = crate::direct::host::serve(tcp, generation, state, environment);
                        workers.fetch_sub(1, Ordering::AcqRel);
                    }),
                    cancel,
                ));
            }
        }
        let mut stream = match connection {
            Ok(stream) => stream,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                if let Some(panes) = panes.lock().unwrap_or_else(|e| e.into_inner()).as_mut() {
                    if let Some(state) = &direct
                        && let Ok(retired) = state
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .pending_releases()
                    {
                        for client in retired {
                            panes.revoked(client);
                        }
                    }
                    panes.tick();
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
                continue;
            }
            Err(_) => return Err(ServerError::Protocol),
        };
        if workers.load(Ordering::Acquire) >= 64 {
            continue;
        }
        if check_peer(&stream).is_err() {
            continue;
        }
        let Ok(record) = data.read() else { continue };
        let workers = Arc::clone(&workers);
        let service_paths = paths.clone();
        let environment = environment.clone();
        workers.fetch_add(1, Ordering::AcqRel);
        connections.retain(|(worker, _)| !worker.is_finished());
        let cancellation = stream.try_clone().map_err(|_| ServerError::Protocol)?;
        let handle = std::thread::spawn(move || {
            let _ = (|| -> Result<(), ServerError> {
                let (_, device, name) =
                    protocol::admit_server(&mut stream, &record.server, &service_paths.identity)?;
                serve_admitted(&mut stream, device, name, None, &environment)
            })();
            workers.fetch_sub(1, Ordering::AcqRel);
        });
        connections.push((handle, cancellation));
    }
    stopping.store(true, Ordering::Release);
    drop(listener);
    if let Some(state) = &direct {
        state.lock().unwrap_or_else(|e| e.into_inner()).stop();
    }
    for (_, tcp) in &network_connections {
        let _ = tcp.shutdown(std::net::Shutdown::Both);
    }
    for (worker, _) in network_connections {
        let _ = worker.join();
    }
    finish_connections(connections);
    if let Some(panes) = panes.lock().unwrap_or_else(|e| e.into_inner()).as_mut() {
        panes.shutdown()?;
    }
    runtime.verify_path(&paths.runtime)?;
    runtime.remove_socket(c"s", socket_identity)?;
    readiness
}

/// A source supervisor owns this foreground run through a pipe, including supervisor death.
/// Retained organization is loaded and shut down through the ordinary host implementation.
pub(crate) fn serve_owned(
    paths: &Paths,
    panes: Box<dyn crate::pane::PaneService>,
    mut owner: impl std::io::Read + Send + 'static,
    ready: impl FnOnce() -> Result<(), ServerError>,
) -> Result<(), ServerError> {
    install_shutdown_signals()?;
    crate::service::check_runtime(paths, false)?;
    let data = Directory::open(&paths.data, true)?;
    let setup = data.lock("setup.lock", false)?;
    data.read_or_create()?;
    drop(setup);
    std::thread::spawn(move || {
        let _ = owner.read(&mut [0]);
        STOP.store(true, std::sync::atomic::Ordering::Release);
    });
    serve_runtime_ready(paths, Some(panes), false, ready)
}

pub(crate) fn serve_admitted(
    stream: &mut dyn crate::transport::Transport,
    client: [u8; 32],
    name: crate::pane::control::ClientName,
    paired: Option<crate::direct::host::Peer<'_>>,
    environment: &Environment,
) -> Result<(), ServerError> {
    let connection = match paired {
        Some(peer) => peer.connection,
        None => protocol::random()?,
    };
    {
        let mut panes = environment.panes.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(paired) = paired {
            environment
                .direct
                .as_ref()
                .ok_or(ServerError::AuthenticationRefused)?
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .authorized_peer(&paired)?;
        }
        if let Some(panes) = panes.as_mut() {
            panes.admitted(client, name);
        }
    }
    let measurement_connection = crate::measurement::scope();
    let mut measurement_sequence = 0;
    let result = (|| {
        loop {
            if environment
                .stopping
                .load(std::sync::atomic::Ordering::Acquire)
                || STOP.load(std::sync::atomic::Ordering::Acquire)
            {
                break;
            }
            let mut wire = protocol::Wire::terminal(stream)?;
            let request: crate::pane::Request = wire.read()?;
            let operation = crate::measurement::Operation::of(&request);
            if measurement_connection != 0 {
                measurement_sequence += 1;
            }
            let measured = |event| crate::measurement::record(event);
            measured(crate::measurement::Event::ServerRequestRead {
                connection: measurement_connection,
                sequence: measurement_sequence,
                operation,
            });
            let shutdown = matches!(request, crate::pane::Request::Shutdown);
            let (reply, stopped) = {
                let mut panes = environment.panes.lock().unwrap_or_else(|e| e.into_inner());
                measured(crate::measurement::Event::ServerAdmitted {
                    connection: measurement_connection,
                    sequence: measurement_sequence,
                    operation,
                });
                if environment
                    .stopping
                    .load(std::sync::atomic::Ordering::Acquire)
                    || STOP.load(std::sync::atomic::Ordering::Acquire)
                {
                    return Err(ServerError::NotRunning);
                }
                if let Some(paired) = paired {
                    environment
                        .direct
                        .as_ref()
                        .ok_or(ServerError::AuthenticationRefused)?
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .authorized_peer(&paired)?;
                }
                let account_administration = shutdown
                    || matches!(
                        &request,
                        crate::pane::Request::Mutate {
                            mutation: crate::hierarchy::Mutation::EndAllSessions { .. },
                            ..
                        }
                    );
                let mut reply = if account_administration && paired.is_some() {
                    // A paired device cannot administer the owning account or end all its Sessions.
                    crate::pane::Reply::Refused(crate::pane::Refusal::Unsupported)
                } else if let crate::pane::Request::Direct(request) = request {
                    let reply = match &environment.direct {
                        Some(state) => state
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .manage(paired.as_ref(), request),
                        None => Err(crate::direct::DirectError::StorageUnavailable),
                    };
                    crate::pane::Reply::Direct(reply.map(|(reply, retired)| {
                        if let (Some(client), Some(panes)) = (retired, panes.as_mut()) {
                            panes.revoked(client);
                        }
                        reply
                    }))
                } else {
                    if shutdown {
                        crate::service::retain_stop(&environment.paths)?;
                    }
                    match panes.as_mut() {
                        Some(panes) => panes.request(client, connection, request),
                        None if matches!(request, crate::pane::Request::Capabilities) => {
                            crate::pane::Reply::Capabilities {
                                development_pane: false,
                                pane_takeover: false,
                                interactive_input: false,
                                key_input_batches: false,
                                native_effects: false,
                                reading: false,
                                graphics: false,
                                accessibility: false,
                                directory_recovery: true,
                                directory_creation: false,
                                directory_matching: false,
                                worktree_writes: false,
                                end_all_sessions: false,
                                workspace_policy: false,
                                presentation_changes: false,
                            }
                        }
                        None => crate::pane::Reply::Refused(crate::pane::Refusal::Unsupported),
                    }
                };
                if paired.is_some()
                    && let crate::pane::Reply::Capabilities {
                        end_all_sessions, ..
                    } = &mut reply
                {
                    *end_all_sessions = false;
                }
                let stopped = shutdown && matches!(reply, crate::pane::Reply::Closed);
                if stopped {
                    environment
                        .stopping
                        .store(true, std::sync::atomic::Ordering::Release);
                }
                (reply, stopped)
            };
            measured(crate::measurement::Event::ServerCompleted {
                connection: measurement_connection,
                sequence: measurement_sequence,
                operation,
            });
            wire.write(&reply)?;
            measured(crate::measurement::Event::ServerReplyWritten {
                connection: measurement_connection,
                sequence: measurement_sequence,
                operation,
            });
            if stopped {
                break;
            }
        }
        Ok(())
    })();
    if let Some(panes) = environment
        .panes
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_mut()
    {
        panes.disconnected(connection);
    }
    result
}

fn finish_connections(connections: Vec<(std::thread::JoinHandle<()>, UnixStream)>) {
    for (_, stream) in &connections {
        // Wake idle readers without cancelling an admitted reply, including Closed. Writes
        // retain the protocol's absolute frame deadline while workers finish before cleanup.
        let _ = stream.shutdown(std::net::Shutdown::Read);
    }
    for (worker, _) in connections {
        let _ = worker.join();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn shutdown_unblocks_idle_readers_and_finishes_pending_replies() {
        use std::io::Read;

        let (idle_client, mut idle_server) = UnixStream::pair().unwrap();
        let idle_cancellation = idle_server.try_clone().unwrap();
        let idle = std::thread::spawn(move || {
            let mut byte = [0];
            assert_eq!(idle_server.read(&mut byte).unwrap(), 0);
        });
        let (mut reply_client, mut reply_server) = UnixStream::pair().unwrap();
        let reply_cancellation = reply_server.try_clone().unwrap();
        let reply = std::thread::spawn(move || {
            // The socket read orders the pending reply after cancellation without a timer.
            let mut byte = [0];
            assert_eq!(reply_server.read(&mut byte).unwrap(), 0);
            protocol::Wire::terminal(&mut reply_server)
                .unwrap()
                .write(&crate::pane::Reply::Closed)
                .unwrap();
        });

        finish_connections(vec![(idle, idle_cancellation), (reply, reply_cancellation)]);

        let received: crate::pane::Reply = protocol::Wire::terminal(&mut reply_client)
            .unwrap()
            .read()
            .unwrap();
        assert!(matches!(received, crate::pane::Reply::Closed));
        drop(idle_client);
    }

    #[test]
    fn refuses_another_account_before_protocol_processing() {
        // SAFETY: geteuid has no preconditions.
        assert_eq!(
            check_account(unsafe { libc::geteuid() }.wrapping_add(1)),
            Err(ServerError::AuthenticationRefused)
        );
    }
    #[test]
    fn validates_real_private_ipc_peers() {
        let (client, server) = UnixStream::pair().unwrap();
        check_peer(&client).unwrap();
        check_peer(&server).unwrap();
    }
    #[test]
    #[ignore = "explicit real-host root-owned peer observation; sends no request bytes"]
    fn refuses_a_real_other_account_peer_without_sending_protocol_bytes() {
        #[cfg(target_os = "macos")]
        let endpoint = "/var/run/usbmuxd";
        #[cfg(target_os = "linux")]
        let endpoint = "/run/dbus/system_bus_socket";
        let stream = UnixStream::connect(endpoint).expect("root-owned host listener required");
        assert_eq!(check_peer(&stream), Err(ServerError::AuthenticationRefused));
    }
}

#[cfg(test)]
mod startup_tests {
    use super::*;
    use std::os::unix::fs::MetadataExt;
    use std::{
        process::{Child, Command, Stdio},
        time::{Duration, Instant},
    };

    const CHILD_TEST: &str = "host::startup_tests::server_start_child";
    const CHILD_ROOT: &str = "SPACETERM_SERVER_START_FIXTURE";

    struct Fixture {
        root: PathBuf,
        paths: Paths,
        device: [u8; 32],
        pin: [u8; 32],
    }
    impl Fixture {
        fn new() -> Self {
            let nonce = u64::from_be_bytes(protocol::random().unwrap()[..8].try_into().unwrap());
            let root = std::env::temp_dir()
                .canonicalize()
                .unwrap()
                .join(format!("st530-{nonce:x}"));
            let paths = Paths {
                identity: "spaceterm-development".into(),
                data: root.join("d"),
                runtime: root.join("r"),
            };
            let data = Directory::open(&paths.data, true).unwrap();
            let _lock = data.lock("setup.lock", false).unwrap();
            let device = protocol::random().unwrap();
            let record = data.read_or_create().unwrap();
            let pin = record.server;
            Directory::open(&paths.runtime, true).unwrap();
            Self {
                root,
                paths,
                device,
                pin,
            }
        }
        fn start_test(&self, test: &str, block_shutdown: bool) -> ChildGuard {
            use std::os::unix::process::CommandExt;
            let mut command = Command::new(std::env::current_exe().unwrap());
            command
                .args(["--exact", test, "--ignored"])
                .env(CHILD_ROOT, &self.root)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null());
            // SAFETY: this child closure uses only async-signal-safe signal-mask operations.
            unsafe {
                command.pre_exec(move || {
                    if block_shutdown {
                        let mut blocked: libc::sigset_t = std::mem::zeroed();
                        libc::sigemptyset(&mut blocked);
                        libc::sigaddset(&mut blocked, libc::SIGTERM);
                        if libc::sigprocmask(libc::SIG_BLOCK, &blocked, std::ptr::null_mut()) != 0 {
                            return Err(std::io::Error::last_os_error());
                        }
                    }
                    Ok(())
                });
            }
            ChildGuard(command.spawn().unwrap())
        }
        fn start(&self) -> ChildGuard {
            self.start_test(CHILD_TEST, false)
        }
    }
    #[test]
    fn client_refuses_a_symlink_endpoint_without_contacting_its_listener() {
        let fixture = Fixture::new();
        let target = fixture.paths.runtime.join("other");
        let listener = UnixListener::bind(&target).unwrap();
        listener.set_nonblocking(true).unwrap();
        std::os::unix::fs::symlink(&target, fixture.paths.socket()).unwrap();
        assert!(matches!(
            connect(&fixture.paths),
            Err(ServerError::UnsafeStorage)
        ));
        assert!(
            matches!(listener.accept(), Err(error) if error.kind() == std::io::ErrorKind::WouldBlock)
        );
        assert!(fixture.paths.socket().is_symlink());
    }
    #[test]
    fn prepared_socket_is_only_ready_after_atomic_publication() {
        let fixture = Fixture::new();
        let runtime = Directory::open(&fixture.paths.runtime, false).unwrap();
        let pending = fixture.paths.runtime.join("p");
        let listener = UnixListener::bind(&pending).unwrap();
        fs::set_permissions(&pending, fs::Permissions::from_mode(0o700)).unwrap();
        assert!(matches!(
            connect(&fixture.paths),
            Err(ServerError::NotRunning)
        ));
        fs::set_permissions(&pending, fs::Permissions::from_mode(0o600)).unwrap();
        let expected = runtime.socket_identity(c"p").unwrap().unwrap();
        runtime.publish_socket(c"p", expected).unwrap();
        let _client = connect(&fixture.paths).unwrap();
        let (_peer, _) = listener.accept().unwrap();
        assert!(runtime.socket_identity(c"s").unwrap() == Some(expected));
        assert!(!pending.exists());
    }

    #[test]
    fn client_refuses_an_unsafe_socket_mode_without_contacting_its_listener() {
        let fixture = Fixture::new();
        let listener = UnixListener::bind(fixture.paths.socket()).unwrap();
        fs::set_permissions(fixture.paths.socket(), fs::Permissions::from_mode(0o700)).unwrap();
        listener.set_nonblocking(true).unwrap();
        assert!(matches!(
            connect(&fixture.paths),
            Err(ServerError::UnsafeStorage)
        ));
        assert!(
            matches!(listener.accept(), Err(error) if error.kind() == std::io::ErrorKind::WouldBlock)
        );
        assert_eq!(
            fs::symlink_metadata(fixture.paths.socket()).unwrap().mode() & 0o7777,
            0o700
        );
    }

    #[test]
    fn socket_publication_preserves_an_unsafe_ready_entry_and_a_replaced_staging_entry() {
        let fixture = Fixture::new();
        let runtime = Directory::open(&fixture.paths.runtime, false).unwrap();
        let pending = fixture.paths.runtime.join("p");
        let _listener = UnixListener::bind(&pending).unwrap();
        fs::set_permissions(&pending, fs::Permissions::from_mode(0o600)).unwrap();
        let expected = runtime.socket_identity(c"p").unwrap().unwrap();
        fs::write(fixture.paths.socket(), b"existing entry").unwrap();
        assert_eq!(
            runtime.publish_socket(c"p", expected),
            Err(ServerError::UnsafeStorage)
        );
        assert_eq!(fs::read(fixture.paths.socket()).unwrap(), b"existing entry");
        fs::remove_file(fixture.paths.socket()).unwrap();
        fs::rename(&pending, fixture.paths.runtime.join("old")).unwrap();
        let _replacement = UnixListener::bind(&pending).unwrap();
        fs::set_permissions(&pending, fs::Permissions::from_mode(0o600)).unwrap();
        assert_eq!(
            runtime.publish_socket(c"p", expected),
            Err(ServerError::UnsafeStorage)
        );
        assert!(!fixture.paths.socket().exists());
        assert!(pending.exists());
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }
    struct ChildGuard(Child);
    impl Drop for ChildGuard {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    #[test]
    #[ignore = "runs only as a child of the server startup tests"]
    fn server_start_child() {
        let root = PathBuf::from(std::env::var_os(CHILD_ROOT).expect("startup fixture root"));
        let result = serve(&Paths {
            identity: "spaceterm-development".into(),
            data: root.join("d"),
            runtime: root.join("r"),
        });
        assert_eq!(result, Err(ServerError::StorageUnavailable));
    }

    #[test]
    #[ignore = "runs only as a child of the owned Development lifetime test"]
    fn owned_development_child() {
        struct Panes(PathBuf);
        impl crate::pane::PaneService for Panes {
            fn request(
                &mut self,
                _: [u8; 32],
                _: [u8; 32],
                _: crate::pane::Request,
            ) -> crate::pane::Reply {
                crate::pane::Reply::Unchanged
            }
            fn disconnected(&mut self, _: [u8; 32]) {}
            fn shutdown(&mut self) -> Result<(), ServerError> {
                fs::write(self.0.join("cleaned"), b"done").unwrap();
                Ok(())
            }
        }
        let root = PathBuf::from(std::env::var_os(CHILD_ROOT).unwrap());
        let result = serve_owned(
            &Paths {
                identity: "spaceterm-development".into(),
                data: root.join("d"),
                runtime: root.join("r"),
            },
            Box::new(Panes(root.clone())),
            std::io::stdin(),
            || {
                fs::write(root.join("ready"), b"ready").unwrap();
                Ok(())
            },
        );
        if std::env::var_os("SPACETERM_OWNED_EXPECT_REFUSAL").is_some() {
            assert_eq!(result, Err(ServerError::StorageUnavailable));
        } else {
            result.unwrap();
        }
    }

    fn owned_child(fixture: &Fixture, refuse: bool) -> ChildGuard {
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args([
                "--exact",
                "host::startup_tests::owned_development_child",
                "--ignored",
            ])
            .env(CHILD_ROOT, &fixture.root)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::inherit());
        if refuse {
            command.env("SPACETERM_OWNED_EXPECT_REFUSAL", "1");
        }
        ChildGuard(command.spawn().unwrap())
    }

    fn await_owned_exit(child: &mut ChildGuard) {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(exit) = child.0.try_wait().unwrap() {
                assert!(exit.success());
                return;
            }
            assert!(Instant::now() < deadline, "owned server did not finish");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn owned_development_eof_finishes_panes_and_preserves_retained_identity_on_relaunch() {
        let fixture = Fixture::new();
        let identity = fs::read(fixture.paths.data.join("identity.json")).unwrap();
        for _ in 0..2 {
            let mut child = owned_child(&fixture, false);
            let deadline = Instant::now() + Duration::from_secs(5);
            while !fixture.root.join("ready").exists() {
                assert!(child.0.try_wait().unwrap().is_none());
                assert!(
                    Instant::now() < deadline,
                    "owned server did not become ready"
                );
                std::thread::sleep(Duration::from_millis(10));
            }
            // Readiness proves this child owns the endpoint. A competing run cannot acquire it.
            let mut refused = owned_child(&fixture, true);
            refused.0.stdin.take();
            await_owned_exit(&mut refused);
            assert!(child.0.try_wait().unwrap().is_none());
            assert!(!fixture.root.join("cleaned").exists());
            child.0.stdin.take();
            await_owned_exit(&mut child);
            assert_eq!(fs::read(fixture.root.join("cleaned")).unwrap(), b"done");
            assert!(!fixture.paths.socket().exists());
            assert_eq!(
                fs::read(fixture.paths.data.join("identity.json")).unwrap(),
                identity
            );
            fs::remove_file(fixture.root.join("ready")).unwrap();
            fs::remove_file(fixture.root.join("cleaned")).unwrap();
        }
    }

    #[test]
    #[ignore = "runs only as a child of the server signal test"]
    fn server_signal_child() {
        install_shutdown_signals().unwrap();
        let root = PathBuf::from(std::env::var_os(CHILD_ROOT).expect("startup fixture root"));
        serve(&Paths {
            identity: "spaceterm-development".into(),
            data: root.join("d"),
            runtime: root.join("r"),
        })
        .unwrap();
    }

    #[test]
    fn shutdown_signal_stops_server_with_an_inherited_blocked_mask() {
        let fixture = Fixture::new();
        let mut child = fixture.start_test("host::startup_tests::server_signal_child", true);
        let deadline = Instant::now() + Duration::from_secs(3);
        let mut stream = loop {
            match UnixStream::connect(fixture.paths.socket()) {
                Ok(stream) => break stream,
                Err(_) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(10))
                }
                Err(_) => panic!("server did not start"),
            }
        };
        protocol::admit_client(
            &mut stream,
            (&fixture.device, &"Test".into()),
            &fixture.pin,
            "spaceterm-development",
            &[1],
        )
        .unwrap();
        let mut idle = UnixStream::connect(fixture.paths.socket()).unwrap();
        // SAFETY: the guard owns this live child process.
        assert_eq!(
            unsafe { libc::kill(child.0.id() as libc::pid_t, libc::SIGTERM) },
            0
        );
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            if let Some(exit) = child.0.try_wait().unwrap() {
                assert!(exit.success(), "server must finish cleanup before exiting");
                assert!(
                    !fixture.paths.socket().exists(),
                    "successful shutdown must remove its endpoint"
                );
                use std::io::Read;
                assert_eq!(idle.read(&mut [0]).unwrap(), 0, "idle admission must close");
                break;
            }
            assert!(
                Instant::now() < deadline,
                "server ignored its shutdown signal"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    #[ignore = "runs only as a child of the server shutdown admission test"]
    fn server_shutdown_child() {
        use crate::pane::{PaneService, Reply, Request};
        use std::sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        };
        struct Service {
            stopped: bool,
            late_requests: Arc<AtomicUsize>,
        }
        impl PaneService for Service {
            fn request(&mut self, _: [u8; 32], _: [u8; 32], request: Request) -> Reply {
                if self.stopped {
                    self.late_requests.fetch_add(1, Ordering::SeqCst);
                }
                if matches!(request, Request::Shutdown) {
                    self.stopped = true;
                    Reply::Closed
                } else {
                    Reply::Unchanged
                }
            }
            fn disconnected(&mut self, _: [u8; 32]) {}
            fn shutdown(&mut self) -> Result<(), ServerError> {
                self.stopped = true;
                Ok(())
            }
        }
        let root = PathBuf::from(std::env::var_os(CHILD_ROOT).expect("startup fixture root"));
        let late_requests = Arc::new(AtomicUsize::new(0));
        serve_with_panes(
            &Paths {
                identity: "spaceterm-development".into(),
                data: root.join("d"),
                runtime: root.join("r"),
            },
            Some(Box::new(Service {
                stopped: false,
                late_requests: Arc::clone(&late_requests),
            })),
        )
        .unwrap();
        fs::write(root.join("stopped"), []).unwrap();
        // Keep the library caller alive while the admitted client finishes its partial frame.
        std::thread::sleep(Duration::from_millis(500));
        assert_eq!(
            late_requests.load(Ordering::SeqCst),
            0,
            "request reached service after shutdown"
        );
    }

    #[cfg(feature = "client")]
    #[test]
    #[ignore = "runs only as a child of the shared transport admission test"]
    fn shared_transport_child() {
        use crate::pane::{PaneService, Reply, Request};
        struct Service;
        impl PaneService for Service {
            fn request(&mut self, _: [u8; 32], _: [u8; 32], request: Request) -> Reply {
                match request {
                    Request::Capabilities => Reply::Capabilities {
                        development_pane: true,
                        pane_takeover: true,
                        interactive_input: true,
                        key_input_batches: false,
                        native_effects: true,
                        reading: false,
                        graphics: false,
                        accessibility: false,
                        directory_recovery: true,
                        directory_creation: false,
                        directory_matching: false,
                        worktree_writes: false,
                        end_all_sessions: false,
                        workspace_policy: true,
                        presentation_changes: false,
                    },
                    Request::Inventory => Reply::Inventory {
                        inventory: crate::hierarchy::State::empty().inventory,
                        next_sequence: 1,
                    },
                    Request::SelectPane { pane, .. } => Reply::Restored {
                        fresh: false,
                        session_ended: false,
                        recovery: None,
                        directory_unavailable: false,
                        server: [1; 32],
                        workspace: [2; 32],
                        tab: pane,
                        pane,
                    },
                    Request::GraphicsChunk { image, offset, .. } => {
                        Reply::GraphicsChunk(Ok(crate::pane::graphics::Chunk {
                            image,
                            offset,
                            bytes: vec![255; crate::pane::graphics::CHUNK_BYTES],
                        }))
                    }
                    Request::SelectionChunk {
                        generation, offset, ..
                    } => Reply::SelectionChunk {
                        generation,
                        offset,
                        total: 32 * crate::pane::reading::SELECTION_CHUNK_BYTES,
                        bytes: vec![65; crate::pane::reading::SELECTION_CHUNK_BYTES],
                    },
                    Request::Shutdown => Reply::Closed,
                    _ => Reply::Unchanged,
                }
            }
            fn disconnected(&mut self, _: [u8; 32]) {}
            fn shutdown(&mut self) -> Result<(), ServerError> {
                Ok(())
            }
        }
        let root = PathBuf::from(std::env::var_os(CHILD_ROOT).expect("startup fixture root"));
        serve_with_panes(
            &Paths {
                identity: "spaceterm-development".into(),
                data: root.join("d"),
                runtime: root.join("r"),
            },
            Some(Box::new(Service)),
        )
        .unwrap();
    }

    #[cfg(feature = "client")]
    #[test]
    fn a_client_that_stops_reading_resource_chunks_does_not_stall_another_client() {
        use crate::pane::{Lease, PaneConnection, Reply, Request};
        for (major, graphics) in [(1, false), (1, true)] {
            let fixture = Fixture::new();
            let _child = fixture.start_test("host::startup_tests::shared_transport_child", false);
            let deadline = Instant::now() + Duration::from_secs(3);
            while !fixture.paths.socket().exists() {
                assert!(Instant::now() < deadline);
                std::thread::sleep(Duration::from_millis(5));
            }
            let mut slow = loop {
                match UnixStream::connect(fixture.paths.socket()) {
                    Ok(stream) => break stream,
                    Err(error)
                        if error.kind() == std::io::ErrorKind::ConnectionRefused
                            && Instant::now() < deadline =>
                    {
                        std::thread::sleep(Duration::from_millis(5))
                    }
                    Err(error) => panic!("test listener unavailable: {error}"),
                }
            };
            protocol::admit_client(
                &mut slow,
                (&fixture.device, &"Test".into()),
                &fixture.pin,
                "spaceterm-development",
                &[major],
            )
            .unwrap();
            let lease = Lease {
                server: [1; 32],
                session: [2; 32],
                control: [3; 32],
                connection: [4; 32],
            };
            // Replies exceed socket buffering. This authenticated reader deliberately consumes none.
            for index in 0..16 {
                protocol::Wire::terminal(&mut slow)
                    .unwrap()
                    .write(&if graphics {
                        Request::GraphicsChunk {
                            lease,
                            image: crate::pane::graphics::ImageKey {
                                image_id: 1,
                                generation: 1,
                            },
                            offset: index * crate::pane::graphics::CHUNK_BYTES,
                        }
                    } else {
                        Request::SelectionChunk {
                            lease,
                            generation: 1,
                            offset: index * crate::pane::reading::SELECTION_CHUNK_BYTES,
                        }
                    })
                    .unwrap();
            }
            let mut stream = UnixStream::connect(fixture.paths.socket()).unwrap();
            protocol::admit_client(
                &mut stream,
                (&fixture.device, &"Test".into()),
                &fixture.pin,
                "spaceterm-development",
                &[major],
            )
            .unwrap();
            let mut current = PaneConnection::negotiate(stream).unwrap();
            let started = Instant::now();
            for _ in 0..16 {
                assert!(matches!(
                    current.exchange(&Request::Inventory).unwrap(),
                    Reply::Inventory { .. }
                ));
            }
            assert!(
                started.elapsed() < Duration::from_secs(2),
                "a slow reader blocked another connection"
            );
        }
    }

    #[cfg(feature = "client")]
    #[test]
    fn two_desktops_keep_all_supported_panes_without_exhausting_management_admission() {
        use crate::pane::{PaneConnection, PaneSize, Reply, Request, SharedPaneConnection};
        for major in [1] {
            let fixture = Fixture::new();
            let other = protocol::random().unwrap();
            let _child = fixture.start_test("host::startup_tests::shared_transport_child", false);
            let deadline = Instant::now() + Duration::from_secs(3);
            let first = SharedPaneConnection::default();
            let second = SharedPaneConnection::default();
            let open = |device: &[u8; 32]| {
                let mut stream = loop {
                    match UnixStream::connect(fixture.paths.socket()) {
                        Ok(stream) => break stream,
                        Err(error)
                            if matches!(
                                error.kind(),
                                std::io::ErrorKind::NotFound
                                    | std::io::ErrorKind::ConnectionRefused
                            ) && Instant::now() < deadline =>
                        {
                            std::thread::sleep(Duration::from_millis(5));
                        }
                        Err(error) => panic!("shared transport listener unavailable: {error}"),
                    }
                };
                protocol::admit_client(
                    &mut stream,
                    (device, &"Test".into()),
                    &fixture.pin,
                    "spaceterm-development",
                    &[major],
                )?;
                PaneConnection::negotiate(stream)
            };
            let mut retained = Vec::new();
            for (desktop, device) in [(&first, &fixture.device), (&second, &other)] {
                let mut inventory = desktop.connect(|| open(device)).unwrap();
                assert!(matches!(
                    inventory.exchange(&Request::Inventory).unwrap(),
                    Reply::Inventory { .. }
                ));
                retained.push(inventory);
                for pane in 0..crate::hierarchy::MAX_PANES {
                    let mut connection = desktop
                        .connect(|| open(device))
                        .expect("supported Pane must have transport capacity");
                    let id = [pane as u8; 32];
                    let reply = connection
                        .exchange(&Request::SelectPane {
                            pane: id,
                            explicit: false,
                            size: PaneSize {
                                columns: 80,
                                rows: 24,
                                cell_width: 8,
                                cell_height: 18,
                            },
                            style: None,
                        })
                        .unwrap();
                    assert!(matches!(reply, Reply::Restored { pane, .. } if pane == id));
                    retained.push(connection);
                }
            }
            // Both complete desktop hierarchies remain subscribed while Settings and
            // management authenticate independently of their shared transports.
            let mut management = open(&fixture.device).unwrap();
            for connection in &mut retained {
                assert!(matches!(
                    connection.exchange(&Request::Inventory).unwrap(),
                    Reply::Inventory { .. }
                ));
            }
            assert!(matches!(
                management.exchange(&Request::Shutdown).unwrap(),
                Reply::Closed
            ));
        }
    }

    #[test]
    fn shutdown_refuses_an_attach_already_waiting_for_its_frame() {
        use crate::pane::{PaneSize, Reply, Request};
        use std::io::Write;
        let fixture = Fixture::new();
        let mut child = fixture.start_test("host::startup_tests::server_shutdown_child", false);
        let deadline = Instant::now() + Duration::from_secs(3);
        let mut pending = loop {
            match UnixStream::connect(fixture.paths.socket()) {
                Ok(stream) => break stream,
                Err(_) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(10))
                }
                Err(_) => panic!("server did not start"),
            }
        };
        protocol::admit_client(
            &mut pending,
            (&fixture.device, &"Test".into()),
            &fixture.pin,
            "spaceterm-development",
            &[1],
        )
        .unwrap();
        let body = serde_json::to_vec(&Request::Attach {
            create: true,
            size: PaneSize {
                columns: 80,
                rows: 24,
                cell_width: 8,
                cell_height: 18,
            },
            resume: None,
            style: None,
        })
        .unwrap();
        pending
            .write_all(&(body.len() as u32).to_be_bytes())
            .unwrap();
        // The admitted worker waits on the incomplete frame while another connection shuts down.
        std::thread::sleep(Duration::from_millis(50));
        let mut shutdown = UnixStream::connect(fixture.paths.socket()).unwrap();
        protocol::admit_client(
            &mut shutdown,
            (&fixture.device, &"Test".into()),
            &fixture.pin,
            "spaceterm-development",
            &[1],
        )
        .unwrap();
        let mut wire = protocol::Wire::terminal(&mut shutdown).unwrap();
        wire.write(&Request::Shutdown).unwrap();
        assert!(matches!(wire.read::<Reply>().unwrap(), Reply::Closed));
        let deadline = Instant::now() + Duration::from_secs(3);
        while !fixture.root.join("stopped").exists() {
            assert!(Instant::now() < deadline, "server did not finish shutdown");
            std::thread::sleep(Duration::from_millis(10));
        }
        let _ = pending.write_all(&body);
        assert!(
            protocol::Wire::terminal(&mut pending)
                .unwrap()
                .read::<Reply>()
                .is_err()
        );
        loop {
            if let Some(exit) = child.0.try_wait().unwrap() {
                assert!(
                    exit.success(),
                    "server dispatched an admitted request after shutdown"
                );
                break;
            }
            assert!(Instant::now() < deadline, "server child did not finish");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn startup_preserves_an_answering_listener_when_the_instance_lock_is_available() {
        let fixture = Fixture::new();
        let _listener = UnixListener::bind(fixture.paths.socket()).unwrap();
        fs::set_permissions(fixture.paths.socket(), fs::Permissions::from_mode(0o600)).unwrap();
        let original = fs::symlink_metadata(fixture.paths.socket()).unwrap().ino();
        let mut child = fixture.start();
        let deadline = Instant::now() + Duration::from_secs(2);
        let exit = loop {
            if let Some(exit) = child.0.try_wait().unwrap() {
                break Some(exit);
            }
            let current = fs::symlink_metadata(fixture.paths.socket()).unwrap().ino();
            if current != original || Instant::now() >= deadline {
                break None;
            }
            std::thread::sleep(Duration::from_millis(10));
        };
        assert_eq!(
            fs::symlink_metadata(fixture.paths.socket()).unwrap().ino(),
            original,
            "startup must preserve the live socket's identity"
        );
        assert!(
            exit.is_some_and(|exit| exit.success()),
            "startup must refuse the competing endpoint"
        );
    }
    #[test]
    fn startup_replaces_a_stale_socket_and_authenticates_the_saved_device() {
        let fixture = Fixture::new();
        let stale = UnixListener::bind(fixture.paths.socket()).unwrap();
        fs::set_permissions(fixture.paths.socket(), fs::Permissions::from_mode(0o600)).unwrap();
        drop(stale);
        let _child = fixture.start();
        let deadline = Instant::now() + Duration::from_secs(2);
        let mut stream = loop {
            match UnixStream::connect(fixture.paths.socket()) {
                Ok(stream) => break stream,
                Err(_) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(10))
                }
                Err(_) => panic!("the server did not replace its stale endpoint"),
            }
        };
        assert_eq!(
            protocol::admit_client(
                &mut stream,
                (&fixture.device, &"Test".into()),
                &fixture.pin,
                "spaceterm-development",
                &[1]
            ),
            Ok(crate::AuthenticatedStatus {
                protocol_major: 1,
                development_pane: false
            })
        );
    }

    #[test]
    fn stale_socket_removal_preserves_a_replacement_entry() {
        let fixture = Fixture::new();
        let runtime = Directory::open(&fixture.paths.runtime, false).unwrap();
        let _lock = runtime.lock("server.lock", true).unwrap();
        let stale = UnixListener::bind(fixture.paths.socket()).unwrap();
        fs::set_permissions(fixture.paths.socket(), fs::Permissions::from_mode(0o600)).unwrap();
        let expected = runtime.socket_identity(c"s").unwrap().unwrap();
        drop(stale);
        fs::rename(fixture.paths.socket(), fixture.paths.runtime.join("old")).unwrap();
        let _replacement = UnixListener::bind(fixture.paths.socket()).unwrap();
        fs::set_permissions(fixture.paths.socket(), fs::Permissions::from_mode(0o600)).unwrap();
        let replacement = fs::symlink_metadata(fixture.paths.socket()).unwrap().ino();
        assert_eq!(
            runtime.remove_socket(c"s", expected),
            Err(ServerError::UnsafeStorage)
        );
        assert_eq!(
            fs::symlink_metadata(fixture.paths.socket()).unwrap().ino(),
            replacement
        );
    }
}
