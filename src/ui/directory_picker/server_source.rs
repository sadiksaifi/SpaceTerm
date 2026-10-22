//! Reads paths through the authenticated owning server; never validates them on this client.
use super::*;
use crate::local_server::LocalServer;
use spaceterm_server::pane::{Reply, Request as PaneRequest, directories::*};
use std::{
    path::PathBuf,
    sync::{
        Arc,
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
    fn exchange(&mut self, request: &PaneRequest) -> Result<Reply, ()>;
}

/// Opens one connection to the owning server for each directory operation.
pub(crate) trait DirectoryConnector: Send + Sync {
    fn connect(&self) -> Result<Box<dyn DirectoryConnection>, ()>;
}

impl DirectoryConnection for spaceterm_server::pane::PaneConnection {
    fn reads_directories(&self) -> bool {
        self.supports_directory_recovery()
    }
    fn creates_directories(&self) -> bool {
        self.supports_directory_creation()
    }
    fn exchange(&mut self, request: &PaneRequest) -> Result<Reply, ()> {
        Self::exchange(self, request).map_err(|_| ())
    }
}

impl DirectoryConnector for LocalServer {
    fn connect(&self) -> Result<Box<dyn DirectoryConnection>, ()> {
        Ok(Box::new(self.connect_pane().map_err(|_| ())?))
    }
}

/// One directory operation the owning server performs.
enum Job {
    Read(Operation),
    /// Creates the directory and any missing parents, then validates it.
    Create(PathBuf),
}

pub(crate) struct ServerDirectorySource {
    connector: Arc<dyn DirectoryConnector>,
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
    executor: gpui::BackgroundExecutor,
    home: Rc<std::cell::RefCell<Option<PathBuf>>>,
}
impl ServerDirectorySource {
    /// Chooses a replacement for `original`, starting at its nearest available ancestor. A
    /// replacement is an existing directory, so this picker never creates one.
    pub(crate) fn new(
        server: LocalServer,
        executor: gpui::BackgroundExecutor,
        original: PathBuf,
        available: Rc<dyn Fn() -> bool>,
    ) -> Self {
        Self {
            original: Some(original),
            selection_label: "Choose",
            creation: false,
            ..Self::browsing(server, executor, None, available)
        }
    }
    /// Browses the server's directories from its home, naming `machine` when the server runs on
    /// another computer. A missing directory can be created there, after the person confirms,
    /// when the server creates directories.
    pub(crate) fn browsing(
        server: LocalServer,
        executor: gpui::BackgroundExecutor,
        machine: Option<String>,
        available: Rc<dyn Fn() -> bool>,
    ) -> Self {
        Self::browsing_through(Arc::new(server), executor, machine, available)
    }
    pub(super) fn browsing_through(
        connector: Arc<dyn DirectoryConnector>,
        executor: gpui::BackgroundExecutor,
        machine: Option<String>,
        available: Rc<dyn Fn() -> bool>,
    ) -> Self {
        Self {
            connector,
            original: None,
            machine,
            selection_label: "Open",
            creation: true,
            creates: Rc::default(),
            available,
            initial: Rc::default(),
            executor,
            home: Rc::default(),
        }
    }
    fn run(&self, job: Job) -> SourceFuture<Value> {
        self.run_reporting(job, None)
    }
    /// Runs `job` on the server, recording into `creates` whether that server creates
    /// directories.
    fn run_reporting(
        &self,
        job: Job,
        creates: Option<Rc<std::cell::Cell<bool>>>,
    ) -> SourceFuture<Value> {
        if !(self.available)() {
            return Box::pin(async { Err(DirectorySourceError::ConnectionLost) });
        }
        let available = Rc::clone(&self.available);
        let connector = Arc::clone(&self.connector);
        let cancelled = Arc::new(AtomicBool::new(false));
        let guard = CancelRead(Arc::clone(&cancelled));
        let task = self.executor.spawn(async move {
            let mut connection = connector
                .connect()
                .map_err(|_| DirectorySourceError::ConnectionLost)?;
            let creates = connection.creates_directories();
            let value = exchange(connection.as_mut(), job, &cancelled)?;
            Ok((value, creates))
        });
        Box::pin(async move {
            let _guard = guard;
            let result = task.await;
            if !available() {
                return Err(DirectorySourceError::ConnectionLost);
            }
            let (value, server_creates) = result?;
            if let Some(creates) = creates {
                creates.set(server_creates);
            }
            Ok(value)
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

/// Begins `job` on the server and polls it until it settles, cancelling it when the picker no
/// longer waits or it outlasts its deadline.
fn exchange(
    connection: &mut dyn DirectoryConnection,
    job: Job,
    cancelled: &AtomicBool,
) -> Result<Value, DirectorySourceError> {
    let creating = matches!(job, Job::Create(_));
    if !(if creating {
        connection.creates_directories()
    } else {
        connection.reads_directories()
    }) {
        return Err(DirectorySourceError::Other);
    }
    let begin = match job {
        Job::Read(operation) => PaneRequest::DirectoryRead(Request::Begin { id: 1, operation }),
        Job::Create(path) => PaneRequest::DirectoryCreate(CreationRequest::Begin { id: 1, path }),
    };
    let poll = || {
        if creating {
            PaneRequest::DirectoryCreate(CreationRequest::Poll { id: 1 })
        } else {
            PaneRequest::DirectoryRead(Request::Poll { id: 1 })
        }
    };
    let cancel = || {
        if creating {
            PaneRequest::DirectoryCreate(CreationRequest::Cancel { id: 1 })
        } else {
            PaneRequest::DirectoryRead(Request::Cancel { id: 1 })
        }
    };
    let mut reply = connection
        .exchange(&begin)
        .map_err(|_| DirectorySourceError::ConnectionLost)?;
    let deadline = Instant::now() + Duration::from_secs(8);
    loop {
        if cancelled.load(Ordering::Acquire) || Instant::now() >= deadline {
            let _ = connection.exchange(&cancel());
            return Err(DirectorySourceError::Other);
        }
        match reply {
            Reply::DirectoryRead(Ok(Progress::Ready(value)))
            | Reply::DirectoryCreate(Ok(Progress::Ready(value))) => return Ok(value),
            Reply::DirectoryRead(Ok(Progress::Pending))
            | Reply::DirectoryCreate(Ok(Progress::Pending)) => {}
            Reply::DirectoryRead(Err(error)) | Reply::DirectoryCreate(Err(error)) => {
                return Err(source_error(error));
            }
            _ => return Err(DirectorySourceError::Other),
        }
        std::thread::sleep(Duration::from_millis(20));
        reply = connection
            .exchange(&poll())
            .map_err(|_| DirectorySourceError::ConnectionLost)?;
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
        let read = self.run_reporting(Job::Read(Operation::Home), Some(Rc::clone(&self.creates)));
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
            .map(|path| self.run(Job::Read(Operation::List { path })));
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
            .map(|path| self.run(Job::Read(Operation::Validate { path })));
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
