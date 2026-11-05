//! The account-local host: one listener per server, one serving thread per admitted connection,
//! and one sync worker. The accept loop waits on its listeners, a wake pipe and the service's next
//! tick; it never sleeps.
use crate::connection::Wake;
use crate::connection::admitted::AdmittedConnection;
use crate::connection::io::Waker;
use crate::connection::outbox::Outbox;
use crate::connection::service::{
    DirectManagement, Dispatch, PaneService, Peer, SharedPaneService,
};
use crate::connection::sync::SyncWorker;
use crate::connection::unix::UnixCarrier;
use crate::pane::control::ClientName;
use crate::protocol::{EffectDone, Input, Operation, Refusal, RequestId, View};
use crate::{CoreError, protocol, storage::Directory};
use spaceterm_directories::CoreDirectories;
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
pub(crate) fn install_shutdown_signals() -> Result<(), CoreError> {
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
            return Err(CoreError::StartupFailed);
        }
        // Desktop background threads can block these signals. Installing a handler does not
        // change the mask inherited through exec, so the server must unblock its shutdown signals.
        let mut shutdown: libc::sigset_t = std::mem::zeroed();
        libc::sigemptyset(&mut shutdown);
        libc::sigaddset(&mut shutdown, libc::SIGTERM);
        libc::sigaddset(&mut shutdown, libc::SIGINT);
        if libc::pthread_sigmask(libc::SIG_UNBLOCK, &shutdown, std::ptr::null_mut()) != 0 {
            return Err(CoreError::StartupFailed);
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
    pub fn for_identity(identity: &str) -> Result<Self, CoreError> {
        // SAFETY: geteuid has no preconditions.
        if unsafe { libc::geteuid() } == 0 {
            return Err(CoreError::UnsafeStorage);
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
            return Err(CoreError::StorageUnavailable);
        }
        // SAFETY: successful getpwuid_r stores a terminated string within buffer, retained here.
        let home = unsafe { CStr::from_ptr(record.pw_dir) };
        let home = PathBuf::from(std::ffi::OsStr::from_bytes(home.to_bytes()));
        // SAFETY: geteuid has no preconditions.
        let directories = CoreDirectories::for_account(&home, unsafe { libc::geteuid() }, identity)
            .map_err(|_| CoreError::StorageUnavailable)?;
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

pub(crate) fn check_peer(stream: &UnixStream) -> Result<(), CoreError> {
    #[cfg(target_os = "macos")]
    let user = {
        let mut user = 0;
        let mut group = 0;
        // SAFETY: getpeereid writes two uid/gid outputs for the retained socket.
        if unsafe { libc::getpeereid(stream.as_raw_fd(), &mut user, &mut group) } != 0 {
            return Err(CoreError::AuthenticationRefused);
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
            return Err(CoreError::AuthenticationRefused);
        }
        credentials.uid
    };
    check_account(user)
}
fn check_account(user: libc::uid_t) -> Result<(), CoreError> {
    // SAFETY: geteuid has no preconditions.
    if user != unsafe { libc::geteuid() } {
        return Err(CoreError::AuthenticationRefused);
    }
    Ok(())
}

pub(crate) fn connect(paths: &Paths) -> Result<UnixStream, CoreError> {
    let directory = Directory::open(&paths.runtime, false).map_err(|error| {
        if error == CoreError::NotSetUp {
            CoreError::NotRunning
        } else {
            error
        }
    })?;
    directory.verify_path(&paths.runtime)?;
    let endpoint = directory
        .socket_identity(c"s")?
        .ok_or(CoreError::NotRunning)?;
    let stream = connect_socket(&paths.socket()).map_err(|_| CoreError::NotRunning)?;
    directory.verify_path(&paths.runtime)?;
    if directory.socket_identity(c"s")? != Some(endpoint) {
        return Err(CoreError::UnsafeStorage);
    }
    check_peer(&stream)?;
    Ok(stream)
}

/// Starts a client link on an admitted account-local socket.
pub(crate) fn link(
    stream: UnixStream,
    events: Arc<dyn crate::connection::link::LinkSink>,
) -> Result<crate::connection::link::CoreLink, CoreError> {
    let carrier = UnixCarrier::new(stream).map_err(|_| CoreError::Protocol)?;
    crate::connection::io::spawn_link(Box::new(carrier), events).map_err(|_| CoreError::Protocol)
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

pub(crate) fn serve_with_panes(
    paths: &Paths,
    panes: Option<crate::PaneFactory>,
) -> Result<(), CoreError> {
    serve_runtime(paths, panes, false)
}

pub(crate) fn serve_runtime(
    paths: &Paths,
    panes: Option<crate::PaneFactory>,
    supervised: bool,
) -> Result<(), CoreError> {
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
    /// Answers service-lane connections. `None` refuses them as unsupported.
    pub(crate) lane: Option<Arc<crate::service::lane::LaneCore>>,
    /// This Core's update engine, whose facts every work connection receives. `None` when the
    /// engine did not open, as without service state: work connections then receive none.
    pub(crate) facts: Option<Arc<dyn crate::update::source::FactsSource>>,
    /// The versions admission offers.
    pub(crate) versions: protocol::CoreVersions,
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
            lane: None,
            facts: None,
            versions: protocol::CoreVersions::current(
                crate::update::facts::Version::parse("1.4.0").unwrap_or_else(|_| unreachable!()),
            ),
        }
    }
}

/// The host's own rules around the application's service: a Shutdown first retains Stop intent,
/// and a completed one stops the host.
struct HostService {
    inner: Box<dyn PaneService>,
    paths: Paths,
    stop: Arc<Stop>,
    /// The membership fence: set while an admitted service operation is unsettled.
    fence: Arc<AtomicBool>,
    /// The connection that fenced Session starts for a local uninstall, until it closes.
    uninstalling: Option<[u8; 32]>,
}
impl PaneService for HostService {
    fn restore(&mut self, store: crate::organization::OrganizationStore) -> Result<(), CoreError> {
        self.inner.restore(store)
    }
    fn admitted(&mut self, peer: &Peer, name: ClientName, outbox: Outbox) {
        self.inner.admitted(peer, name, outbox)
    }
    fn revoked(&mut self, client: [u8; 32]) {
        self.inner.revoked(client)
    }
    fn release_control(&mut self, client: [u8; 32]) {
        self.inner.release_control(client)
    }
    fn holds(&self, client: [u8; 32]) -> bool {
        self.inner.holds(client)
    }
    fn sessions(&self) -> Vec<[u8; 32]> {
        self.inner.sessions()
    }
    fn request(&mut self, peer: &Peer, id: RequestId, operation: Operation) -> Dispatch {
        // The fence and its check share this service's lock, so no Session starts between an
        // admitted operation's observation and its outcome.
        if let Operation::Sessions { fence } = operation {
            if fence {
                self.uninstalling = Some(peer.connection);
            }
            return Dispatch::reply(Ok(crate::protocol::ReplyBody::Sessions(
                self.inner.sessions(),
            )));
        }
        if (self.fence.load(Ordering::Acquire) || self.uninstalling.is_some())
            && matches!(operation, Operation::Claim(_))
        {
            return Dispatch::reply(Err(Refusal::ServiceBusy));
        }
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
    fn publish(&mut self, peer: &Peer) {
        self.inner.publish(peer)
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
        if self.uninstalling == Some(peer.connection) {
            self.uninstalling = None;
        }
        self.inner.disconnected(peer)
    }
    fn shutdown(&mut self) -> Result<(), CoreError> {
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
    fn shutdown(&mut self) -> Result<(), CoreError> {
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
        if !retired.is_empty() {
            let mut panes = self.panes.lock().unwrap_or_else(|e| e.into_inner());
            for client in retired {
                panes.revoked(client);
            }
        }
        Ok(reply)
    }
}

/// The lane's view of this host: its Terminal Sessions, its membership fence and its paired
/// devices.
struct HostLane {
    panes: SharedPaneService,
    counters: Arc<crate::diagnostics::TerminalCounters>,
    fence: Arc<AtomicBool>,
    direct: Option<crate::direct::host::Shared>,
    stop: Arc<Stop>,
}
impl HostLane {
    fn manage(
        &self,
        request: crate::direct::Management,
    ) -> Result<Option<crate::direct::ManagementReply>, CoreError> {
        let Some(state) = &self.direct else {
            return Ok(None);
        };
        let (reply, _) = state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .manage(None, request)?;
        Ok(Some(reply))
    }
}
impl crate::service::lane::LaneHost for HostLane {
    fn fence(
        &self,
        admit: &mut dyn FnMut(&[[u8; 32]]) -> Result<(), CoreError>,
    ) -> Result<(), CoreError> {
        let panes = self.panes.lock().unwrap_or_else(|e| e.into_inner());
        admit(&panes.sessions())?;
        self.fence.store(true, Ordering::Release);
        Ok(())
    }
    fn release_fence(&self) {
        let _panes = self.panes.lock().unwrap_or_else(|e| e.into_inner());
        self.fence.store(false, Ordering::Release);
    }
    fn stopping(&self) -> bool {
        self.stop.stopping()
    }
    fn sessions(&self) -> Vec<[u8; 32]> {
        self.panes
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .sessions()
    }
    fn terminal_counters(&self) -> crate::diagnostics::TerminalCounterSnapshot {
        self.counters.snapshot()
    }
    fn devices(&self) -> Result<Vec<crate::service::lane::LaneDevice>, CoreError> {
        let Some(crate::direct::ManagementReply::Devices(devices)) =
            self.manage(crate::direct::Management::Devices)?
        else {
            return Ok(Vec::new());
        };
        Ok(devices.iter().map(crate::service::lane::device).collect())
    }
    fn change_network(
        &self,
        access: crate::service::lane::network::NetworkAccess,
        caller: Option<&crate::service::lane::network::NetworkCaller>,
        decide: &mut dyn FnMut(
            &crate::service::lane::network::NetworkPlan,
        ) -> Result<bool, CoreError>,
    ) -> Result<Option<crate::service::lane::network::ListenerView>, CoreError> {
        let state = self.direct.as_ref().ok_or(CoreError::Unsupported)?;
        crate::direct::host::change_network(state, access, caller, decide)
    }
    fn listener(&self) -> Result<crate::service::lane::network::ListenerView, CoreError> {
        let state = self.direct.as_ref().ok_or(CoreError::Unsupported)?;
        Ok(crate::direct::host::listener(state))
    }
    fn network(&self) -> Result<crate::service::lane::LaneNetwork, CoreError> {
        let off = crate::service::lane::LaneNetwork {
            enabled: false,
            bind: None,
            listening: false,
            addresses: Vec::new(),
        };
        let Some(crate::direct::ManagementReply::Status(status)) =
            self.manage(crate::direct::Management::Status)?
        else {
            return Ok(off);
        };
        Ok(crate::service::lane::LaneNetwork {
            enabled: status.enabled,
            bind: status.policy.as_ref().map(|policy| policy.bind.to_string()),
            listening: matches!(
                status.listener,
                crate::direct::ListenerState::Listening { .. }
            ),
            addresses: status
                .network_addresses
                .map(|addresses| addresses.available)
                .unwrap_or_default(),
        })
    }
}

/// Every revocation, over IPC or the network, forgets the revoked pairing's settled service
/// operations in the ledger under `data`, so they never hold its capacity. An unsettled one stays
/// orphaned until it settles.
pub(crate) fn forget_service_operations(data: std::path::PathBuf) -> crate::direct::host::Revoked {
    Box::new(move |pairing| {
        if let Ok(data) = Directory::open(&data, false) {
            let _ = crate::service::operations::Ledger::in_directory(data).forget_pairing(pairing);
        }
    })
}

/// One serving thread and what ends it: its socket's read side and its I/O thread's wake pipe.
pub(crate) struct Running {
    thread: std::thread::JoinHandle<()>,
    /// Set by the thread just before it rings the host's bell and ends.
    done: Arc<AtomicBool>,
    sever: Box<dyn FnOnce() + Send>,
    wake: Arc<dyn Wake>,
}
impl Running {
    /// A connection still in admission sees its socket close. An admitted one stops reading,
    /// writes its pending replies and says goodbye.
    pub(crate) fn stop(self) -> std::thread::JoinHandle<()> {
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
    ready: impl FnOnce() -> Result<(), CoreError>,
) -> Result<(), CoreError> {
    raise_descriptor_limit();
    crate::service::check_runtime(paths, supervised)?;
    // The data reaches this runtime's generation before anything here reads it.
    crate::migration::core_data::prepare(paths)?;
    let data = Directory::open(&paths.data, false)?;
    // Verify retained identity before changing any runtime state. Never generate a replacement.
    // The identity never changes while this host runs, so admission reuses it.
    let core = data.read()?.core;
    let runtime = Directory::open(&paths.runtime, true)?;
    let _lock = runtime.lock("server.lock", true)?;
    crate::service::check_runtime(paths, supervised)?;
    let (bell, ring) = UnixStream::pair().map_err(|_| CoreError::StartupFailed)?;
    for end in [&bell, &ring] {
        end.set_nonblocking(true)
            .map_err(|_| CoreError::StartupFailed)?;
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
    let (sync, sync_thread) = SyncWorker::spawn().map_err(|_| CoreError::StartupFailed)?;
    let (inner, counters, version) = match panes {
        Some(factory) => (
            (factory.build)(sync.clone())?,
            factory.counters,
            factory.version,
        ),
        // A host without Panes runs no Terminal Session, so nothing records into its counters.
        None => (
            Box::new(NoPanes) as Box<dyn PaneService>,
            Arc::default(),
            None,
        ),
    };
    let version = match version {
        Some(version) => version,
        None => crate::update::facts::Version::parse(crate::UNVERSIONED)
            .map_err(|_| CoreError::StartupFailed)?,
    };
    let versions = protocol::CoreVersions::current(version);
    let fence = Arc::new(AtomicBool::new(false));
    let mut panes: Box<dyn PaneService> = Box::new(HostService {
        inner,
        paths: paths.clone(),
        stop: Arc::clone(&stop),
        fence: Arc::clone(&fence),
        uninstalling: None,
    });
    {
        // A Git write can outlive a crashed server through its inherited lock. Restore only
        // after that original child exits, before publishing an endpoint or admitting work.
        let _writes = data.lock("worktree.lock", false)?;
        panes.restore(crate::organization::OrganizationStore(Directory::open(
            &paths.data,
            false,
        )?))?;
    }
    panes.ticker(Arc::clone(&stop.wake));
    let socket = paths.socket();
    if let Some(identity) = runtime.socket_identity(c"s")? {
        runtime.verify_path(&paths.runtime)?;
        // Only a definite refusal proves staleness. Answering listeners and uncertain failures
        // remain untouched, even when their instance lock is missing or no longer held.
        if !matches!(connect_socket(&socket), Err(SocketUnavailable::Refused)) {
            return Err(CoreError::StorageUnavailable);
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
            return Err(CoreError::StorageUnavailable);
        }
        runtime.verify_path(&paths.runtime)?;
        runtime.remove_socket(c"p", identity)?;
    }
    let listener = UnixListener::bind(&pending).map_err(|_| CoreError::StorageUnavailable)?;
    fs::set_permissions(&pending, fs::Permissions::from_mode(0o600))
        .map_err(|_| CoreError::StorageUnavailable)?;
    let socket_identity = runtime
        .socket_identity(c"p")?
        .ok_or(CoreError::UnsafeStorage)?;
    runtime.verify_path(&paths.runtime)?;
    if supervised {
        crate::service::activation::ready(paths)?;
    }
    runtime.publish_socket(c"p", socket_identity)?;
    listener
        .set_nonblocking(true)
        .map_err(|_| CoreError::Protocol)?;
    let panes: SharedPaneService = Arc::new(Mutex::new(panes));
    // A failed direct-network store never prevents owning-account admission.
    let direct = crate::direct::host::State::open(paths).ok();
    if let Some(state) = &direct {
        let mut state = state.lock().unwrap_or_else(|e| e.into_inner());
        state.set_wake(Arc::clone(&stop.wake));
        state.on_revoke(forget_service_operations(paths.data.clone()));
    }
    // The lane settles what an earlier run left in the service-operation ledger before any
    // connection is accepted. A server without service state answers no lane.
    let host_lane = Arc::new(HostLane {
        panes: Arc::clone(&panes),
        counters,
        fence,
        direct: direct.clone(),
        stop: Arc::clone(&stop),
    });
    // One update engine per Core: the lane answers from it, and the watcher below drives it.
    let updates = crate::service::Service::for_paths(paths.clone())
        .and_then(crate::service::updates::Updates::open);
    let lane =
        crate::service::lane::LaneCore::start(paths, Arc::clone(&host_lane) as _, updates.clone())
            .ok();
    let feed = lane.as_ref().and_then(|lane| lane.feed_check());
    // The lane holds the pane service, whose shutdown the server waits for, so the update
    // watcher reaches it only weakly.
    let watched = Arc::downgrade(&host_lane);
    drop(host_lane);
    let restarter = lane.as_ref().map(|lane| {
        let restarter: Arc<dyn crate::service::updates::Restarter> = Arc::clone(lane) as _;
        Arc::downgrade(&restarter)
    });
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
        lane,
        facts: updates.as_ref().ok().map(|updates| {
            Arc::new(updates.clone()) as Arc<dyn crate::update::source::FactsSource>
        }),
        versions,
    };
    SIGNAL_WAKE.store(ring.as_raw_fd(), Ordering::Release);
    let readiness = ready();
    if readiness.is_err() {
        stop.request();
    }
    // Once ready, the engine follows the live Terminal Session count, so a Pending Version
    // that is not breaking restarts silently at zero sessions, including right after boot.
    // A Release Core checks its own feed once ready.
    if readiness.is_ok()
        && let (Some(feed), Ok(updates)) = (feed, &updates)
    {
        let host = watched.clone();
        feed.spawn(
            updates.clone(),
            Arc::new(move || host.upgrade().is_none_or(|host| host.stop.stopping())),
        );
    }
    // A supervised Core restarts itself; a Core that fails to read its updates still serves.
    if readiness.is_ok()
        && supervised
        && let (Ok(updates), Some(restarter)) = (&updates, restarter)
    {
        let host = watched;
        // A Pending Version of another work major strands the Paired Devices whose last
        // admitted Hello offered none of its major.
        let paired = direct.clone();
        let stranded = Arc::new(move |major| {
            paired.as_ref().map_or_else(Vec::new, |state| {
                state
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .stranded(major)
            })
        });
        let _ = updates.run(restarter, stranded, move || {
            let host = host.upgrade()?;
            (!host.stop.stopping()).then(|| {
                let sessions = crate::service::lane::LaneHost::sessions(&*host).len();
                u32::try_from(sessions).unwrap_or(u32::MAX)
            })
        });
    }
    let mut connections: Vec<Running> = Vec::new();
    let mut network: Vec<Running> = Vec::new();
    // A listener that failed to accept, for example out of descriptors, rests until a connection
    // ends or the pause passes, instead of waking the loop at once forever.
    let mut local_paused: Option<Instant> = None;
    while !stop.stopping() {
        let deadline = tick(&panes, direct.as_ref());
        // A connection thread rings the bell when it ends, so a full set of network slots waits
        // for that instead of a listener it cannot accept from.
        if reap(&mut connections, &mut network) {
            local_paused = None;
        }
        let now = Instant::now();
        local_paused = local_paused.filter(|until| now < *until);
        let deadline = [deadline, local_paused].into_iter().flatten().min();
        // The QUIC listener's own thread rings the bell when a connection arrives.
        wait_for_work(&bell, local_paused.is_none().then_some(&listener), deadline);
        if stop.stopping() {
            break;
        }
        if reap(&mut connections, &mut network) {
            local_paused = None;
        }
        // The fixed admission limits bound worker allocation; admission has an absolute deadline.
        while network.len() < MAX_NETWORK_CONNECTIONS
            && let Some(state) = &direct
        {
            let accepted = state.lock().unwrap_or_else(|e| e.into_inner()).accept();
            let Some((stream, generation)) = accepted else {
                break;
            };
            match spawn_network(stream, generation, state, &environment) {
                Ok(running) => network.push(running),
                Err(_) => break,
            }
        }
        while local_paused.is_none() {
            let stream = match listener.accept() {
                Ok((stream, _)) => stream,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(error) if accept_retries(&error) => continue,
                // Out of descriptors or memory: the service keeps running and accepts again later.
                Err(_) => {
                    local_paused = Some(Instant::now() + ACCEPT_PAUSE);
                    break;
                }
            };
            if connections.len() >= MAX_LOCAL_CONNECTIONS || check_peer(&stream).is_err() {
                continue;
            }
            if let Ok(running) = spawn_local(stream, core, paths, &environment) {
                connections.push(running);
            }
        }
    }
    stop.request();
    SIGNAL_WAKE.store(-1, Ordering::Release);
    // Connections still waiting to be accepted close in order. A listener closed with them
    // pending resets them on Linux.
    while let Ok((stream, _)) = listener.accept() {
        crate::connection::io::discard_unread(stream.as_raw_fd());
    }
    drop(listener);
    // Ticks no longer run, so replies they would have answered are refused now.
    panes.lock().unwrap_or_else(|e| e.into_inner()).stopping();
    // An admission waiting for an identity that another admission holds is refused, so joining
    // its thread never waits for a reservation.
    if let Some(state) = &direct {
        state.lock().unwrap_or_else(|e| e.into_inner()).stopping();
    }
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
/// Serving threads for network connections, including those still in the QUIC handshake.
const MAX_NETWORK_CONNECTIONS: usize = 32;

/// Runs the service's timed work and revocations found by direct-network recovery, and returns
/// when the service next needs a tick.
fn tick(
    panes: &SharedPaneService,
    direct: Option<&crate::direct::host::Shared>,
) -> Option<Instant> {
    let (retired, listen) = direct
        .map(|state| {
            let mut state = state.lock().unwrap_or_else(|e| e.into_inner());
            let retired = state.pending_releases().unwrap_or_default();
            // A listener waiting for an address binds once the previous one's last connection
            // ended; its thread rings this loop then.
            (retired, state.retry_listen())
        })
        .unwrap_or_default();
    let mut panes = panes.lock().unwrap_or_else(|e| e.into_inner());
    for client in retired {
        panes.revoked(client);
    }
    [panes.tick(Instant::now()), listen]
        .into_iter()
        .flatten()
        .min()
}

/// How long a listener that failed to accept rests unless a connection ends first.
const ACCEPT_PAUSE: std::time::Duration = std::time::Duration::from_secs(1);

/// Whether an accept error concerns only the connection it tried to accept, so the next accept
/// may succeed at once.
pub(crate) fn accept_retries(error: &std::io::Error) -> bool {
    matches!(
        error.kind(),
        std::io::ErrorKind::Interrupted | std::io::ErrorKind::ConnectionAborted
    )
}

/// Forgets the serving threads that ended and reports whether any did.
fn reap(connections: &mut Vec<Running>, network: &mut Vec<Running>) -> bool {
    let before = connections.len() + network.len();
    connections.retain(|running| !running.done.load(Ordering::Acquire));
    network.retain(|running| !running.done.load(Ordering::Acquire));
    connections.len() + network.len() < before
}

/// Raises the soft descriptor limit to the hard one. Each connection holds several descriptors,
/// and launchd starts services with a soft limit of 256.
fn raise_descriptor_limit() {
    let mut limit = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // SAFETY: getrlimit writes one rlimit into the live local.
    if unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut limit) } != 0 {
        return;
    }
    // macOS refuses a soft limit above OPEN_MAX, even when the hard limit is unlimited.
    #[cfg(target_os = "macos")]
    let ceiling = limit.rlim_max.min(10240);
    #[cfg(not(target_os = "macos"))]
    let ceiling = limit.rlim_max;
    if limit.rlim_cur < ceiling {
        limit.rlim_cur = ceiling;
        // SAFETY: setrlimit reads one rlimit from the live local. Failure keeps the old limit.
        unsafe {
            libc::setrlimit(libc::RLIMIT_NOFILE, &limit);
        }
    }
}

/// Waits for a connection on either listener, a wake, or the service's next tick. A paused
/// listener is left out.
fn wait_for_work(bell: &UnixStream, listener: Option<&UnixListener>, deadline: Option<Instant>) {
    let mut descriptors = vec![libc::pollfd {
        fd: bell.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    }];
    if let Some(listener) = listener {
        descriptors.push(libc::pollfd {
            fd: listener.as_raw_fd(),
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

/// Marks a serving thread done and rings the host's bell when it drops, also when the thread
/// panics, so the accept loop frees its slot and may accept again.
struct Ended {
    done: Arc<AtomicBool>,
    wake: Arc<dyn Wake>,
}

impl Drop for Ended {
    fn drop(&mut self) {
        self.done.store(true, Ordering::Release);
        self.wake.wake();
    }
}

fn spawn_local(
    stream: UnixStream,
    core: [u8; 32],
    paths: &Paths,
    environment: &Environment,
) -> Result<Running, CoreError> {
    let (waker, wake) = Waker::new().map_err(|_| CoreError::Protocol)?;
    let cancel = stream.try_clone().map_err(|_| CoreError::Protocol)?;
    let identity = paths.identity.clone();
    let environment = environment.clone();
    let outbox_wake = Arc::clone(&wake);
    let done = Arc::new(AtomicBool::new(false));
    let finished = Arc::clone(&done);
    let thread = std::thread::Builder::new()
        .name("spaceterm-connection".into())
        .spawn(move || {
            let _ended = Ended {
                done: finished,
                wake: Arc::clone(&environment.stop.wake),
            };
            let _ = serve_local(stream, core, &identity, waker, outbox_wake, &environment);
        })
        .map_err(|_| CoreError::Protocol)?;
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
    core: [u8; 32],
    identity: &str,
    waker: Waker,
    wake: Arc<dyn Wake>,
    environment: &Environment,
) -> Result<(), CoreError> {
    let protocol::Admitted {
        client,
        name,
        status,
        ..
    } = match protocol::admit_core(&mut stream, &core, identity, &environment.versions)? {
        protocol::Admission::Work(admitted) => admitted,
        // Account authority never changes while the server runs. Stopping severs this
        // socket's reading, which ends the lane's wait for a request.
        protocol::Admission::Service(admitted) => {
            let lane = environment.lane.as_ref().ok_or(CoreError::Unsupported)?;
            return lane.serve(&mut stream, admitted, None, &|| Ok(()));
        }
    };
    let peer = Peer {
        client,
        connection: protocol::random()?,
        pairing: None,
    };
    let carrier = UnixCarrier::new(stream).map_err(|_| CoreError::Protocol)?;
    serve_admitted(
        Box::new(carrier),
        Served {
            peer,
            name,
            work: status.work,
        },
        Outbox::new(wake),
        waker,
        environment,
    );
    Ok(())
}

/// Serves one QUIC connection. Its handshake and pairing exchange wait under deadlines, and an
/// admitted work connection stops on the wake. Stopping severs an admitted service lane.
pub(crate) fn spawn_network(
    stream: crate::connection::quic::QuicStream,
    generation: [u8; 32],
    state: &crate::direct::host::Shared,
    environment: &Environment,
) -> Result<Running, CoreError> {
    let (waker, wake) = Waker::new().map_err(|_| CoreError::Protocol)?;
    let state = Arc::clone(state);
    let environment = environment.clone();
    let outbox_wake = Arc::clone(&wake);
    let done = Arc::new(AtomicBool::new(false));
    let finished = Arc::clone(&done);
    let lane = crate::direct::host::LaneStop::default();
    let stopping = lane.clone();
    let thread = std::thread::Builder::new()
        .name("spaceterm-network".into())
        .spawn(move || {
            let _ended = Ended {
                done: finished,
                wake: Arc::clone(&environment.stop.wake),
            };
            let _ = crate::direct::host::serve(
                stream,
                generation,
                state,
                &environment,
                waker,
                outbox_wake,
                &stopping,
            );
        })
        .map_err(|_| CoreError::Protocol)?;
    Ok(Running {
        thread,
        done,
        sever: Box::new(move || lane.stop()),
        wake,
    })
}

/// Who an admitted work connection serves, and the work version it selected.
pub(crate) struct Served {
    pub(crate) peer: Peer,
    pub(crate) name: ClientName,
    pub(crate) work: crate::protocol::version::ProtocolVersion,
}

/// How often a work connection's facts follower checks that its connection still runs.
const FOLLOW_RECHECK: std::time::Duration = std::time::Duration::from_secs(1);

/// Sends one work connection the update facts now, then again after every change, until it is
/// dropped. It follows on its own thread, which ends at its next recheck after the drop.
struct FactsFollower {
    ended: Arc<AtomicBool>,
}
impl FactsFollower {
    fn start(source: Arc<dyn crate::update::source::FactsSource>, outbox: Outbox) -> Self {
        let ended = Arc::new(AtomicBool::new(false));
        let mut sent = source.facts();
        let _ = outbox.update_facts(sent.clone());
        let follow = {
            let ended = Arc::clone(&ended);
            move || {
                while !ended.load(Ordering::Acquire) {
                    let facts = source.wait(sent.revision, FOLLOW_RECHECK);
                    if facts.revision == sent.revision || ended.load(Ordering::Acquire) {
                        continue;
                    }
                    if outbox.update_facts(facts.clone()).is_err() {
                        return;
                    }
                    sent = facts;
                }
            }
        };
        let _ = std::thread::Builder::new()
            .name("spaceterm-core-facts".into())
            .spawn(follow);
        Self { ended }
    }
}
impl Drop for FactsFollower {
    fn drop(&mut self) {
        self.ended.store(true, Ordering::Release);
    }
}

/// Serves one admitted connection on this thread until it ends. When its selected version has
/// [`crate::protocol::capability::Capability::UpdateFacts`], its first message is the update
/// facts, and it receives them again after every change.
pub(crate) fn serve_admitted(
    carrier: Box<dyn crate::connection::carrier::Carrier>,
    Served { peer, name, work }: Served,
    outbox: Outbox,
    waker: Waker,
    environment: &Environment,
) {
    use crate::protocol::capability::{Capabilities, Capability, Unavailable};
    let receives_facts = Capabilities::new(Ok(work), Err(Unavailable::NotConnected))
        .check(Capability::UpdateFacts)
        .is_ok();
    let _facts = environment
        .facts
        .as_ref()
        .filter(|_| receives_facts)
        .map(|source| FactsFollower::start(Arc::clone(source), outbox.clone()));
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
    ready: impl FnOnce() -> Result<(), CoreError>,
) -> Result<(), CoreError> {
    install_shutdown_signals()?;
    crate::service::check_runtime(paths, false)?;
    let data = Directory::open(&paths.data, true)?;
    crate::migration::core_data::prepare(paths)?;
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
            fn shutdown(&mut self) -> Result<(), CoreError> {
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
            let (client, core) = UnixStream::pair().unwrap();
            let (waker, wake) = Waker::new().unwrap();
            wakes.push(Arc::clone(&wake));
            let environment = environment.clone();
            threads.push(std::thread::spawn(move || {
                serve_admitted(
                    Box::new(UnixCarrier::new(core).unwrap()),
                    Served {
                        peer: Peer {
                            client: [1; 32],
                            connection: [connection; 32],
                            pairing: None,
                        },
                        name: "Test".into(),
                        work: crate::protocol::version::WORK,
                    },
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
            fn shutdown(&mut self) -> Result<(), CoreError> {
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
        let (client, core) = UnixStream::pair().unwrap();
        let (waker, wake) = Waker::new().unwrap();
        let host_wake = Arc::clone(&wake);
        let serving = {
            let environment = environment.clone();
            std::thread::spawn(move || {
                serve_admitted(
                    Box::new(UnixCarrier::new(core).unwrap()),
                    Served {
                        peer: Peer {
                            client: [1; 32],
                            connection: [1; 32],
                            pairing: None,
                        },
                        name: "Test".into(),
                        work: crate::protocol::version::WORK,
                    },
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

    /// One work connection served as `serve_admitted` serves it, from a host whose service
    /// pushes an inventory at admission and whose update facts the test changes by hand.
    mod facts_delivery {
        use super::*;
        use crate::connection::link::{CoreLink, LinkEvent};
        use crate::protocol::InventoryRevision;
        use crate::protocol::version::ProtocolVersion;
        use crate::update::facts::{Platform, UpdateCheck, UpdateFacts, Version};
        use crate::update::source::FactsSource;

        /// Pushes an inventory at admission.
        struct Inventory;
        impl PaneService for Inventory {
            fn admitted(&mut self, _: &Peer, _: ClientName, outbox: Outbox) {
                outbox
                    .inventory(InventoryRevision {
                        inventory: crate::hierarchy::State::empty().inventory,
                        next_sequence: 1,
                    })
                    .unwrap();
            }
            fn request(&mut self, _: &Peer, _: RequestId, _: Operation) -> Dispatch {
                Dispatch::reply(Ok(crate::protocol::ReplyBody::Done))
            }
            fn sessions(&self) -> Vec<[u8; 32]> {
                Vec::new()
            }
            fn input(&mut self, _: &Peer, _: Input) {}
            fn view(&mut self, _: &Peer, _: View) {}
            fn effect_done(&mut self, _: &Peer, _: EffectDone) {}
            fn acknowledge(&mut self, _: &Peer, _: u64) {}
            fn disconnected(&mut self, _: &Peer) {}
            fn shutdown(&mut self) -> Result<(), CoreError> {
                Ok(())
            }
        }

        /// Facts the test changes by hand.
        pub(super) struct Board(Mutex<UpdateFacts>, std::sync::Condvar);
        impl Board {
            fn new() -> Arc<Self> {
                Arc::new(Self(
                    Mutex::new(UpdateFacts {
                        revision: 1,
                        platform: Platform::LinuxX86_64,
                        running: Version::parse("1.4.0").unwrap(),
                        pending: None,
                        sessions: 0,
                        operation: None,
                        check: UpdateCheck::Never,
                        skipped: None,
                        last_failure: None,
                    }),
                    std::sync::Condvar::new(),
                ))
            }
            fn change(&self, revision: u64, sessions: u32) {
                let mut facts = self.0.lock().unwrap();
                facts.revision = revision;
                facts.sessions = sessions;
                self.1.notify_all();
            }
        }
        impl FactsSource for Board {
            fn facts(&self) -> UpdateFacts {
                self.0.lock().unwrap().clone()
            }
            fn wait(&self, revision: u64, timeout: std::time::Duration) -> UpdateFacts {
                let facts = self.0.lock().unwrap();
                let (facts, _) = self
                    .1
                    .wait_timeout_while(facts, timeout, |facts| facts.revision == revision)
                    .unwrap();
                facts.clone()
            }
        }

        /// The (revision, sessions) of each update facts message, and `None` for each inventory,
        /// in arrival order.
        type Events = Arc<Mutex<Vec<Option<(u64, u32)>>>>;

        /// One connection at selected work version `work`.
        struct Connection {
            link: CoreLink,
            events: Events,
            serving: std::thread::JoinHandle<()>,
        }
        impl Connection {
            fn open(environment: &Environment, work: ProtocolVersion) -> Self {
                let (client, core) = UnixStream::pair().unwrap();
                let (waker, wake) = Waker::new().unwrap();
                let serving = {
                    let environment = environment.clone();
                    std::thread::spawn(move || {
                        serve_admitted(
                            Box::new(UnixCarrier::new(core).unwrap()),
                            Served {
                                peer: Peer {
                                    client: [1; 32],
                                    connection: [1; 32],
                                    pairing: None,
                                },
                                name: "Test".into(),
                                work,
                            },
                            Outbox::new(wake),
                            waker,
                            &environment,
                        );
                    })
                };
                let events = Events::default();
                let link = crate::connection::io::spawn_link(
                    Box::new(UnixCarrier::new(client).unwrap()),
                    Arc::new({
                        let events = Arc::clone(&events);
                        move |event: LinkEvent| {
                            events.lock().unwrap().push(match event {
                                LinkEvent::UpdateFacts(facts) => {
                                    Some((facts.revision, facts.sessions))
                                }
                                LinkEvent::Inventory(_) => None,
                                LinkEvent::Health(_) => return,
                            });
                        }
                    }),
                )
                .unwrap();
                // A change before the first facts are written would replace them in their slot.
                link.next_inventory(0).unwrap();
                Self {
                    link,
                    events,
                    serving,
                }
            }
            /// The events once there are `count`, or after ten seconds.
            fn events(&self, count: usize) -> Vec<Option<(u64, u32)>> {
                let deadline = Instant::now() + std::time::Duration::from_secs(10);
                while self.events.lock().unwrap().len() < count && Instant::now() < deadline {
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
                self.events.lock().unwrap().clone()
            }
            /// Closes the client's end, as an idle client that disconnects does.
            fn close(self) {
                drop(self.link);
                self.serving.join().unwrap();
            }
        }

        fn environment(board: &Arc<Board>) -> Environment {
            let (sync, _) = SyncWorker::spawn().unwrap();
            let mut environment =
                Environment::for_tests(Arc::new(Mutex::new(Box::new(Inventory))), sync, None);
            environment.facts = Some(Arc::clone(board) as _);
            environment
        }

        /// The update facts are a work connection's first message, ahead of the inventory its
        /// service pushes at admission, and they arrive again after each change.
        #[test]
        fn a_work_connection_hears_the_update_facts_first_and_after_each_change() {
            let board = Board::new();
            let environment = environment(&board);
            let connection = Connection::open(&environment, crate::protocol::version::WORK);
            board.change(2, 1);
            assert_eq!(
                connection.events(3),
                [Some((1, 0)), None, Some((2, 1))],
                "facts, then the inventory, then the changed facts"
            );
            assert_eq!(
                connection.link.update_facts().map(|facts| facts.sessions),
                Some(1)
            );
            connection.close();
        }

        /// A selected version without the facts frame never receives it. Every capability is
        /// 1.0 today, so only a version below 1.0 lacks it.
        #[test]
        fn a_connection_whose_version_lacks_the_facts_frame_never_receives_it() {
            let board = Board::new();
            let environment = environment(&board);
            let older = ProtocolVersion { major: 0, minor: 9 };
            let connection = Connection::open(&environment, older);
            board.change(2, 1);
            std::thread::sleep(FOLLOW_RECHECK + std::time::Duration::from_millis(200));
            assert_eq!(connection.events(1), [None], "the inventory alone");
            assert!(connection.link.update_facts().is_none());
            connection.close();
        }

        /// An idle connection that closes while the facts stay unchanged releases its follower
        /// within its recheck, and a later connection alone hears later changes.
        #[test]
        fn a_closed_connection_releases_its_facts_follower_while_the_facts_stay_unchanged() {
            let board = Board::new();
            let environment = environment(&board);
            // The test's handle and the environment's.
            let held = Arc::strong_count(&board);
            let first = Connection::open(&environment, crate::protocol::version::WORK);
            assert_eq!(first.events(2), [Some((1, 0)), None]);
            assert!(
                Arc::strong_count(&board) > held,
                "the follower holds the source"
            );
            first.close();
            let deadline = Instant::now() + 3 * FOLLOW_RECHECK;
            while Arc::strong_count(&board) > held && Instant::now() < deadline {
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            assert_eq!(
                Arc::strong_count(&board),
                held,
                "the ended connection's follower released its source"
            );
            let second = Connection::open(&environment, crate::protocol::version::WORK);
            board.change(2, 3);
            assert_eq!(second.events(3), [Some((1, 0)), None, Some((2, 3))]);
            // The second connection's serving thread holds the environment, and its follower
            // holds the source; the first connection holds nothing.
            assert_eq!(Arc::strong_count(&board), held + 2);
            second.close();
        }
    }

    #[test]
    fn a_panicking_connection_still_frees_its_slot_and_reports_its_end() {
        use std::sync::atomic::AtomicUsize;
        /// Panics on any request, after recording connections that ended.
        struct Panicking(Arc<AtomicUsize>);
        impl PaneService for Panicking {
            fn admitted(&mut self, _: &Peer, _: ClientName, _: Outbox) {}
            fn request(&mut self, _: &Peer, _: RequestId, _: Operation) -> Dispatch {
                panic!("a service failure");
            }
            fn input(&mut self, _: &Peer, _: Input) {}
            fn view(&mut self, _: &Peer, _: View) {}
            fn effect_done(&mut self, _: &Peer, _: EffectDone) {}
            fn acknowledge(&mut self, _: &Peer, _: u64) {}
            fn disconnected(&mut self, _: &Peer) {
                self.0.fetch_add(1, Ordering::SeqCst);
            }
            fn shutdown(&mut self) -> Result<(), CoreError> {
                Ok(())
            }
        }
        let fixture = crate::direct::tests::Fixture::new();
        let ended = Arc::new(AtomicUsize::new(0));
        let rung = Arc::new(AtomicBool::new(false));
        let (sync, _) = SyncWorker::spawn().unwrap();
        let mut environment = Environment::for_tests(
            Arc::new(Mutex::new(Box::new(Panicking(Arc::clone(&ended))))),
            sync,
            None,
        );
        environment.stop = Arc::new(Stop {
            stopping: AtomicBool::new(false),
            wake: Arc::new({
                let rung = Arc::clone(&rung);
                move || rung.store(true, Ordering::Release)
            }),
        });
        let (mut client, core) = UnixStream::pair().unwrap();
        let running = spawn_local(core, [7; 32], &fixture.paths, &environment).unwrap();
        protocol::admit_client(
            &mut client,
            (&[9; 32], &"Test".into()),
            &[7; 32],
            &fixture.paths.identity,
        )
        .unwrap();
        let link = link(client, Arc::new(|_| {})).unwrap();
        let reply = link.request(Operation::Shutdown).unwrap();
        // The accept loop drops a reaped thread's socket clone; this test drops it here.
        let Running {
            thread,
            done,
            sever,
            wake,
        } = running;
        drop((sever, wake));
        assert!(thread.join().is_err(), "the service panicked");
        assert!(reply.wait().is_err());
        assert!(done.load(Ordering::Acquire));
        assert!(
            rung.load(Ordering::Acquire),
            "the accept loop must learn of the free slot"
        );
        assert_eq!(
            ended.load(Ordering::SeqCst),
            1,
            "the service must learn of the end"
        );
    }

    #[test]
    fn an_ended_connection_thread_rings_the_accept_loop_bell() {
        let fixture = crate::direct::tests::Fixture::new();
        fixture.enable();
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
        let (client, core) = UnixStream::pair().unwrap();
        let running = spawn_local(core, [7; 32], &fixture.paths, &environment).unwrap();
        // The peer leaves during admission, so the thread ends.
        drop(client);
        running.thread.join().unwrap();
        assert!(running.done.load(Ordering::Acquire));
        assert!(
            rung.load(Ordering::Acquire),
            "the accept loop must learn of the free slot"
        );
    }

    /// A local uninstall reads the live Sessions and fences later Session starts in one step, and
    /// the fence holds for every connection until the uninstalling connection closes.
    #[test]
    fn an_uninstall_fence_refuses_every_session_start_until_its_connection_closes() {
        use crate::protocol::ReplyBody;
        struct Running;
        impl PaneService for Running {
            fn admitted(&mut self, _: &Peer, _: ClientName, _: Outbox) {}
            fn request(&mut self, _: &Peer, _: RequestId, _: Operation) -> Dispatch {
                Dispatch::reply(Ok(ReplyBody::Done))
            }
            fn sessions(&self) -> Vec<[u8; 32]> {
                vec![[3; 32]]
            }
            fn input(&mut self, _: &Peer, _: Input) {}
            fn view(&mut self, _: &Peer, _: View) {}
            fn effect_done(&mut self, _: &Peer, _: EffectDone) {}
            fn acknowledge(&mut self, _: &Peer, _: u64) {}
            fn disconnected(&mut self, _: &Peer) {}
            fn shutdown(&mut self) -> Result<(), CoreError> {
                Ok(())
            }
        }
        fn peer(connection: u8) -> Peer {
            Peer {
                client: [connection; 32],
                connection: [connection; 32],
                pairing: None,
            }
        }
        fn claim(host: &mut HostService, peer: &Peer) -> Result<ReplyBody, Refusal> {
            let operation = Operation::Claim(crate::protocol::Claim {
                channel: crate::protocol::Channel(1),
                attachment: crate::attachments::Attachment {
                    workspace: [1; 32],
                    generation: 1,
                },
                pane: [2; 32],
                claim: crate::pane::AttachmentClaim::Select { explicit: true },
                size: None,
                style: None,
            });
            match host.request(peer, RequestId(1), operation) {
                Dispatch::Reply(reply) => *reply,
                _ => panic!("expected a reply"),
            }
        }
        let mut host = HostService {
            fence: Arc::default(),
            uninstalling: None,
            inner: Box::new(Running),
            paths: Paths {
                identity: "spaceterm-development".into(),
                data: "/nonexistent/d".into(),
                runtime: "/nonexistent/r".into(),
            },
            stop: Arc::new(Stop {
                stopping: AtomicBool::new(false),
                wake: Arc::new(|| {}),
            }),
        };
        let (uninstaller, other) = (peer(1), peer(2));
        assert!(claim(&mut host, &other).is_ok(), "no fence yet");
        let Dispatch::Reply(reply) = host.request(
            &uninstaller,
            RequestId(1),
            Operation::Sessions { fence: true },
        ) else {
            panic!("expected a reply");
        };
        assert!(matches!(*reply, Ok(ReplyBody::Sessions(ref ids)) if ids == &vec![[3; 32]]));
        assert!(matches!(
            claim(&mut host, &other),
            Err(Refusal::ServiceBusy)
        ));
        assert!(matches!(
            claim(&mut host, &uninstaller),
            Err(Refusal::ServiceBusy)
        ));
        host.disconnected(&other);
        assert!(matches!(
            claim(&mut host, &other),
            Err(Refusal::ServiceBusy)
        ));
        host.disconnected(&uninstaller);
        assert!(
            claim(&mut host, &other).is_ok(),
            "the fence ends with its connection"
        );
    }

    #[test]
    fn the_host_restores_its_service_from_retained_organization() {
        struct Restoring(Arc<AtomicBool>);
        impl PaneService for Restoring {
            fn restore(
                &mut self,
                _: crate::organization::OrganizationStore,
            ) -> Result<(), CoreError> {
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
            fn shutdown(&mut self) -> Result<(), CoreError> {
                Ok(())
            }
        }
        let root = std::env::temp_dir().canonicalize().unwrap().join(format!(
            "st-restore-{:x}",
            u64::from_be_bytes(protocol::random().unwrap()[..8].try_into().unwrap())
        ));
        let restored = Arc::new(AtomicBool::new(false));
        let mut host: Box<dyn PaneService> = Box::new(HostService {
            fence: Arc::default(),
            uninstalling: None,
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
            Err(CoreError::AuthenticationRefused)
        );
    }
    #[test]
    fn validates_real_private_ipc_peers() {
        let (client, core) = UnixStream::pair().unwrap();
        check_peer(&client).unwrap();
        check_peer(&core).unwrap();
    }
    #[test]
    #[ignore = "explicit real-host root-owned peer observation; sends no request bytes"]
    fn refuses_a_real_other_account_peer_without_sending_protocol_bytes() {
        #[cfg(target_os = "macos")]
        let endpoint = "/var/run/usbmuxd";
        #[cfg(target_os = "linux")]
        let endpoint = "/run/dbus/system_bus_socket";
        let stream = UnixStream::connect(endpoint).expect("root-owned host listener required");
        assert_eq!(check_peer(&stream), Err(CoreError::AuthenticationRefused));
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

    const CHILD_TEST: &str = "host::startup_tests::core_start_child";
    const CHILD_ROOT: &str = "SPACETERM_MACHINE_START_FIXTURE";

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
            let pin = record.core;
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
            // A selector that names no test would start a child that exits at once, and a test
            // that only waits for exit would pass without starting SpaceTerm Core.
            let listed = Command::new(std::env::current_exe().unwrap())
                .args(["--exact", test, "--ignored", "--list"])
                .output()
                .unwrap();
            assert!(
                String::from_utf8_lossy(&listed.stdout)
                    .lines()
                    .any(|line| line == format!("{test}: test")),
                "{test} names no child test"
            );
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
            Err(CoreError::UnsafeStorage)
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
            Err(CoreError::NotRunning)
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
            Err(CoreError::UnsafeStorage)
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
            Err(CoreError::UnsafeStorage)
        );
        assert_eq!(fs::read(fixture.paths.socket()).unwrap(), b"existing entry");
        fs::remove_file(fixture.paths.socket()).unwrap();
        fs::rename(&pending, fixture.paths.runtime.join("old")).unwrap();
        let _replacement = UnixListener::bind(&pending).unwrap();
        fs::set_permissions(&pending, fs::Permissions::from_mode(0o600)).unwrap();
        assert_eq!(
            runtime.publish_socket(c"p", expected),
            Err(CoreError::UnsafeStorage)
        );
        assert!(!fixture.paths.socket().exists());
        assert!(pending.exists());
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }
    /// Leaves a private socket entry at `path` that refuses every connection. Its socket binds
    /// but never listens: on macOS a socket's close-on-exec flag is set after it is created, so a
    /// child that another test spawns in between keeps a copy, and a listening copy would answer.
    fn stale_socket(path: &Path) {
        let socket =
            socket2::Socket::new(socket2::Domain::UNIX, socket2::Type::STREAM, None).unwrap();
        socket
            .bind(&socket2::SockAddr::unix(path).unwrap())
            .unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
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
        fn shutdown(&mut self) -> Result<(), CoreError> {
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

    fn admitted_link(fixture: &Fixture, device: &[u8; 32]) -> crate::connection::link::CoreLink {
        let mut stream = connect_when_ready(fixture);
        protocol::admit_client(
            &mut stream,
            (device, &"Test".into()),
            &fixture.pin,
            "spaceterm-development",
        )
        .unwrap();
        link(stream, Arc::new(|_| {})).unwrap()
    }

    #[test]
    #[ignore = "runs only as a child of the server startup tests"]
    fn core_start_child() {
        let root = PathBuf::from(std::env::var_os(CHILD_ROOT).expect("startup fixture root"));
        let result = serve_with_panes(
            &Paths {
                identity: "spaceterm-development".into(),
                data: root.join("d"),
                runtime: root.join("r"),
            },
            None,
        );
        assert_eq!(result, Err(CoreError::StorageUnavailable));
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
            fn shutdown(&mut self) -> Result<(), CoreError> {
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
            crate::panes(Arc::default(), {
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
            assert_eq!(result, Err(CoreError::StorageUnavailable));
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
    fn a_core_refuses_to_serve_data_it_cannot_read_and_leaves_it_unchanged() {
        let fixture = Fixture::new();
        let marker = fixture.paths.data.join("generation.json");
        let identity = fs::read(fixture.paths.data.join("identity.json")).unwrap();
        // A newer SpaceTerm Core wrote this data.
        fs::write(&marker, br#"{"generation":99}"#).unwrap();
        fs::set_permissions(&marker, fs::Permissions::from_mode(0o600)).unwrap();
        assert_eq!(
            serve_with_panes(&fixture.paths, None),
            Err(CoreError::IncompatibleState)
        );
        assert_eq!(fs::read(&marker).unwrap(), br#"{"generation":99}"#);
        // Data older than any generation this Core migrates waits for a confirmed reset.
        fs::write(&marker, br#"{"generation":0}"#).unwrap();
        assert_eq!(
            serve_with_panes(&fixture.paths, None),
            Err(CoreError::DataResetRequired)
        );
        assert_eq!(
            fs::read(fixture.paths.data.join("identity.json")).unwrap(),
            identity
        );
        assert_eq!(
            crate::migration::pending(&crate::migration::core_data::store(&fixture.paths.data))
                .unwrap()
                .map(|pending| pending.kinds),
            Some(vec![crate::migration::DataKind::MachineId])
        );
        fs::remove_dir_all(&fixture.root).unwrap();
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
    fn owned_development_runs_beside_a_stopped_service_that_is_not_installed() {
        let fixture = Fixture::new();
        let data = Directory::open(&fixture.paths.data, false).unwrap();
        // The record an uninstalled service leaves behind.
        data.write_json(
            c"service.json",
            &serde_json::json!({
                "installation": "a".repeat(64),
                "stopped": true,
                "automatic_start": true,
                "installed": false,
                "removing": false,
                "run": null,
            }),
        )
        .unwrap();
        let record = fs::read(fixture.paths.data.join("service.json")).unwrap();
        let mut child = owned_child(&fixture, false);
        let deadline = Instant::now() + Duration::from_secs(5);
        while !fixture.root.join("ready").exists() {
            assert!(
                child.0.try_wait().unwrap().is_none(),
                "the owned server refused to start"
            );
            assert!(
                Instant::now() < deadline,
                "owned server did not become ready"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        // The Development client's admission check passes, and its link is admitted.
        assert_eq!(
            crate::service::check_admission(&fixture.paths, Duration::from_secs(1), None),
            Ok(())
        );
        admitted_link(&fixture, &fixture.device).close();
        child.0.stdin.take();
        await_owned_exit(&mut child);
        assert_eq!(
            fs::read(fixture.paths.data.join("service.json")).unwrap(),
            record
        );
    }

    #[test]
    #[ignore = "runs only as a child of the server signal test"]
    fn core_signal_child() {
        install_shutdown_signals().unwrap();
        let root = PathBuf::from(std::env::var_os(CHILD_ROOT).expect("startup fixture root"));
        serve_with_panes(
            &Paths {
                identity: "spaceterm-development".into(),
                data: root.join("d"),
                runtime: root.join("r"),
            },
            None,
        )
        .unwrap();
    }

    #[test]
    #[ignore = "runs only as a child of the descriptor exhaustion test"]
    fn core_descriptor_child() {
        // A few descriptors above those already open, so connections exhaust them quickly.
        let open = (0..4096)
            // SAFETY: F_GETFD only reads the flags of a descriptor number.
            .filter(|fd| unsafe { libc::fcntl(*fd, libc::F_GETFD) } != -1)
            .max()
            .unwrap_or(0);
        let limit = libc::rlimit {
            rlim_cur: (open + 24) as libc::rlim_t,
            rlim_max: (open + 24) as libc::rlim_t,
        };
        // SAFETY: setrlimit reads one rlimit from the live local.
        assert_eq!(unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &limit) }, 0);
        install_shutdown_signals().unwrap();
        let root = PathBuf::from(std::env::var_os(CHILD_ROOT).expect("startup fixture root"));
        serve_with_panes(
            &Paths {
                identity: "spaceterm-development".into(),
                data: root.join("d"),
                runtime: root.join("r"),
            },
            None,
        )
        .unwrap();
    }

    #[test]
    fn exhausted_descriptors_pause_accepting_without_ending_the_core() {
        let fixture = Fixture::new();
        let mut child = fixture.start_test("host::startup_tests::core_descriptor_child", false);
        let first = connect_when_ready(&fixture);
        // Idle connections hold descriptors in admission until the server runs out.
        let idle: Vec<_> = (0..64)
            .filter_map(|_| UnixStream::connect(fixture.paths.socket()).ok())
            .chain(std::iter::once(first))
            .collect();
        std::thread::sleep(Duration::from_millis(300));
        assert!(
            child.0.try_wait().unwrap().is_none(),
            "running out of descriptors must not end the server"
        );
        drop(idle);
        // Admission resumes once connections end.
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let mut stream = connect_when_ready(&fixture);
            if protocol::admit_client(
                &mut stream,
                (&fixture.device, &"Test".into()),
                &fixture.pin,
                "spaceterm-development",
            )
            .is_ok()
            {
                break;
            }
            assert!(Instant::now() < deadline, "the server never accepted again");
            std::thread::sleep(Duration::from_millis(50));
        }
        // SAFETY: the guard owns this live child process.
        assert_eq!(
            unsafe { libc::kill(child.0.id() as libc::pid_t, libc::SIGTERM) },
            0
        );
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(exit) = child.0.try_wait().unwrap() {
                assert!(exit.success(), "the server must still shut down in order");
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
    fn shutdown_signal_stops_core_with_an_inherited_blocked_mask() {
        let fixture = Fixture::new();
        let mut child = fixture.start_test("host::startup_tests::core_signal_child", true);
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
    fn core_shutdown_child() {
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
            Some(crate::panes(Arc::default(), move |sync| {
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
        fn shutdown(&mut self) -> Result<(), CoreError> {
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
            Some(crate::panes(Arc::default(), |_| Ok(Deferring::default()))),
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
        use crate::protocol::{ClaimReply, ReplyBody, Restored};
        let root = PathBuf::from(std::env::var_os(CHILD_ROOT).expect("startup fixture root"));
        serve_with_panes(
            &Paths {
                identity: "spaceterm-development".into(),
                data: root.join("d"),
                runtime: root.join("r"),
            },
            Some(crate::panes(Arc::default(), |_| {
                Ok(Answering(|operation| {
                    Dispatch::reply(Ok(match operation {
                        Operation::Claim(claim) => {
                            ReplyBody::Claim(ClaimReply::Restored(Box::new(Restored {
                                fresh: false,
                                session_ended: None,
                                recovery: None,
                                directory_unavailable: false,
                                core: [1; 32],
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
                        Operation::ScrollbackPage {
                            epoch, start, rows, ..
                        } => ReplyBody::Page(crate::protocol::Page {
                            epoch,
                            start,
                            rows: (0..rows)
                                .map(|index| crate::protocol::screen::Row {
                                    index,
                                    wrapped: false,
                                    runs: vec![crate::protocol::screen::Run {
                                        style: crate::protocol::screen::StyleRef::Default,
                                        link: None,
                                        text: "A".repeat(200),
                                        cells: crate::protocol::screen::Cells::Narrow,
                                    }],
                                })
                                .collect(),
                            styles: Vec::new(),
                            links: Vec::new(),
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
                    Operation::ScrollbackPage {
                        channel: Channel(1),
                        epoch: 1,
                        start: u64::from(index) * u64::from(crate::protocol::limits::MAX_PAGE_ROWS),
                        rows: crate::protocol::limits::MAX_PAGE_ROWS,
                    }
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
            // A connection whose writes stall ends after `STALL_AFTER`, which would free one
            // that it blocked. Finishing sooner shows that nothing waited for the slow reader.
            assert!(
                started.elapsed() < crate::protocol::limits::STALL_AFTER,
                "a slow reader blocked another connection for {:?}",
                started.elapsed()
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
            ClientMessage, CoreMessage, Fatal, RawFrame, ReplyBody, Request, RequestId,
            frame::{Assembler, FrameReader, encode_client},
        };
        use std::io::{Read, Write};
        let fixture = Fixture::new();
        let mut child = fixture.start_test("host::startup_tests::core_shutdown_child", false);
        let mut pending = connect_when_ready(&fixture);
        protocol::admit_client(
            &mut pending,
            (&fixture.device, &"Test".into()),
            &fixture.pin,
            "spaceterm-development",
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
                    .core(
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
                [] | [CoreMessage::Goodbye(Fatal::Stopping)]
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
        // Generous, because a loaded test host can take seconds to start the child.
        let deadline = Instant::now() + Duration::from_secs(30);
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
        stale_socket(&fixture.paths.socket());
        let _child = fixture.start();
        // Generous, because a loaded test host can take seconds to start the child.
        let deadline = Instant::now() + Duration::from_secs(30);
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
            )
            .map(|negotiated| negotiated.selected),
            Ok(protocol::version::WORK)
        );
    }

    #[test]
    fn stale_socket_removal_preserves_a_replacement_entry() {
        let fixture = Fixture::new();
        let runtime = Directory::open(&fixture.paths.runtime, false).unwrap();
        let _lock = runtime.lock("server.lock", true).unwrap();
        stale_socket(&fixture.paths.socket());
        let expected = runtime.socket_identity(c"s").unwrap().unwrap();
        fs::rename(fixture.paths.socket(), fixture.paths.runtime.join("old")).unwrap();
        let _replacement = UnixListener::bind(fixture.paths.socket()).unwrap();
        fs::set_permissions(fixture.paths.socket(), fs::Permissions::from_mode(0o600)).unwrap();
        let replacement = fs::symlink_metadata(fixture.paths.socket()).unwrap().ino();
        assert_eq!(
            runtime.remove_socket(c"s", expected),
            Err(CoreError::UnsafeStorage)
        );
        assert_eq!(
            fs::symlink_metadata(fixture.paths.socket()).unwrap().ino(),
            replacement
        );
    }
}
