//! Reads paths through the authenticated owning server; never validates them on this client.
use super::*;
use crate::server_connection::ServerConnection;
use spaceterm_server::pane::{Reply, Request as PaneRequest, directories::*};
use std::{
    path::PathBuf,
    sync::{
        Arc, PoisonError,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

/// One connection to the owning server's directory channel. Nothing behind it reads or
/// changes this computer's filesystem.
pub(crate) trait DirectoryConnection: Send {
    /// Whether the server reads directories for this client.
    fn reads_directories(&self) -> bool;
    /// Whether the server creates directories for this client.
    fn creates_directories(&self) -> bool;
    /// Whether the server says if a directory is the same one another path names.
    fn matches_directories(&self) -> bool {
        false
    }
    fn exchange(&mut self, request: &PaneRequest) -> Result<Reply, ()>;
}

/// Opens a connection to the owning server's directory channel.
pub(crate) trait DirectoryConnector: Send + Sync {
    fn connect(&self) -> Result<Box<dyn DirectoryConnection>, ()>;
}

impl DirectoryConnection for spaceterm_server::connection::link::ServerLink {
    // Protocol major 2 servers read, create and match directories.
    fn reads_directories(&self) -> bool {
        true
    }
    fn creates_directories(&self) -> bool {
        true
    }
    fn matches_directories(&self) -> bool {
        true
    }
    fn exchange(&mut self, request: &PaneRequest) -> Result<Reply, ()> {
        use spaceterm_server::protocol::{Operation, ReplyBody};
        let (operation, create) = match request {
            PaneRequest::DirectoryRead(read) => (Operation::DirectoryRead(read.clone()), false),
            PaneRequest::DirectoryCreate(create) => {
                (Operation::DirectoryCreate(create.clone()), true)
            }
            _ => return Err(()),
        };
        match self.request(operation).map_err(|_| ())?.wait() {
            Ok(ReplyBody::Directory(progress)) if create => Ok(Reply::DirectoryCreate(progress)),
            Ok(ReplyBody::Directory(progress)) => Ok(Reply::DirectoryRead(progress)),
            _ => Err(()),
        }
    }
}

impl DirectoryConnector for ServerConnection {
    /// Each picker opens its own connection, so the server cancels its reads when it closes.
    fn connect(&self) -> Result<Box<dyn DirectoryConnection>, ()> {
        Ok(Box::new(
            ServerConnection::connect(self, Arc::new(|_| {})).map_err(|_| ())?,
        ))
    }
}

/// Reaches a presented server's directory channel. Production reaches the server itself.
pub(crate) type ServerDirectories = Rc<dyn Fn(&ServerConnection) -> Arc<dyn DirectoryConnector>>;

/// The directory channel of each presented server, over its own admitted route.
pub(crate) fn owning_server_directories() -> ServerDirectories {
    Rc::new(|server| Arc::new(server.clone()))
}

/// Which reads a newer one makes pointless. The picker shows only its latest listing and its
/// latest probe of the typed path, so a newer one of either replaces a waiting older one.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Lane {
    Listing,
    Probe,
    /// Discovering home, selecting, and creating are always answered.
    Kept,
}

/// One directory operation the owning server performs.
#[derive(Clone)]
enum Job {
    Read(Operation),
    /// Creates the directory and any missing parents, then validates it.
    Create(PathBuf),
}

pub(crate) struct ServerDirectorySource {
    channel: DirectoryChannel,
    /// A directory whose nearest available ancestor the picker opens at, instead of home.
    original: Option<PathBuf>,
    /// The other computer's name, for a server that runs on one.
    machine: Option<String>,
    selection_label: &'static str,
    /// Whether this picker may offer to create a missing directory at all.
    creation: bool,
    /// Whether the server said it creates directories, learned on the first connection.
    creates: Rc<std::cell::Cell<bool>>,
    available: Rc<dyn Fn() -> bool>,
    initial: Rc<std::cell::RefCell<Option<PickerPath>>>,
    home: Rc<std::cell::RefCell<Option<PathBuf>>>,
}
impl ServerDirectorySource {
    /// Chooses a replacement for `original`, starting at its nearest available ancestor. A
    /// replacement is an existing directory, so this picker never creates one.
    pub(crate) fn new(
        server: ServerConnection,
        original: PathBuf,
        available: Rc<dyn Fn() -> bool>,
    ) -> Self {
        Self {
            original: Some(original),
            selection_label: "Choose",
            creation: false,
            ..Self::browsing(server, None, available)
        }
    }
    /// Browses the server's directories from its home, naming `machine` when the server runs on
    /// another computer. A missing directory can be created there, after the person confirms,
    /// when the server creates directories.
    pub(crate) fn browsing(
        server: ServerConnection,
        machine: Option<String>,
        available: Rc<dyn Fn() -> bool>,
    ) -> Self {
        Self::browsing_through(Arc::new(server), machine, available)
    }
    pub(crate) fn browsing_through(
        connector: Arc<dyn DirectoryConnector>,
        machine: Option<String>,
        available: Rc<dyn Fn() -> bool>,
    ) -> Self {
        Self {
            channel: DirectoryChannel::new(connector),
            original: None,
            machine,
            selection_label: "Open",
            creation: true,
            creates: Rc::default(),
            available,
            initial: Rc::default(),
            home: Rc::default(),
        }
    }
    fn run(&self, job: Job) -> SourceFuture<Value> {
        self.run_reporting(job, Lane::Kept, None)
    }
    fn run_in(&self, lane: Lane, job: Job) -> SourceFuture<Value> {
        self.run_reporting(job, lane, None)
    }
    /// Runs `job` on the server, recording into `creates` whether that server creates
    /// directories.
    fn run_reporting(
        &self,
        job: Job,
        lane: Lane,
        creates: Option<Rc<std::cell::Cell<bool>>>,
    ) -> SourceFuture<Value> {
        if !(self.available)() {
            return Box::pin(async { Err(DirectorySourceError::ConnectionLost) });
        }
        let available = Rc::clone(&self.available);
        let cancelled = Arc::new(AtomicBool::new(false));
        let guard = CancelRead(Arc::clone(&cancelled));
        let task = self.channel.submit(job, lane, cancelled);
        Box::pin(async move {
            let _guard = guard;
            let result = task.await;
            if !available() {
                return Err(DirectorySourceError::ConnectionLost);
            }
            let (value, server_creates) = result.map_err(|failure| match failure {
                // The server's route is still up, so a channel that couldn't open says nothing
                // about the connection.
                Failure::ChannelUnavailable => DirectorySourceError::Other,
                Failure::Source(error) => error,
            })?;
            if let Some(creates) = creates {
                creates.set(server_creates);
            }
            Ok(value)
        })
    }
    /// Asks the server which of `candidates` names the same directory as `path`, by the
    /// directories' identity on that server, returning the first match's position. A server
    /// that can't compare directories reports none.
    pub(crate) fn match_directory(
        &self,
        path: PathBuf,
        candidates: Vec<PathBuf>,
    ) -> SourceFuture<Option<usize>> {
        let read = self.run(Job::Read(Operation::Match { path, candidates }));
        Box::pin(async move {
            match read.await? {
                Value::Match { index } => Ok(index.map(usize::from)),
                _ => Err(DirectorySourceError::Other),
            }
        })
    }
    fn path(&self, directory: PickerPath) -> Result<PathBuf, DirectorySourceError> {
        if let Some(relative) = directory.as_str().strip_prefix("~/") {
            self.home
                .borrow()
                .as_ref()
                .map(|home| home.join(relative))
                .ok_or(DirectorySourceError::Other)
        } else if directory.as_str().starts_with('/') {
            Ok(directory.into_string().into())
        } else {
            Err(DirectorySourceError::Other)
        }
    }
}
struct CancelRead(Arc<AtomicBool>);
impl Drop for CancelRead {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Release);
    }
}

/// Why a queued directory operation produced no answer.
enum Failure {
    /// No channel to the server could be opened or kept.
    ChannelUnavailable,
    Source(DirectorySourceError),
}

type Answer = Result<(Value, bool), Failure>;

struct Queued {
    job: Job,
    lane: Lane,
    cancelled: Arc<AtomicBool>,
    answer: async_channel::Sender<Answer>,
}

/// How many operations the picker still waits for may queue for the channel. Superseded ones
/// the picker abandoned leave the queue before this applies.
const QUEUE_LIMIT: usize = 64;

/// The operations waiting for the channel's worker, and whether the picker still holds it.
#[derive(Default)]
struct Queue {
    waiting: std::collections::VecDeque<Queued>,
    closed: bool,
}

#[derive(Default)]
struct Jobs {
    queue: std::sync::Mutex<Queue>,
    ready: std::sync::Condvar,
}

impl Jobs {
    /// Queues `queued` unless as many live operations already wait.
    fn push(&self, queued: Queued) -> bool {
        let mut queue = self.queue.lock().unwrap_or_else(PoisonError::into_inner);
        let lane = queued.lane;
        queue.waiting.retain(|waiting| {
            !waiting.cancelled.load(Ordering::Acquire)
                && (lane == Lane::Kept || waiting.lane != lane)
        });
        if queue.closed || queue.waiting.len() >= QUEUE_LIMIT {
            return false;
        }
        queue.waiting.push_back(queued);
        self.ready.notify_one();
        true
    }

    /// The next operation, or `None` once the picker dropped the channel.
    fn pop(&self) -> Option<Queued> {
        let mut queue = self.queue.lock().unwrap_or_else(PoisonError::into_inner);
        loop {
            if let Some(queued) = queue.waiting.pop_front() {
                return Some(queued);
            }
            if queue.closed {
                return None;
            }
            queue = self
                .ready
                .wait(queue)
                .unwrap_or_else(PoisonError::into_inner);
        }
    }

    fn close(&self) {
        self.queue
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .closed = true;
        self.ready.notify_all();
    }
}

/// One picker's connection to the owning server's directory operations. A single worker runs
/// them one at a time over one reused connection, so a burst of typing opens one server
/// channel, not one per keystroke. An operation abandoned before its turn never reaches the
/// server.
struct DirectoryChannel {
    connector: Arc<dyn DirectoryConnector>,
    /// The worker's queue, once its first operation started it.
    jobs: std::cell::RefCell<Option<Arc<Jobs>>>,
}

impl Drop for DirectoryChannel {
    fn drop(&mut self) {
        if let Some(jobs) = self.jobs.get_mut() {
            jobs.close();
        }
    }
}

impl DirectoryChannel {
    fn new(connector: Arc<dyn DirectoryConnector>) -> Self {
        Self {
            connector,
            jobs: std::cell::RefCell::new(None),
        }
    }

    /// Queues `job` and answers once the server settles it.
    fn submit(
        &self,
        job: Job,
        lane: Lane,
        cancelled: Arc<AtomicBool>,
    ) -> impl std::future::Future<Output = Answer> + 'static {
        let (answer, answered) = async_channel::bounded(1);
        let queued = Queued {
            job,
            lane,
            cancelled,
            answer,
        };
        let sent = self.jobs().is_some_and(|jobs| jobs.push(queued));
        async move {
            if !sent {
                return Err(Failure::Source(DirectorySourceError::Other));
            }
            answered
                .recv()
                .await
                .unwrap_or(Err(Failure::Source(DirectorySourceError::Other)))
        }
    }

    /// The worker's queue, starting the worker on first use.
    fn jobs(&self) -> Option<Arc<Jobs>> {
        let mut jobs = self.jobs.borrow_mut();
        if jobs.is_none() {
            let queue = Arc::new(Jobs::default());
            let served = Arc::clone(&queue);
            let connector = Arc::clone(&self.connector);
            std::thread::Builder::new()
                .name("spaceterm-directory-channel".into())
                .spawn(move || serve(connector.as_ref(), &served))
                .ok()?;
            *jobs = Some(queue);
        }
        jobs.clone()
    }
}

/// Runs queued operations until the picker drops its channel, reusing one connection.
fn serve(connector: &dyn DirectoryConnector, jobs: &Jobs) {
    let mut connection: Option<Box<dyn DirectoryConnection>> = None;
    let mut next_id = 0_u64;
    while let Some(queued) = jobs.pop() {
        if queued.cancelled.load(Ordering::Acquire) {
            continue;
        }
        // A read the connection lost is asked again once on a fresh connection. A creation is
        // never repeated: its first attempt may already have taken effect.
        let attempts = if matches!(queued.job, Job::Read(_)) {
            2
        } else {
            1
        };
        // The server closes a channel that stays idle, and the picker can sit idle on its
        // confirmation alert, so a creation is sent only over a channel that was just admitted.
        // Opening it changes nothing on the server.
        if matches!(queued.job, Job::Create(_)) {
            connection = None;
        }
        let mut answer = Err(Failure::ChannelUnavailable);
        for _ in 0..attempts {
            if queued.cancelled.load(Ordering::Acquire) {
                break;
            }
            if connection.is_none() {
                connection = connector.connect().ok();
                // Connecting can take a while; an operation the picker abandoned meanwhile is
                // never sent.
                if queued.cancelled.load(Ordering::Acquire) {
                    break;
                }
            }
            let Some(open) = connection.as_mut() else {
                answer = Err(Failure::ChannelUnavailable);
                continue;
            };
            next_id += 1;
            let creates = open.creates_directories();
            match exchange(
                open.as_mut(),
                queued.job.clone(),
                next_id,
                &queued.cancelled,
            ) {
                Ok(value) => {
                    answer = Ok((value, creates));
                    break;
                }
                Err(Exchange::Lost) => {
                    connection = None;
                    answer = Err(Failure::ChannelUnavailable);
                }
                Err(Exchange::Answered(error)) => {
                    answer = Err(Failure::Source(error));
                    break;
                }
            }
        }
        let _ = queued.answer.try_send(answer);
    }
}

/// How one exchange ended without a value.
enum Exchange {
    /// The connection failed, so the next operation needs a new one.
    Lost,
    Answered(DirectorySourceError),
}

/// Begins `job` on the server as operation `id` and polls it until it settles, cancelling it
/// when the picker no longer waits or it outlasts its deadline.
fn exchange(
    connection: &mut dyn DirectoryConnection,
    job: Job,
    id: u64,
    cancelled: &AtomicBool,
) -> Result<Value, Exchange> {
    let creating = matches!(job, Job::Create(_));
    if !(if creating {
        connection.creates_directories()
    } else {
        connection.reads_directories()
    }) {
        return Err(Exchange::Answered(DirectorySourceError::Other));
    }
    // A server that can't compare directories is never asked to; it reports no match, as
    // comparing only the paths' spellings would.
    if matches!(job, Job::Read(Operation::Match { .. })) && !connection.matches_directories() {
        return Ok(Value::Match { index: None });
    }
    let begin = match job {
        Job::Read(operation) => PaneRequest::DirectoryRead(Request::Begin { id, operation }),
        Job::Create(path) => PaneRequest::DirectoryCreate(CreationRequest::Begin { id, path }),
    };
    let poll = || {
        if creating {
            PaneRequest::DirectoryCreate(CreationRequest::Poll { id })
        } else {
            PaneRequest::DirectoryRead(Request::Poll { id })
        }
    };
    let cancel = || {
        if creating {
            PaneRequest::DirectoryCreate(CreationRequest::Cancel { id })
        } else {
            PaneRequest::DirectoryRead(Request::Cancel { id })
        }
    };
    let mut reply = connection.exchange(&begin).map_err(|()| Exchange::Lost)?;
    let deadline = Instant::now() + Duration::from_secs(8);
    loop {
        if cancelled.load(Ordering::Acquire) || Instant::now() >= deadline {
            connection
                .exchange(&cancel())
                .map_err(|()| Exchange::Lost)?;
            return Err(Exchange::Answered(DirectorySourceError::Other));
        }
        match reply {
            Reply::DirectoryRead(Ok(Progress::Ready(value)))
            | Reply::DirectoryCreate(Ok(Progress::Ready(value))) => return Ok(value),
            Reply::DirectoryRead(Ok(Progress::Pending))
            | Reply::DirectoryCreate(Ok(Progress::Pending)) => {}
            Reply::DirectoryRead(Err(error)) | Reply::DirectoryCreate(Err(error)) => {
                return Err(Exchange::Answered(source_error(error)));
            }
            _ => return Err(Exchange::Answered(DirectorySourceError::Other)),
        }
        std::thread::sleep(Duration::from_millis(20));
        reply = connection.exchange(&poll()).map_err(|()| Exchange::Lost)?;
    }
}
impl DirectorySource for ServerDirectorySource {
    fn machine_name(&self) -> Option<&str> {
        self.machine.as_deref()
    }
    fn allows_creation(&self) -> bool {
        self.creation && self.creates.get()
    }
    fn initial_query(&self) -> Option<PickerPath> {
        self.initial.borrow().clone()
    }
    fn selection_label(&self) -> &'static str {
        self.selection_label
    }
    fn discover_home(&self) -> SourceFuture<PickerPath> {
        let read = self.run_reporting(
            Job::Read(Operation::Home),
            Lane::Kept,
            Some(Rc::clone(&self.creates)),
        );
        let home = Rc::clone(&self.home);
        let ancestor = self
            .original
            .clone()
            .map(|path| self.run(Job::Read(Operation::Ancestor { path })));
        let initial = Rc::clone(&self.initial);
        Box::pin(async move {
            match read.await? {
                Value::Directory(path) => {
                    let value = PickerPath::new(
                        path.to_str().ok_or(DirectorySourceError::Other)?.to_owned(),
                    )
                    .map_err(|_| DirectorySourceError::Other)?;
                    *home.borrow_mut() = Some(path);
                    if let Some(ancestor) = ancestor
                        && let Ok(Value::Directory(path)) = ancestor.await
                    {
                        *initial.borrow_mut() = path.to_str().and_then(|s| {
                            PickerPath::new(format!("{}/", s.trim_end_matches('/'))).ok()
                        });
                    }
                    Ok(value)
                }
                _ => Err(DirectorySourceError::Other),
            }
        })
    }
    fn list_directories(&self, directory: PickerPath) -> SourceFuture<DirectoryListing> {
        let read = self
            .path(directory)
            .map(|path| self.run_in(Lane::Listing, Job::Read(Operation::List { path })));
        Box::pin(async move {
            match read?.await? {
                Value::Listing { names, truncated }
                    if names.len() <= MAX_ROWS
                        && names.iter().map(String::len).sum::<usize>() <= MAX_LIST_BYTES =>
                {
                    Ok(DirectoryListing::bounded(
                        names
                            .into_iter()
                            .filter_map(|name| DirectoryRow::new(name).ok())
                            .collect(),
                        truncated,
                    ))
                }
                _ => Err(DirectorySourceError::Other),
            }
        })
    }
    fn probe_exact_path(&self, directory: PickerPath) -> SourceFuture<ExactPathState> {
        let read = self
            .path(directory)
            .map(|path| self.run_in(Lane::Probe, Job::Read(Operation::Validate { path })));
        Box::pin(async move {
            match read?.await {
                Ok(Value::Directory(_)) => Ok(ExactPathState::ReadableDirectory),
                Err(DirectorySourceError::Missing) => Ok(ExactPathState::Missing),
                Err(error) => Err(error),
                _ => Err(DirectorySourceError::Other),
            }
        })
    }
    fn create_directory_recursively(&self, directory: PickerPath) -> SourceFuture<()> {
        if !self.allows_creation() {
            return Box::pin(async { Err(DirectorySourceError::Other) });
        }
        let created = self.path(directory).map(|path| self.run(Job::Create(path)));
        Box::pin(async move {
            match created?.await? {
                Value::Directory(_) => Ok(()),
                _ => Err(DirectorySourceError::Other),
            }
        })
    }
    fn select(&self, directory: PickerPath) -> SourceFuture<DirectorySelection> {
        let read = self
            .path(directory)
            .map(|path| self.run(Job::Read(Operation::Validate { path })));
        Box::pin(async move {
            match read?.await? {
                Value::Directory(path) => Ok(DirectorySelection::Server(path)),
                _ => Err(DirectorySourceError::Other),
            }
        })
    }
}
fn source_error(error: Error) -> DirectorySourceError {
    match error {
        Error::Missing => DirectorySourceError::Missing,
        Error::NotDirectory => DirectorySourceError::NotDirectory,
        Error::PermissionDenied => DirectorySourceError::PermissionDenied,
        _ => DirectorySourceError::Other,
    }
}

/// Owning servers that answer in process, through the same request handler and bounded
/// directory workers a real server runs.
#[cfg(test)]
pub(crate) mod testing {
    use super::{DirectoryConnection, DirectoryConnector, ServerDirectories};
    use spaceterm_server::pane::{Reply, Request as PaneRequest};
    use std::rc::Rc;
    use std::sync::{Arc, Mutex};

    pub(crate) type ServerHandler = Arc<Mutex<Box<dyn FnMut(&PaneRequest) -> Reply + Send>>>;

    pub(crate) struct InProcessServer(pub(crate) ServerHandler);

    struct InProcessConnection {
        server: ServerHandler,
        reads: bool,
        creates: bool,
        matches: bool,
    }

    impl DirectoryConnector for InProcessServer {
        fn connect(&self) -> Result<Box<dyn DirectoryConnection>, ()> {
            // Protocol major 2 servers read, create and match directories.
            Ok(Box::new(InProcessConnection {
                server: Arc::clone(&self.0),
                reads: true,
                creates: true,
                matches: true,
            }))
        }
    }

    impl DirectoryConnection for InProcessConnection {
        fn reads_directories(&self) -> bool {
            self.reads
        }
        fn creates_directories(&self) -> bool {
            self.creates
        }
        fn matches_directories(&self) -> bool {
            self.matches
        }
        fn exchange(&mut self, request: &PaneRequest) -> Result<Reply, ()> {
            Ok((self.server.lock().unwrap())(request))
        }
    }

    /// Every presented server answers through the server runtime's own directory handler, on
    /// this computer's filesystem.
    #[cfg(feature = "native-tests")]
    pub(crate) fn in_process() -> ServerDirectories {
        let handler: Box<dyn FnMut(&PaneRequest) -> Reply + Send> =
            Box::new(crate::runtime::directory_testing::directory_host());
        let server = Arc::new(InProcessServer(Arc::new(Mutex::new(handler))));
        Rc::new(move |_| Arc::clone(&server) as Arc<dyn DirectoryConnector>)
    }

    /// A server from before directories could be compared: it reads directories and answers
    /// nothing else.
    pub(crate) fn older() -> ServerDirectories {
        gated(Arc::new(std::sync::atomic::AtomicBool::new(true)))
    }

    /// The [`older`] server over a channel that opens only once `open` is set, so a test holds
    /// its answers back.
    pub(crate) fn gated(open: Arc<std::sync::atomic::AtomicBool>) -> ServerDirectories {
        struct Older(Arc<std::sync::atomic::AtomicBool>);
        struct OlderConnection;
        impl DirectoryConnector for Older {
            fn connect(&self) -> Result<Box<dyn DirectoryConnection>, ()> {
                while !self.0.load(std::sync::atomic::Ordering::Acquire) {
                    std::thread::sleep(std::time::Duration::from_millis(1));
                }
                Ok(Box::new(OlderConnection))
            }
        }
        impl DirectoryConnection for OlderConnection {
            fn reads_directories(&self) -> bool {
                true
            }
            fn creates_directories(&self) -> bool {
                false
            }
            fn exchange(&mut self, _: &PaneRequest) -> Result<Reply, ()> {
                Err(())
            }
        }
        let server = Arc::new(Older(open));
        Rc::new(move |_| Arc::clone(&server) as Arc<dyn DirectoryConnector>)
    }
}
