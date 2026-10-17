//! Reads paths through the authenticated owning server; never validates them on this client.
use super::*;
use crate::{
    domain::{RemoteDirectory, RemoteDirectoryIdentity},
    local_server::LocalServer,
};
use spaceterm_server::pane::{Reply, Request as PaneRequest, directories::*};
use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

pub(crate) struct ServerDirectorySource {
    server: LocalServer,
    original: PathBuf,
    available: Rc<dyn Fn() -> bool>,
    initial: Rc<std::cell::RefCell<Option<PickerPath>>>,
    executor: gpui::BackgroundExecutor,
    home: Rc<std::cell::RefCell<Option<PathBuf>>>,
}
impl ServerDirectorySource {
    pub(crate) fn new(
        server: LocalServer,
        executor: gpui::BackgroundExecutor,
        original: PathBuf,
        available: Rc<dyn Fn() -> bool>,
    ) -> Self {
        Self {
            server,
            original,
            available,
            initial: Rc::default(),
            executor,
            home: Rc::default(),
        }
    }
    fn run(&self, operation: Operation) -> SourceFuture<Value> {
        if !(self.available)() {
            return Box::pin(async { Err(DirectorySourceError::ConnectionLost) });
        }
        let available = Rc::clone(&self.available);
        let server = self.server.clone();
        let cancelled = Arc::new(AtomicBool::new(false));
        let guard = CancelRead(Arc::clone(&cancelled));
        let task = self.executor.spawn(async move {
            let mut connection = server
                .connect_pane()
                .map_err(|_| DirectorySourceError::ConnectionLost)?;
            if !connection.supports_directory_recovery() {
                return Err(DirectorySourceError::Other);
            }
            let request = |request| PaneRequest::DirectoryRead(request);
            let mut reply = connection
                .exchange(&request(Request::Begin { id: 1, operation }))
                .map_err(|_| DirectorySourceError::ConnectionLost)?;
            let deadline = Instant::now() + Duration::from_secs(8);
            loop {
                if cancelled.load(Ordering::Acquire) || Instant::now() >= deadline {
                    let _ = connection.exchange(&request(Request::Cancel { id: 1 }));
                    return Err(DirectorySourceError::Other);
                }
                match reply {
                    Reply::DirectoryRead(Ok(Progress::Ready(value))) => return Ok(value),
                    Reply::DirectoryRead(Ok(Progress::Pending)) => {}
                    Reply::DirectoryRead(Err(error)) => return Err(source_error(error)),
                    _ => return Err(DirectorySourceError::Other),
                }
                std::thread::sleep(Duration::from_millis(20));
                reply = connection
                    .exchange(&request(Request::Poll { id: 1 }))
                    .map_err(|_| DirectorySourceError::ConnectionLost)?;
            }
        });
        Box::pin(async move {
            let _guard = guard;
            let result = task.await;
            if available() {
                result
            } else {
                Err(DirectorySourceError::ConnectionLost)
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
impl DirectorySource for ServerDirectorySource {
    fn machine_name(&self) -> Option<&str> {
        None
    }
    fn allows_creation(&self) -> bool {
        false
    }
    fn initial_query(&self) -> Option<PickerPath> {
        self.initial.borrow().clone()
    }
    fn selection_label(&self) -> &'static str {
        "Choose"
    }
    fn discover_home(&self) -> SourceFuture<PickerPath> {
        let read = self.run(Operation::Home);
        let home = Rc::clone(&self.home);
        let ancestor = self.run(Operation::Ancestor {
            path: self.original.clone(),
        });
        let initial = Rc::clone(&self.initial);
        Box::pin(async move {
            match read.await? {
                Value::Directory(path) => {
                    let value = PickerPath::new(
                        path.to_str().ok_or(DirectorySourceError::Other)?.to_owned(),
                    )
                    .map_err(|_| DirectorySourceError::Other)?;
                    *home.borrow_mut() = Some(path);
                    if let Ok(Value::Directory(path)) = ancestor.await {
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
            .map(|path| self.run(Operation::List { path }));
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
            .map(|path| self.run(Operation::Validate { path }));
        Box::pin(async move {
            match read?.await {
                Ok(Value::Directory(_)) => Ok(ExactPathState::ReadableDirectory),
                Err(DirectorySourceError::Missing) => Ok(ExactPathState::Missing),
                Err(error) => Err(error),
                _ => Err(DirectorySourceError::Other),
            }
        })
    }
    fn create_directory_recursively(&self, _: PickerPath) -> SourceFuture<()> {
        Box::pin(async { Err(DirectorySourceError::Other) })
    }
    fn pin(&self, directory: PickerPath) -> SourceFuture<PinnedDirectory> {
        let read = self
            .path(directory)
            .map(|path| self.run(Operation::Validate { path }));
        Box::pin(async move {
            match read?.await? {
                Value::Directory(path) => {
                    let path = path.to_str().ok_or(DirectorySourceError::Other)?.to_owned();
                    // A remote directory value carries no client-local authority, even on this Mac.
                    Ok(PinnedDirectory::Remote {
                        directory: RemoteDirectory::new(path.clone())
                            .map_err(|_| DirectorySourceError::Other)?,
                        identity: RemoteDirectoryIdentity::new(path)
                            .map_err(|_| DirectorySourceError::Other)?,
                    })
                }
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
