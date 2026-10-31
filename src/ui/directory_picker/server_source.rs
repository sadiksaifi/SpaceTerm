//! Reads paths through the authenticated owning server; never validates them on this client.
use super::*;
use crate::server_connection::ServerConnection;
use spaceterm_server::pane::directories::*;
use spaceterm_server::protocol;
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
    /// Starts `operation`, a directory read or creation, as operation `id`. Its reply arrives
    /// when the server settles it.
    fn begin(&mut self, id: u64, operation: protocol::Operation) -> Result<(), ()>;
    /// Waits at most `timeout` for operation `id` to settle. `None` means it still runs.
    fn settled(&mut self, id: u64, timeout: Duration) -> Result<Option<Result<Value, Error>>, ()>;
    /// Abandons operation `id`.
    fn cancel(&mut self, id: u64) -> Result<(), ()>;
}

/// Opens a connection to the owning server's directory channel.
pub(crate) trait DirectoryConnector: Send + Sync {
    fn connect(&self) -> Result<Box<dyn DirectoryConnection>, ()>;
}

/// A picker's own connection. Each operation is one request whose reply arrives when the server
/// settles it.
struct LinkDirectories {
    link: spaceterm_server::connection::link::ServerLink,
    reads: std::collections::HashMap<u64, spaceterm_server::connection::link::PendingReply>,
}

impl DirectoryConnection for LinkDirectories {
    fn begin(&mut self, id: u64, operation: protocol::Operation) -> Result<(), ()> {
        let pending = self.link.request(operation).map_err(|_| ())?;
        self.reads.insert(id, pending);
        Ok(())
    }
    fn settled(&mut self, id: u64, timeout: Duration) -> Result<Option<Result<Value, Error>>, ()> {
        let Some(reply) = self.reads.get(&id).ok_or(())?.wait_for(timeout) else {
            return Ok(None);
        };
        self.reads.remove(&id);
        match reply {
            Ok(protocol::ReplyBody::Directory(result)) => Ok(Some(result)),
            _ => Err(()),
        }
    }
    fn cancel(&mut self, id: u64) -> Result<(), ()> {
        if let Some(pending) = self.reads.remove(&id) {
            pending.cancel();
        }
        Ok(())
    }
}

impl DirectoryConnector for ServerConnection {
    /// Each picker opens its own connection, so the server cancels its reads when it closes.
    fn connect(&self) -> Result<Box<dyn DirectoryConnection>, ()> {
        Ok(Box::new(LinkDirectories {
            link: ServerConnection::connect(self, Arc::new(|_| {})).map_err(|_| ())?,
            reads: std::collections::HashMap::new(),
        }))
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
    /// Whether this picker offers to create a missing directory.
    creation: bool,
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
    /// another computer. A missing directory can be created there, after the person confirms.
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
            available,
            initial: Rc::default(),
            home: Rc::default(),
        }
    }
    fn run(&self, job: Job) -> SourceFuture<Value> {
        self.run_in(Lane::Kept, job)
    }
    /// Runs `job` on the server.
    fn run_in(&self, lane: Lane, job: Job) -> SourceFuture<Value> {
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
            result.map_err(|failure| match failure {
                // The server's route is still up, so a channel that couldn't open says nothing
                // about the connection.
                Failure::ChannelUnavailable => DirectorySourceError::Other,
                Failure::Source(error) => error,
            })
        })
    }
    /// Asks the server which of `candidates` names the same directory as `path`, by the
    /// directories' identity on that server, returning the first match's position.
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

type Answer = Result<Value, Failure>;

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
            match exchange(
                open.as_mut(),
                queued.job.clone(),
                next_id,
                &queued.cancelled,
            ) {
                Ok(value) => {
                    answer = Ok(value);
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

/// Begins `job` on the server as operation `id` and waits until it settles, cancelling it when the
/// picker no longer waits or it outlasts its deadline.
fn exchange(
    connection: &mut dyn DirectoryConnection,
    job: Job,
    id: u64,
    cancelled: &AtomicBool,
) -> Result<Value, Exchange> {
    let operation = match job {
        Job::Read(operation) => protocol::Operation::DirectoryRead(operation),
        Job::Create(path) => protocol::Operation::DirectoryCreate(Creation { path }),
    };
    connection
        .begin(id, operation)
        .map_err(|()| Exchange::Lost)?;
    let deadline = Instant::now() + Duration::from_secs(8);
    loop {
        if cancelled.load(Ordering::Acquire) || Instant::now() >= deadline {
            connection.cancel(id).map_err(|()| Exchange::Lost)?;
            return Err(Exchange::Answered(DirectorySourceError::Other));
        }
        // The picker abandons an operation by setting a flag, so the wait wakes to look at it.
        match connection
            .settled(id, Duration::from_millis(20))
            .map_err(|()| Exchange::Lost)?
        {
            Some(Ok(value)) => return Ok(value),
            Some(Err(error)) => return Err(Exchange::Answered(source_error(error))),
            None => {}
        }
    }
}
impl DirectorySource for ServerDirectorySource {
    fn machine_name(&self) -> Option<&str> {
        self.machine.as_deref()
    }
    fn allows_creation(&self) -> bool {
        self.creation
    }
    fn initial_query(&self) -> Option<PickerPath> {
        self.initial.borrow().clone()
    }
    fn selection_label(&self) -> &'static str {
        self.selection_label
    }
    fn discover_home(&self) -> SourceFuture<PickerPath> {
        let read = self.run(Job::Read(Operation::Home));
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
    use crate::runtime::directory_testing::DirectoryHost;
    use spaceterm_server::pane::directories::{Error, Value};
    use spaceterm_server::protocol::Operation;
    use std::rc::Rc;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    pub(crate) type ServerHandler = Arc<Mutex<DirectoryHost>>;

    pub(crate) struct InProcessServer(pub(crate) ServerHandler);

    struct InProcessConnection {
        server: ServerHandler,
    }

    impl DirectoryConnector for InProcessServer {
        fn connect(&self) -> Result<Box<dyn DirectoryConnection>, ()> {
            Ok(Box::new(InProcessConnection {
                server: Arc::clone(&self.0),
            }))
        }
    }

    impl DirectoryConnection for InProcessConnection {
        fn begin(&mut self, id: u64, operation: Operation) -> Result<(), ()> {
            self.server.lock().unwrap().begin(id, operation)
        }
        fn settled(
            &mut self,
            id: u64,
            timeout: Duration,
        ) -> Result<Option<Result<Value, Error>>, ()> {
            self.server.lock().unwrap().settled(id, timeout)
        }
        fn cancel(&mut self, id: u64) -> Result<(), ()> {
            self.server.lock().unwrap().cancel(id);
            Ok(())
        }
    }

    /// Every presented server answers through the server runtime's own directory handler, on
    /// this computer's filesystem.
    #[cfg(feature = "native-tests")]
    pub(crate) fn in_process() -> ServerDirectories {
        let server = Arc::new(InProcessServer(Arc::new(Mutex::new(
            crate::runtime::directory_testing::directory_host(),
        ))));
        Rc::new(move |_| Arc::clone(&server) as Arc<dyn DirectoryConnector>)
    }

    /// A server on which every directory is the one its path spells: it says which candidate
    /// names the same directory as a path, and reads nothing else.
    pub(crate) fn matching() -> ServerDirectories {
        gated(Arc::new(std::sync::atomic::AtomicBool::new(true)))
    }

    /// The [`matching`] server over a channel that opens only once `open` is set, so a test holds
    /// its answers back.
    pub(crate) fn gated(open: Arc<std::sync::atomic::AtomicBool>) -> ServerDirectories {
        struct Matching(Arc<std::sync::atomic::AtomicBool>);
        #[derive(Default)]
        struct MatchingConnection(std::collections::HashMap<u64, Result<Value, Error>>);
        impl DirectoryConnector for Matching {
            fn connect(&self) -> Result<Box<dyn DirectoryConnection>, ()> {
                while !self.0.load(std::sync::atomic::Ordering::Acquire) {
                    std::thread::sleep(std::time::Duration::from_millis(1));
                }
                Ok(Box::<MatchingConnection>::default())
            }
        }
        impl DirectoryConnection for MatchingConnection {
            fn begin(&mut self, id: u64, operation: Operation) -> Result<(), ()> {
                use spaceterm_server::pane::directories::Operation as Read;
                let answer = match operation {
                    Operation::DirectoryRead(Read::Match { path, candidates }) => Ok(Value::Match {
                        index: candidates
                            .iter()
                            .position(|candidate| *candidate == path)
                            .and_then(|index| u16::try_from(index).ok()),
                    }),
                    _ => Err(Error::Unavailable),
                };
                self.0.insert(id, answer);
                Ok(())
            }
            fn settled(
                &mut self,
                id: u64,
                _: Duration,
            ) -> Result<Option<Result<Value, Error>>, ()> {
                self.0.remove(&id).map(Some).ok_or(())
            }
            fn cancel(&mut self, id: u64) -> Result<(), ()> {
                self.0.remove(&id);
                Ok(())
            }
        }
        let server = Arc::new(Matching(open));
        Rc::new(move |_| Arc::clone(&server) as Arc<dyn DirectoryConnector>)
    }
}
