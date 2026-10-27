//! The account-local host: one listener per server, one serving thread per admitted connection,
//! and one sync worker. The accept loop waits on its listeners, a wake pipe and the service's next
//! tick; it never sleeps.
use crate::connection::Wake;
use crate::connection::io::Waker;
use crate::connection::outbox::Outbox;
use crate::connection::server::AdmittedConnection;
use crate::connection::service::{
    DirectManagement, Dispatch, PaneService, Peer, SharedPaneService,
};
use crate::connection::sync::SyncWorker;
use crate::connection::unix::UnixCarrier;
use crate::pane::control::ClientName;
use crate::protocol::{EffectDone, Input, Operation, Refusal, RequestId, View};
use crate::{ServerError, protocol, storage::Directory};
use spaceterm_directories::ServerDirectories;
use std::{
    ffi::CStr,
    fs,
    io::{Read, Write},
    os::{
        fd::AsRawFd,
        unix::{
            ffi::OsStrExt,
            fs::PermissionsExt,
            net::{UnixListener, UnixStream},
        },
    },
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Instant,
};

static STOP: AtomicBool = AtomicBool::new(false);
extern "C" fn stop_signal(_: libc::c_int) {
    STOP.store(true, Ordering::Release);
    wake_for_signal();
}
pub(crate) fn install_shutdown_signals() -> Result<(), ServerError> {
    STOP.store(false, Ordering::Release);
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

/// Starts a client link on an admitted account-local socket.
pub(crate) fn link(
    stream: UnixStream,
    events: Arc<dyn crate::connection::link::LinkSink>,
) -> Result<crate::connection::link::ServerLink, ServerError> {
    let carrier = UnixCarrier::new(stream).map_err(|_| ServerError::Protocol)?;
    crate::connection::io::spawn_link(Box::new(carrier), events).map_err(|_| ServerError::Protocol)
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
    panes: Option<crate::PaneFactory>,
) -> Result<(), ServerError> {
    serve_runtime(paths, panes, false)
}

pub(crate) fn serve_runtime(
    paths: &Paths,
    panes: Option<crate::PaneFactory>,
    supervised: bool,
) -> Result<(), ServerError> {
    serve_runtime_ready(paths, panes, supervised, || Ok(()))
}

/// Stops the host: set by a Shutdown request, a signal or the owning supervisor's exit.
pub(crate) struct Stop {
    stopping: AtomicBool,
    wake: Arc<dyn Wake>,
}
impl Stop {
    pub(crate) fn stopping(&self) -> bool {
        self.stopping.load(Ordering::Acquire) || STOP.load(Ordering::Acquire)
    }
    pub(crate) fn flag(&self) -> &AtomicBool {
        &self.stopping
    }
    fn request(&self) {
        self.stopping.store(true, Ordering::Release);
        self.wake.wake();
    }
}

/// What every connection of one running host shares.
#[derive(Clone)]
pub(crate) struct Environment {
    pub(crate) panes: SharedPaneService,
    pub(crate) sync: SyncWorker,
    pub(crate) management: Option<Arc<dyn DirectManagement>>,
    pub(crate) stop: Arc<Stop>,
}

#[cfg(test)]
impl Environment {
    /// An environment for one test connection, whose host never stops it.
    pub(crate) fn for_tests(
        panes: SharedPaneService,
        sync: SyncWorker,
        direct: Option<crate::direct::host::Shared>,
    ) -> Self {
        Self {
            management: direct.map(|state| {
                Arc::new(HostDirect {
                    state,
                    panes: Arc::clone(&panes),
                }) as Arc<dyn DirectManagement>
            }),
            panes,
            sync,
            stop: Arc::new(Stop {
                stopping: AtomicBool::new(false),
                wake: Arc::new(|| {}),
            }),
        }
    }
}

/// The host's own rules around the application's service: a Shutdown first retains Stop intent,
/// and a completed one stops the host.
struct HostService {
    inner: Box<dyn PaneService>,
    paths: Paths,
    stop: Arc<Stop>,
}
impl PaneService for HostService {
    fn restore(
        &mut self,
        store: crate::organization::OrganizationStore,
    ) -> Result<(), ServerError> {
        self.inner.restore(store)
    }
    fn admitted(&mut self, peer: &Peer, name: ClientName, outbox: Outbox) {
        self.inner.admitted(peer, name, outbox)
    }
    fn revoked(&mut self, client: [u8; 32]) {
        self.inner.revoked(client)
    }
    fn request(&mut self, peer: &Peer, id: RequestId, operation: Operation) -> Dispatch {
        let shutdown = matches!(operation, Operation::Shutdown);
        if shutdown && crate::service::retain_stop(&self.paths).is_err() {
            return Dispatch::reply(Err(Refusal::StorageUnavailable));
        }
        let dispatch = self.inner.request(peer, id, operation);
        if shutdown && matches!(&dispatch, Dispatch::Reply(result) if result.is_ok()) {
            self.stop.request();
        }
        dispatch
    }
    fn input(&mut self, peer: &Peer, input: Input) {
        self.inner.input(peer, input)
    }
    fn view(&mut self, peer: &Peer, view: View) {
        self.inner.view(peer, view)
    }
    fn effect_done(&mut self, peer: &Peer, done: EffectDone) {
        self.inner.effect_done(peer, done)
    }
    fn acknowledge(&mut self, peer: &Peer, sequence: u64) {
        self.inner.acknowledge(peer, sequence)
    }
    fn cancel(&mut self, peer: &Peer, id: RequestId) {
        self.inner.cancel(peer, id)
    }
    fn stopping(&mut self) {
        self.inner.stopping()
    }
    fn ticker(&mut self, wake: Arc<dyn Wake>) {
        self.inner.ticker(wake)
    }
    fn tick(&mut self, now: Instant) -> Option<Instant> {
        self.inner.tick(now)
    }
    fn disconnected(&mut self, peer: &Peer) {
        self.inner.disconnected(peer)
    }
    fn shutdown(&mut self) -> Result<(), ServerError> {
        self.inner.shutdown()
    }
}

/// A host without terminal authority refuses every operation.
struct NoPanes;
impl PaneService for NoPanes {
    fn admitted(&mut self, _: &Peer, _: ClientName, _: Outbox) {}
    fn request(&mut self, _: &Peer, _: RequestId, _: Operation) -> Dispatch {
        Dispatch::reply(Err(Refusal::Unsupported))
    }
    fn input(&mut self, _: &Peer, _: Input) {}
    fn view(&mut self, _: &Peer, _: View) {}
    fn effect_done(&mut self, _: &Peer, _: EffectDone) {}
    fn acknowledge(&mut self, _: &Peer, _: u64) {}
    fn disconnected(&mut self, _: &Peer) {}
    fn shutdown(&mut self) -> Result<(), ServerError> {
        Ok(())
    }
}

/// Direct-network administration over the host's state. A revocation ends live connections
/// inside the state's lock before the service learns of it, and the two locks are never nested.
struct HostDirect {
    state: crate::direct::host::Shared,
    panes: SharedPaneService,
}
impl DirectManagement for HostDirect {
    fn manage(
        &self,
        peer: &Peer,
        request: crate::direct::Management,
    ) -> Result<crate::direct::ManagementReply, crate::direct::DirectError> {
        let actor = peer.pairing.map(|pairing| pairing.pairing);
        let result = self
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .manage(actor, request);
        let (reply, retired) = result?;
        if let Some(client) = retired {
            self.panes
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .revoked(client);
        }
        Ok(reply)
    }
}

/// One serving thread and what ends it: its socket's read side and its I/O thread's wake pipe.
struct Running {
    thread: std::thread::JoinHandle<()>,
    /// Set by the thread just before it rings the host's bell and ends.
    done: Arc<AtomicBool>,
    sever: Box<dyn FnOnce() + Send>,
    wake: Arc<dyn Wake>,
}
impl Running {
    /// A connection still in admission sees its socket close. An admitted one stops reading,
    /// writes its pending replies and says goodbye.
    fn stop(self) -> std::thread::JoinHandle<()> {
        (self.sever)();
        self.wake.wake();
        self.thread
    }
}

/// The signal handler's way to wake the accept loop. Writing to a pipe is async-signal-safe.
static SIGNAL_WAKE: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(-1);

fn wake_for_signal() {
    let fd = SIGNAL_WAKE.load(Ordering::Acquire);
    if fd >= 0 {
        // SAFETY: write is async-signal-safe and reads one byte from a static buffer.
        unsafe {
            libc::write(fd, [1u8].as_ptr().cast(), 1);
        }
    }
}

/// The owner receives readiness only after this process owns and publishes the endpoint.
fn serve_runtime_ready(
    paths: &Paths,
    panes: Option<crate::PaneFactory>,
    supervised: bool,
    ready: impl FnOnce() -> Result<(), ServerError>,
) -> Result<(), ServerError> {
    crate::service::check_runtime(paths, supervised)?;
    let data = Directory::open(&paths.data, false)?;
    // Verify retained identity before changing any runtime state. Never generate a replacement.
    // The identity never changes while this host runs, so admission reuses it.
    let server = data.read()?.server;
    let runtime = Directory::open(&paths.runtime, true)?;
    let _lock = runtime.lock("server.lock", true)?;
    crate::service::check_runtime(paths, supervised)?;
    let (bell, ring) = UnixStream::pair().map_err(|_| ServerError::StartupFailed)?;
    for end in [&bell, &ring] {
        end.set_nonblocking(true)
            .map_err(|_| ServerError::StartupFailed)?;
    }
    let ring = Arc::new(ring);
    let stop = Arc::new(Stop {
        stopping: AtomicBool::new(false),
        wake: Arc::new({
            let ring = Arc::clone(&ring);
            move || {
                let _ = (&*ring).write(&[1]);
            }
        }),
    });
    let (sync, sync_thread) = SyncWorker::spawn().map_err(|_| ServerError::StartupFailed)?;
    let inner = match panes {
        Some(build) => build(sync.clone())?,
        None => Box::new(NoPanes),
    };
    let mut panes: Box<dyn PaneService> = Box::new(HostService {
        inner,
        paths: paths.clone(),
        stop: Arc::clone(&stop),
    });
    panes.restore(crate::organization::OrganizationStore(Directory::open(
        &paths.data,
        false,
    )?))?;
    panes.ticker(Arc::clone(&stop.wake));
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
    listener
        .set_nonblocking(true)
        .map_err(|_| ServerError::Protocol)?;
    let panes: SharedPaneService = Arc::new(Mutex::new(panes));
    // A failed direct-network store never prevents owning-account admission.
    let direct = crate::direct::host::State::open(paths).ok();
    if let Some(state) = &direct {
        state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .set_wake(Arc::clone(&stop.wake));
    }
    let environment = Environment {
        panes: Arc::clone(&panes),
        sync: sync.clone(),
        management: direct.as_ref().map(|state| {
            Arc::new(HostDirect {
                state: Arc::clone(state),
                panes: Arc::clone(&panes),
            }) as Arc<dyn DirectManagement>
        }),
        stop: Arc::clone(&stop),
    };
    SIGNAL_WAKE.store(ring.as_raw_fd(), Ordering::Release);
    let readiness = ready();
    if readiness.is_err() {
        stop.request();
    }
    let mut connections: Vec<Running> = Vec::new();
    let mut network: Vec<Running> = Vec::new();
    while !stop.stopping() {
        let deadline = tick(&panes, direct.as_ref());
        // A connection thread rings the bell when it ends, so a full set of network slots waits
        // for that instead of a listener it cannot accept from.
        connections.retain(|running| !running.done.load(Ordering::Acquire));
        network.retain(|running| !running.done.load(Ordering::Acquire));
        let listening = network_listener(direct.as_ref(), network.len());
        wait_for_work(&bell, &listener, listening, deadline);
        if stop.stopping() {
            break;
        }
        connections.retain(|running| !running.done.load(Ordering::Acquire));
        network.retain(|running| !running.done.load(Ordering::Acquire));
        // The fixed admission limits bound worker allocation; admission has an absolute deadline.
        while network.len() < MAX_NETWORK_CONNECTIONS
            && let Some(state) = &direct
        {
            let accepted = state.lock().unwrap_or_else(|e| e.into_inner()).accept();
            let Some((tcp, generation)) = accepted else {
                break;
            };
            match spawn_network(tcp, generation, state, &environment) {
                Ok(running) => network.push(running),
                Err(_) => break,
            }
        }
        loop {
            let stream = match listener.accept() {
                Ok((stream, _)) => stream,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(_) => return Err(ServerError::Protocol),
            };
            if connections.len() >= MAX_LOCAL_CONNECTIONS || check_peer(&stream).is_err() {
                continue;
            }
            if let Ok(running) = spawn_local(stream, server, paths, &environment) {
                connections.push(running);
            }
        }
    }
    stop.request();
    SIGNAL_WAKE.store(-1, Ordering::Release);
    drop(listener);
    // Ticks no longer run, so replies they would have answered are refused now.
    panes.lock().unwrap_or_else(|e| e.into_inner()).stopping();
    let threads: Vec<_> = connections
        .into_iter()
        .chain(network)
        .map(Running::stop)
        .collect();
    for thread in threads {
        let _ = thread.join();
    }
    if let Some(state) = &direct {
        state.lock().unwrap_or_else(|e| e.into_inner()).stop();
    }
    // Jobs may take the service lock, so they finish before it is held for shutdown.
    let _ = sync.flush();
    panes.lock().unwrap_or_else(|e| e.into_inner()).shutdown()?;
    let _ = sync.flush();
    // The service and every connection's environment hold the last clones of the sync worker.
    drop(environment);
    drop(panes);
    drop(sync);
    let _ = sync_thread.join();
    runtime.verify_path(&paths.runtime)?;
    runtime.remove_socket(c"s", socket_identity)?;
    readiness
}

/// Serving threads for account-local connections, including those still in admission.
const MAX_LOCAL_CONNECTIONS: usize = 64;
/// Serving threads for network connections, including those still in the WSS handshake.
const MAX_NETWORK_CONNECTIONS: usize = 32;

/// The network listener the accept loop waits on, or `None` while every network slot is taken.
fn network_listener(
    direct: Option<&crate::direct::host::Shared>,
    running: usize,
) -> Option<std::os::fd::RawFd> {
    if running >= MAX_NETWORK_CONNECTIONS {
        return None;
    }
    direct.and_then(|state| {
        state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .listener_fd()
    })
}

/// Runs the service's timed work and revocations found by direct-network recovery, and returns
/// when the service next needs a tick.
fn tick(
    panes: &SharedPaneService,
    direct: Option<&crate::direct::host::Shared>,
) -> Option<Instant> {
    let retired = direct
        .and_then(|state| {
            state
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .pending_releases()
                .ok()
        })
        .unwrap_or_default();
    let mut panes = panes.lock().unwrap_or_else(|e| e.into_inner());
    for client in retired {
        panes.revoked(client);
    }
    panes.tick(Instant::now())
}

/// Waits for a connection on either listener, a wake, or the service's next tick.
fn wait_for_work(
    bell: &UnixStream,
    listener: &UnixListener,
    network: Option<std::os::fd::RawFd>,
    deadline: Option<Instant>,
) {
    let mut descriptors = vec![
        libc::pollfd {
            fd: bell.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        },
        libc::pollfd {
            fd: listener.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        },
    ];
    if let Some(fd) = network {
        descriptors.push(libc::pollfd {
            fd,
            events: libc::POLLIN,
            revents: 0,
        });
    }
    let timeout = deadline.map_or(-1, |deadline| {
        let remaining = deadline.saturating_duration_since(Instant::now());
        remaining.as_micros().div_ceil(1000).min(i32::MAX as u128) as i32
    });
    // SAFETY: `descriptors` is a live, writable array of `pollfd` for the whole call.
    unsafe {
        libc::poll(
            descriptors.as_mut_ptr(),
            descriptors.len() as libc::nfds_t,
            timeout,
        );
    }
    let mut drained = [0; 64];
    while matches!((&*bell).read(&mut drained), Ok(count) if count > 0) {}
}

/// Marks a serving thread done and rings the host's bell, so the accept loop frees its slot and
/// may accept again.
fn ended(done: &AtomicBool, environment: &Environment) {
    done.store(true, Ordering::Release);
    environment.stop.wake.wake();
}

fn spawn_local(
    stream: UnixStream,
    server: [u8; 32],
    paths: &Paths,
    environment: &Environment,
) -> Result<Running, ServerError> {
    let (waker, wake) = Waker::new().map_err(|_| ServerError::Protocol)?;
    let cancel = stream.try_clone().map_err(|_| ServerError::Protocol)?;
    let identity = paths.identity.clone();
    let environment = environment.clone();
    let outbox_wake = Arc::clone(&wake);
    let done = Arc::new(AtomicBool::new(false));
    let finished = Arc::clone(&done);
    let thread = std::thread::Builder::new()
        .name("spaceterm-connection".into())
        .spawn(move || {
            let _ = serve_local(stream, server, &identity, waker, outbox_wake, &environment);
            ended(&finished, &environment);
        })
        .map_err(|_| ServerError::Protocol)?;
    Ok(Running {
        thread,
        done,
        sever: Box::new(move || {
            let _ = cancel.shutdown(std::net::Shutdown::Read);
        }),
        wake,
    })
}

fn serve_local(
    mut stream: UnixStream,
    server: [u8; 32],
    identity: &str,
    waker: Waker,
    wake: Arc<dyn Wake>,
    environment: &Environment,
) -> Result<(), ServerError> {
    let (_, client, name) = protocol::admit_server(&mut stream, &server, identity)?;
    let peer = Peer {
        client,
        connection: protocol::random()?,
        pairing: None,
    };
    let carrier = UnixCarrier::new(stream).map_err(|_| ServerError::Protocol)?;
    serve_admitted(
        Box::new(carrier),
        peer,
        name,
        Outbox::new(wake),
        waker,
        environment,
    );
    Ok(())
}

fn spawn_network(
    tcp: std::net::TcpStream,
    generation: [u8; 32],
    state: &crate::direct::host::Shared,
    environment: &Environment,
) -> Result<Running, ServerError> {
    let (waker, wake) = Waker::new().map_err(|_| ServerError::Protocol)?;
    let cancel = tcp.try_clone().map_err(|_| ServerError::Protocol)?;
    let state = Arc::clone(state);
    let environment = environment.clone();
    let outbox_wake = Arc::clone(&wake);
    let done = Arc::new(AtomicBool::new(false));
    let finished = Arc::clone(&done);
    let thread = std::thread::Builder::new()
        .name("spaceterm-network".into())
        .spawn(move || {
            let _ = crate::direct::host::serve(
                tcp,
                generation,
                state,
                &environment,
                waker,
                outbox_wake,
            );
            ended(&finished, &environment);
        })
        .map_err(|_| ServerError::Protocol)?;
    Ok(Running {
        thread,
        done,
        sever: Box::new(move || {
            let _ = cancel.shutdown(std::net::Shutdown::Read);
        }),
        wake,
    })
}

/// Serves one admitted connection on this thread until it ends.
pub(crate) fn serve_admitted(
    carrier: Box<dyn crate::connection::carrier::Carrier>,
    peer: Peer,
    name: ClientName,
    outbox: Outbox,
    waker: Waker,
    environment: &Environment,
) {
    environment
        .panes
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .admitted(&peer, name, outbox.clone());
    let connection = AdmittedConnection::new(
        peer,
        outbox,
        environment.sync.clone(),
        environment.management.clone(),
        Instant::now(),
    );
    crate::connection::io::serve(
        carrier,
        connection,
        &waker,
        &environment.panes,
        environment.stop.flag(),
    );
}

/// A source supervisor owns this foreground run through a pipe, including supervisor death.
/// Retained organization is loaded and shut down through the ordinary host implementation.
pub(crate) fn serve_owned(
    paths: &Paths,
    panes: crate::PaneFactory,
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
        STOP.store(true, Ordering::Release);
        wake_for_signal();
    });
    serve_runtime_ready(paths, Some(panes), false, ready)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn stopping_writes_pending_replies_then_says_goodbye_to_every_connection() {
        use crate::connection::link::{Health, LinkError};
        use crate::protocol::{Fatal, ReplyBody};
        /// Stops the host while it answers, as a completed Shutdown does.
        struct Stopping(Arc<Stop>);
        impl PaneService for Stopping {
            fn admitted(&mut self, _: &Peer, _: ClientName, _: Outbox) {}
            fn request(&mut self, _: &Peer, _: RequestId, _: Operation) -> Dispatch {
                self.0.stopping.store(true, Ordering::Release);
                Dispatch::reply(Ok(ReplyBody::Done))
            }
            fn input(&mut self, _: &Peer, _: Input) {}
            fn view(&mut self, _: &Peer, _: View) {}
            fn effect_done(&mut self, _: &Peer, _: EffectDone) {}
            fn acknowledge(&mut self, _: &Peer, _: u64) {}
            fn disconnected(&mut self, _: &Peer) {}
            fn shutdown(&mut self) -> Result<(), ServerError> {
                Ok(())
            }
        }
        let (sync, _) = SyncWorker::spawn().unwrap();
        let mut environment =
            Environment::for_tests(Arc::new(Mutex::new(Box::new(NoPanes))), sync, None);
        environment.panes = Arc::new(Mutex::new(Box::new(Stopping(Arc::clone(
            &environment.stop,
        )))));
        let mut threads = Vec::new();
        let mut links = Vec::new();
        let mut wakes = Vec::new();
        for connection in 0..2u8 {
            let (client, server) = UnixStream::pair().unwrap();
            let (waker, wake) = Waker::new().unwrap();
            wakes.push(Arc::clone(&wake));
            let environment = environment.clone();
            threads.push(std::thread::spawn(move || {
                serve_admitted(
                    Box::new(UnixCarrier::new(server).unwrap()),
                    Peer {
                        client: [1; 32],
                        connection: [connection; 32],
                        pairing: None,
                    },
                    "Test".into(),
                    Outbox::new(wake),
                    waker,
                    &environment,
                )
            }));
            links.push(
                crate::connection::io::spawn_link(
                    Box::new(UnixCarrier::new(client).unwrap()),
                    Arc::new(|_| {}),
                )
                .unwrap(),
            );
        }
        let reply = links[0]
            .request(Operation::ReleaseAttachment {
                attachment: crate::attachments::Attachment {
                    workspace: [2; 32],
                    generation: 1,
                },
            })
            .unwrap()
            .wait();
        assert!(
            matches!(reply, Ok(ReplyBody::Done)),
            "the pending reply is written"
        );
        // The host wakes every connection once it stops.
        for wake in &wakes {
            wake.wake();
        }
        for thread in threads {
            thread.join().unwrap();
        }
        for link in &links {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
            while link.health() == Health::Live {
                assert!(std::time::Instant::now() < deadline);
                std::thread::yield_now();
            }
            assert_eq!(
                link.health(),
                Health::Closed(LinkError::Goodbye(Fatal::Stopping))
            );
        }
    }

    #[test]
    fn stopping_waits_for_a_reply_the_sync_worker_still_owes() {
        use crate::connection::link::{Health, LinkError};
        use crate::protocol::{Fatal, ReplyBody};
        /// Stops the host and answers on the sync worker once the test releases the job.
        struct Syncing(Arc<Stop>, Mutex<Option<std::sync::mpsc::Receiver<()>>>);
        impl PaneService for Syncing {
            fn admitted(&mut self, _: &Peer, _: ClientName, _: Outbox) {}
            fn request(&mut self, _: &Peer, _: RequestId, _: Operation) -> Dispatch {
                self.0.stopping.store(true, Ordering::Release);
                let released = self.1.lock().unwrap().take().unwrap();
                Dispatch::After(Box::new(move || {
                    let _ = released.recv();
                    Ok(ReplyBody::Done)
                }))
            }
            fn input(&mut self, _: &Peer, _: Input) {}
            fn view(&mut self, _: &Peer, _: View) {}
            fn effect_done(&mut self, _: &Peer, _: EffectDone) {}
            fn acknowledge(&mut self, _: &Peer, _: u64) {}
            fn disconnected(&mut self, _: &Peer) {}
            fn shutdown(&mut self) -> Result<(), ServerError> {
                Ok(())
            }
        }
        let (release, released) = std::sync::mpsc::channel();
        let (sync, _) = SyncWorker::spawn().unwrap();
        let mut environment =
            Environment::for_tests(Arc::new(Mutex::new(Box::new(NoPanes))), sync, None);
        environment.panes = Arc::new(Mutex::new(Box::new(Syncing(
            Arc::clone(&environment.stop),
            Mutex::new(Some(released)),
        ))));
        let (client, server) = UnixStream::pair().unwrap();
        let (waker, wake) = Waker::new().unwrap();
        let host_wake = Arc::clone(&wake);
        let serving = {
            let environment = environment.clone();
            std::thread::spawn(move || {
                serve_admitted(
                    Box::new(UnixCarrier::new(server).unwrap()),
                    Peer {
                        client: [1; 32],
                        connection: [1; 32],
                        pairing: None,
                    },
                    "Test".into(),
                    Outbox::new(wake),
                    waker,
                    &environment,
                )
            })
        };
        let link = crate::connection::io::spawn_link(
            Box::new(UnixCarrier::new(client).unwrap()),
            Arc::new(|_| {}),
        )
        .unwrap();
        let pending = link.request(Operation::Shutdown).unwrap();
        while !environment.stop.stopping() {
            std::thread::yield_now();
        }
        // The host wakes every connection once it stops. This one stops reading, and its
        // goodbye waits for the owed reply.
        host_wake.wake();
        std::thread::sleep(std::time::Duration::from_millis(100));
        assert_eq!(link.health(), Health::Live);
        release.send(()).unwrap();
        assert!(matches!(pending.wait(), Ok(ReplyBody::Done)));
        serving.join().unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while link.health() == Health::Live {
            assert!(std::time::Instant::now() < deadline);
            std::thread::yield_now();
        }
        assert_eq!(
            link.health(),
            Health::Closed(LinkError::Goodbye(Fatal::Stopping))
        );
    }

    #[test]
    fn full_network_slots_leave_the_listener_unwatched_and_an_ended_thread_rings_the_bell() {
        let fixture = crate::direct::tests::Fixture::new();
        fixture.enable();
        let state = Some(&fixture.state);
        assert!(network_listener(state, MAX_NETWORK_CONNECTIONS - 1).is_some());
        // A readable listener the loop cannot accept from would wake it at once, forever.
        assert_eq!(network_listener(state, MAX_NETWORK_CONNECTIONS), None);
        let rung = Arc::new(AtomicBool::new(false));
        let (sync, _) = SyncWorker::spawn().unwrap();
        let mut environment =
            Environment::for_tests(Arc::new(Mutex::new(Box::new(NoPanes))), sync, None);
        environment.stop = Arc::new(Stop {
            stopping: AtomicBool::new(false),
            wake: Arc::new({
                let rung = Arc::clone(&rung);
                move || rung.store(true, Ordering::Release)
            }),
        });
        let (client, server) = UnixStream::pair().unwrap();
        let running = spawn_local(server, [7; 32], &fixture.paths, &environment).unwrap();
        // The peer leaves during admission, so the thread ends.
        drop(client);
        running.thread.join().unwrap();
        assert!(running.done.load(Ordering::Acquire));
        assert!(
            rung.load(Ordering::Acquire),
            "the accept loop must learn of the free slot"
        );
    }

    #[test]
    fn the_host_restores_its_service_from_retained_organization() {
        struct Restoring(Arc<AtomicBool>);
        impl PaneService for Restoring {
            fn restore(
                &mut self,
                _: crate::organization::OrganizationStore,
            ) -> Result<(), ServerError> {
                self.0.store(true, Ordering::Release);
                Ok(())
            }
            fn admitted(&mut self, _: &Peer, _: ClientName, _: Outbox) {}
            fn request(&mut self, _: &Peer, _: RequestId, _: Operation) -> Dispatch {
                Dispatch::reply(Err(Refusal::Unsupported))
            }
            fn input(&mut self, _: &Peer, _: Input) {}
            fn view(&mut self, _: &Peer, _: View) {}
            fn effect_done(&mut self, _: &Peer, _: EffectDone) {}
            fn acknowledge(&mut self, _: &Peer, _: u64) {}
            fn disconnected(&mut self, _: &Peer) {}
            fn shutdown(&mut self) -> Result<(), ServerError> {
                Ok(())
            }
        }
        let root = std::env::temp_dir().canonicalize().unwrap().join(format!(
            "st-restore-{:x}",
            u64::from_be_bytes(protocol::random().unwrap()[..8].try_into().unwrap())
        ));
        let restored = Arc::new(AtomicBool::new(false));
        let mut host: Box<dyn PaneService> = Box::new(HostService {
            inner: Box::new(Restoring(Arc::clone(&restored))),
            paths: Paths {
                identity: "spaceterm-development".into(),
                data: root.join("d"),
                runtime: root.join("r"),
            },
            stop: Arc::new(Stop {
                stopping: AtomicBool::new(false),
                wake: Arc::new(|| {}),
            }),
        });
        let data = Directory::open(&root.join("d"), true).unwrap();
        host.restore(crate::organization::OrganizationStore(data))
            .unwrap();
        assert!(restored.load(Ordering::Acquire));
        let _ = fs::remove_dir_all(root);
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

    /// A test service: `answer` replies to every operation.
    struct Answering<F>(F);
    impl<F: FnMut(Operation) -> Dispatch + Send> PaneService for Answering<F> {
        fn admitted(&mut self, _: &Peer, _: ClientName, _: Outbox) {}
        fn request(&mut self, _: &Peer, _: RequestId, operation: Operation) -> Dispatch {
            (self.0)(operation)
        }
        fn input(&mut self, _: &Peer, _: Input) {}
        fn view(&mut self, _: &Peer, _: View) {}
        fn effect_done(&mut self, _: &Peer, _: EffectDone) {}
        fn acknowledge(&mut self, _: &Peer, _: u64) {}
        fn disconnected(&mut self, _: &Peer) {}
        fn shutdown(&mut self) -> Result<(), ServerError> {
            Ok(())
        }
    }

    fn connect_when_ready(fixture: &Fixture) -> UnixStream {
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            match UnixStream::connect(fixture.paths.socket()) {
                Ok(stream) => return stream,
                Err(_) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(5)),
                Err(error) => panic!("server did not start: {error}"),
            }
        }
    }

    fn admitted_link(fixture: &Fixture, device: &[u8; 32]) -> crate::connection::link::ServerLink {
        let mut stream = connect_when_ready(fixture);
        protocol::admit_client(
            &mut stream,
            (device, &"Test".into()),
            &fixture.pin,
            "spaceterm-development",
            &[protocol::MAJOR],
        )
        .unwrap();
        link(stream, Arc::new(|_| {})).unwrap()
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
        impl PaneService for Panes {
            fn admitted(&mut self, _: &Peer, _: ClientName, _: Outbox) {}
            fn request(&mut self, _: &Peer, _: RequestId, _: Operation) -> Dispatch {
                Dispatch::reply(Ok(crate::protocol::ReplyBody::Done))
            }
            fn input(&mut self, _: &Peer, _: Input) {}
            fn view(&mut self, _: &Peer, _: View) {}
            fn effect_done(&mut self, _: &Peer, _: EffectDone) {}
            fn acknowledge(&mut self, _: &Peer, _: u64) {}
            fn disconnected(&mut self, _: &Peer) {}
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
            crate::panes({
                let root = root.clone();
                move |_| Ok(Panes(root))
            }),
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
            &[protocol::MAJOR],
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
        use crate::protocol::ReplyBody;
        use std::sync::atomic::AtomicUsize;
        let root = PathBuf::from(std::env::var_os(CHILD_ROOT).expect("startup fixture root"));
        let late_requests = Arc::new(AtomicUsize::new(0));
        let late = Arc::clone(&late_requests);
        let mut stopped = false;
        serve_with_panes(
            &Paths {
                identity: "spaceterm-development".into(),
                data: root.join("d"),
                runtime: root.join("r"),
            },
            // Like a real service, this one keeps the shared sync worker until it is dropped.
            Some(crate::panes(move |sync| {
                Ok(Answering(move |operation| {
                    let _ = &sync;
                    if stopped {
                        late.fetch_add(1, Ordering::SeqCst);
                    }
                    stopped |= matches!(operation, Operation::Shutdown);
                    Dispatch::reply(Ok(ReplyBody::Done))
                }))
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

    /// Defers every `Resolve` until the host stops, then refuses it.
    #[derive(Default)]
    struct Deferring {
        outboxes: std::collections::BTreeMap<[u8; 32], Outbox>,
        owed: Vec<([u8; 32], RequestId)>,
    }
    impl PaneService for Deferring {
        fn admitted(&mut self, peer: &Peer, _: ClientName, outbox: Outbox) {
            self.outboxes.insert(peer.connection, outbox);
        }
        fn request(&mut self, peer: &Peer, id: RequestId, operation: Operation) -> Dispatch {
            if matches!(operation, Operation::Resolve { .. }) {
                self.owed.push((peer.connection, id));
                return Dispatch::Deferred;
            }
            Dispatch::reply(Ok(crate::protocol::ReplyBody::Done))
        }
        fn input(&mut self, _: &Peer, _: Input) {}
        fn view(&mut self, _: &Peer, _: View) {}
        fn effect_done(&mut self, _: &Peer, _: EffectDone) {}
        fn acknowledge(&mut self, _: &Peer, _: u64) {}
        fn stopping(&mut self) {
            for (connection, id) in self.owed.drain(..) {
                let _ = self.outboxes[&connection].reply(id, Err(Refusal::Failed));
            }
        }
        fn disconnected(&mut self, _: &Peer) {}
        fn shutdown(&mut self) -> Result<(), ServerError> {
            Ok(())
        }
    }

    #[test]
    #[ignore = "runs only as a child of the owed reply shutdown test"]
    fn owed_reply_shutdown_child() {
        let root = PathBuf::from(std::env::var_os(CHILD_ROOT).expect("startup fixture root"));
        serve_with_panes(
            &Paths {
                identity: "spaceterm-development".into(),
                data: root.join("d"),
                runtime: root.join("r"),
            },
            Some(crate::panes(|_| Ok(Deferring::default()))),
        )
        .unwrap();
        fs::write(root.join("stopped"), []).unwrap();
    }

    #[test]
    fn shutdown_refuses_owed_replies_instead_of_waiting_for_them() {
        use crate::connection::link::RequestError;
        let fixture = Fixture::new();
        let mut child = fixture.start_test("host::startup_tests::owed_reply_shutdown_child", false);
        let waiting = admitted_link(&fixture, &fixture.device);
        let owed = waiting.request(Operation::Resolve { sequence: 1 }).unwrap();
        // The deferred request reaches the service before shutdown begins.
        assert!(matches!(
            waiting.request(round_trip()).unwrap().wait(),
            Ok(crate::protocol::ReplyBody::Done)
        ));
        let shutdown = admitted_link(&fixture, &fixture.device);
        assert!(matches!(
            shutdown.request(Operation::Shutdown).unwrap().wait(),
            Ok(crate::protocol::ReplyBody::Done)
        ));
        let deadline = Instant::now() + Duration::from_secs(3);
        while !fixture.root.join("stopped").exists() {
            assert!(
                Instant::now() < deadline,
                "shutdown waited for a reply nothing would send"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(
            owed.wait().unwrap_err(),
            RequestError::Refused(Refusal::Failed)
        );
        assert!(child.0.wait().unwrap().success());
    }

    /// A request the test service answers at once.
    fn round_trip() -> Operation {
        Operation::ReleaseAttachment {
            attachment: crate::attachments::Attachment {
                workspace: [2; 32],
                generation: 1,
            },
        }
    }

    #[test]
    #[ignore = "runs only as a child of the shared transport admission test"]
    fn shared_transport_child() {
        use crate::protocol::{
            ClaimReply, ReplyBody, Restored,
            interim::{Interim, InterimReply},
        };
        let root = PathBuf::from(std::env::var_os(CHILD_ROOT).expect("startup fixture root"));
        serve_with_panes(
            &Paths {
                identity: "spaceterm-development".into(),
                data: root.join("d"),
                runtime: root.join("r"),
            },
            Some(crate::panes(|_| {
                Ok(Answering(|operation| {
                    Dispatch::reply(Ok(match operation {
                        Operation::Claim(claim) => {
                            ReplyBody::Claim(ClaimReply::Restored(Box::new(Restored {
                                fresh: false,
                                session_ended: false,
                                recovery: None,
                                directory_unavailable: false,
                                server: [1; 32],
                                workspace: [2; 32],
                                tab: claim.pane,
                                pane: claim.pane,
                            })))
                        }
                        Operation::Graphics { image, offset, .. } => {
                            ReplyBody::Graphics(Ok(crate::pane::graphics::Chunk {
                                image,
                                offset: offset as usize,
                                bytes: vec![255; crate::pane::graphics::CHUNK_BYTES],
                            }))
                        }
                        Operation::Interim(Interim::SelectionChunk {
                            generation, offset, ..
                        }) => ReplyBody::Interim(InterimReply::SelectionChunk {
                            generation,
                            offset,
                            total: 32 * crate::pane::reading::SELECTION_CHUNK_BYTES as u64,
                            bytes: vec![65; crate::pane::reading::SELECTION_CHUNK_BYTES],
                        }),
                        _ => ReplyBody::Done,
                    }))
                }))
            })),
        )
        .unwrap();
    }

    #[test]
    fn a_client_that_stops_reading_bulk_replies_does_not_stall_another_client() {
        use crate::protocol::{
            Channel, ClientMessage, ReplyBody, Request, RequestId, frame::encode_client,
            interim::Interim,
        };
        use std::io::Write;
        for graphics in [false, true] {
            let fixture = Fixture::new();
            let _child = fixture.start_test("host::startup_tests::shared_transport_child", false);
            let mut slow = connect_when_ready(&fixture);
            protocol::admit_client(
                &mut slow,
                (&fixture.device, &"Test".into()),
                &fixture.pin,
                "spaceterm-development",
                &[protocol::MAJOR],
            )
            .unwrap();
            // Replies exceed socket buffering. This admitted client deliberately reads none.
            for index in 0..16u32 {
                let operation = if graphics {
                    Operation::Graphics {
                        channel: Channel(1),
                        image: crate::pane::graphics::ImageKey {
                            image_id: 1,
                            generation: 1,
                        },
                        offset: u64::from(index) * crate::pane::graphics::CHUNK_BYTES as u64,
                    }
                } else {
                    Operation::Interim(Interim::SelectionChunk {
                        channel: Channel(1),
                        generation: 1,
                        offset: u64::from(index)
                            * crate::pane::reading::SELECTION_CHUNK_BYTES as u64,
                    })
                };
                let message = ClientMessage::Request(Box::new(Request {
                    id: RequestId(index + 1),
                    operation,
                }));
                for frame in encode_client(&message).unwrap().frames {
                    slow.write_all(&frame).unwrap();
                }
            }
            let current = admitted_link(&fixture, &fixture.device);
            let started = Instant::now();
            for _ in 0..16 {
                let reply = current.request(round_trip()).unwrap().wait();
                assert!(matches!(reply, Ok(ReplyBody::Done)));
            }
            assert!(
                started.elapsed() < Duration::from_secs(2),
                "a slow reader blocked another connection"
            );
        }
    }

    #[test]
    fn two_desktops_claim_every_supported_pane_over_one_connection_each() {
        use crate::pane::{AttachmentClaim, PaneSize};
        use crate::protocol::{ClaimReply, ReplyBody};
        let fixture = Fixture::new();
        let other = protocol::random().unwrap();
        let _child = fixture.start_test("host::startup_tests::shared_transport_child", false);
        let mut links = Vec::new();
        for device in [fixture.device, other] {
            let link = admitted_link(&fixture, &device);
            assert!(matches!(
                link.request(round_trip()).unwrap().wait(),
                Ok(ReplyBody::Done)
            ));
            for pane in 0..crate::hierarchy::MAX_PANES {
                let channel = link.open_channel(Arc::new(|_| {})).unwrap();
                let id = [pane as u8; 32];
                let reply = channel
                    .claim(
                        crate::attachments::Attachment {
                            workspace: [2; 32],
                            generation: 1,
                        },
                        id,
                        AttachmentClaim::Select { explicit: false },
                        PaneSize {
                            columns: 80,
                            rows: 24,
                            cell_width: 8,
                            cell_height: 18,
                        },
                        None,
                    )
                    .unwrap()
                    .wait();
                assert!(matches!(
                    reply,
                    Ok(ReplyBody::Claim(ClaimReply::Restored(restored))) if restored.pane == id
                ));
            }
            links.push(link);
        }
        // Both desktops stay connected while management admits independently.
        let management = admitted_link(&fixture, &fixture.device);
        for link in &links {
            assert!(matches!(
                link.request(round_trip()).unwrap().wait(),
                Ok(ReplyBody::Done)
            ));
        }
        assert!(matches!(
            management.request(Operation::Shutdown).unwrap().wait(),
            Ok(ReplyBody::Done)
        ));
    }

    #[test]
    fn shutdown_refuses_a_request_already_waiting_for_its_frame() {
        use crate::protocol::{
            ClientMessage, Fatal, RawFrame, ReplyBody, Request, RequestId, ServerMessage,
            frame::{Assembler, FrameReader, encode_client},
        };
        use std::io::{Read, Write};
        let fixture = Fixture::new();
        let mut child = fixture.start_test("host::startup_tests::server_shutdown_child", false);
        let mut pending = connect_when_ready(&fixture);
        protocol::admit_client(
            &mut pending,
            (&fixture.device, &"Test".into()),
            &fixture.pin,
            "spaceterm-development",
            &[protocol::MAJOR],
        )
        .unwrap();
        let frame = encode_client(&ClientMessage::Request(Box::new(Request {
            id: RequestId(1),
            operation: round_trip(),
        })))
        .unwrap()
        .frames
        .remove(0);
        pending.write_all(&frame[..3]).unwrap();
        // The admitted connection waits on the incomplete frame while another shuts down.
        std::thread::sleep(Duration::from_millis(50));
        let shutdown = admitted_link(&fixture, &fixture.device);
        assert!(matches!(
            shutdown.request(Operation::Shutdown).unwrap().wait(),
            Ok(ReplyBody::Done)
        ));
        let deadline = Instant::now() + Duration::from_secs(3);
        while !fixture.root.join("stopped").exists() {
            assert!(Instant::now() < deadline, "server did not finish shutdown");
            std::thread::sleep(Duration::from_millis(10));
        }
        let _ = pending.write_all(&frame[3..]);
        let mut received = Vec::new();
        let _ = pending.read_to_end(&mut received);
        let mut reader = FrameReader::default();
        reader.feed(&received);
        let mut assembler = Assembler::default();
        let mut messages = Vec::new();
        while let Some(frame) = reader.next_frame().unwrap() {
            messages.extend(
                assembler
                    .server(
                        RawFrame::parse(&{
                            let mut bytes = ((frame.body.len() + 1) as u32).to_be_bytes().to_vec();
                            bytes.push(frame.kind);
                            bytes.extend_from_slice(&frame.body);
                            bytes
                        })
                        .unwrap(),
                    )
                    .unwrap(),
            );
        }
        assert!(
            matches!(
                messages.as_slice(),
                [] | [ServerMessage::Goodbye(Fatal::Stopping)]
            ),
            "the server answered a request after shutdown"
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
                &[protocol::MAJOR]
            ),
            Ok(crate::AuthenticatedStatus {
                protocol_major: protocol::MAJOR,
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
