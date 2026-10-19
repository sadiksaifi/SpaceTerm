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
    mut panes: Option<Box<dyn crate::pane::PaneService>>,
    supervised: bool,
) -> Result<(), ServerError> {
    crate::service::check_runtime(paths, supervised)?;
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
    // The fixed admission limit bounds worker allocation; each frame has an absolute deadline.
    for connection in listener.incoming() {
        if stopping.load(Ordering::Acquire) || STOP.load(Ordering::Acquire) {
            break;
        }
        let mut stream = match connection {
            Ok(stream) => stream,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                if let Some(panes) = panes.lock().unwrap_or_else(|e| e.into_inner()).as_mut() {
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
        let panes = Arc::clone(&panes);
        let workers = Arc::clone(&workers);
        let stopping = Arc::clone(&stopping);
        let service_paths = paths.clone();
        workers.fetch_add(1, Ordering::AcqRel);
        connections.retain(|(worker, _)| !worker.is_finished());
        let cancellation = stream.try_clone().map_err(|_| ServerError::Protocol)?;
        let handle = std::thread::spawn(move || {
            let _ = (|| -> Result<(), ServerError> {
                let (_, device) =
                    protocol::admit_server(&mut stream, &record.server, &service_paths.identity)?;
                let connection = protocol::random()?;
                let result = (|| {
                    loop {
                        if stopping.load(Ordering::Acquire) || STOP.load(Ordering::Acquire) {
                            break;
                        }
                        let mut wire = protocol::Wire::terminal(&mut stream)?;
                        let request = wire.read()?;
                        let shutdown = matches!(request, crate::pane::Request::Shutdown);
                        if shutdown {
                            crate::service::retain_stop(&service_paths)?;
                        }
                        let (reply, stopped) = {
                            let mut panes = panes.lock().unwrap_or_else(|e| e.into_inner());
                            // Admission is fenced under the same lock as final cleanup. A worker
                            // may have been waiting for its frame or this lock when shutdown began.
                            if stopping.load(Ordering::Acquire) || STOP.load(Ordering::Acquire) {
                                return Err(ServerError::NotRunning);
                            }
                            let reply = match panes.as_mut() {
                                Some(panes) => panes.request(device, connection, request),
                                None if matches!(request, crate::pane::Request::Capabilities) => {
                                    crate::pane::Reply::Capabilities {
                                        development_pane: false,
                                        pane_takeover: false,
                                        interactive_input: false,
                                        native_effects: false,
                                        reading: false,
                                        graphics: false,
                                        accessibility: false,
                                        directory_recovery: true,
                                        worktree_writes: false,
                                        workspace_policy: false,
                                    }
                                }
                                None => {
                                    crate::pane::Reply::Refused(crate::pane::Refusal::Unsupported)
                                }
                            };
                            let stopped = shutdown && matches!(reply, crate::pane::Reply::Closed);
                            if stopped {
                                stopping.store(true, Ordering::Release);
                            }
                            (reply, stopped)
                        };
                        wire.write(&reply)?;
                        if stopped {
                            break;
                        }
                    }
                    Ok::<_, ServerError>(())
                })();
                if let Some(panes) = panes.lock().unwrap_or_else(|e| e.into_inner()).as_mut() {
                    panes.disconnected(connection);
                }
                result
            })();
            workers.fetch_sub(1, Ordering::AcqRel);
        });
        connections.push((handle, cancellation));
    }
    stopping.store(true, Ordering::Release);
    drop(listener);
    for (_, stream) in &connections {
        let _ = stream.shutdown(std::net::Shutdown::Both);
    }
    for (worker, _) in connections {
        let _ = worker.join();
    }
    if let Some(panes) = panes.lock().unwrap_or_else(|e| e.into_inner()).as_mut() {
        panes.shutdown()?;
    }
    runtime.verify_path(&paths.runtime)?;
    runtime.remove_socket(c"s", socket_identity)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
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
            &fixture.device,
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
                        native_effects: true,
                        reading: false,
                        graphics: false,
                        accessibility: false,
                        directory_recovery: true,
                        worktree_writes: false,
                        workspace_policy: true,
                    },
                    Request::Inventory => Reply::Inventory {
                        inventory: crate::hierarchy::State::empty().inventory,
                        next_sequence: 1,
                    },
                    Request::SelectPane { pane, .. } => Reply::Restored {
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
                &fixture.device,
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
                &fixture.device,
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
                    device,
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
            &fixture.device,
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
            &fixture.device,
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
                &fixture.device,
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
