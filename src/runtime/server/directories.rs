//! Bounded directory reads and explicit idempotent writes outside the terminal operation lock.
use crate::platform::local_filesystem::LocalFilesystemError;
use spaceterm_server::pane::directories::*;
use std::{
    collections::BTreeMap,
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread::JoinHandle,
    time::{Duration, Instant},
};

type Id = [u8; 32];
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Kind {
    Read,
    Create,
}
enum Work {
    Read(Operation),
    Create(std::path::PathBuf),
}
enum WorkerRequest {
    Begin { id: u64, work: Work },
    Poll { id: u64 },
    Cancel { id: u64 },
}
struct Job {
    cancelled: Arc<AtomicBool>,
    result: mpsc::Receiver<Result<Value, Error>>,
    worker: JoinHandle<()>,
    deadline: Instant,
}
#[derive(Default)]
pub(super) struct DirectoryOperations {
    jobs: BTreeMap<(Id, Kind, u64), Job>,
    retiring: Vec<Job>,
}
impl DirectoryOperations {
    pub(super) fn cancel_connection(&mut self, connection: Id) {
        let keys: Vec<_> = self
            .jobs
            .keys()
            .filter(|(c, _, _)| *c == connection)
            .copied()
            .collect();
        for key in keys {
            self.cancel(key);
        }
    }
    pub(super) fn stop(&mut self) {
        let jobs = std::mem::take(&mut self.jobs);
        for (_, job) in jobs {
            job.cancelled.store(true, Ordering::Release);
            self.retiring.push(job);
        }
    }
    fn cancel(&mut self, key: (Id, Kind, u64)) {
        if let Some(job) = self.jobs.remove(&key) {
            job.cancelled.store(true, Ordering::Release);
            self.retiring.push(job);
        }
    }
    pub(super) fn tick(&mut self) {
        let expired: Vec<_> = self
            .jobs
            .iter()
            .filter(|(_, r)| Instant::now() >= r.deadline)
            .map(|(key, _)| *key)
            .collect();
        for key in expired {
            self.cancel(key);
        }
        self.retiring.retain(|job| !job.worker.is_finished());
    }
    pub(super) fn request(&mut self, connection: Id, request: Request) -> Result<Progress, Error> {
        let request = match request {
            Request::Begin { id, operation } => WorkerRequest::Begin {
                id,
                work: Work::Read(operation),
            },
            Request::Poll { id } => WorkerRequest::Poll { id },
            Request::Cancel { id } => WorkerRequest::Cancel { id },
        };
        self.work(connection, Kind::Read, request)
    }
    pub(super) fn create(
        &mut self,
        connection: Id,
        request: CreationRequest,
    ) -> Result<Progress, Error> {
        let request = match request {
            CreationRequest::Begin { id, path } => WorkerRequest::Begin {
                id,
                work: Work::Create(path),
            },
            CreationRequest::Poll { id } => WorkerRequest::Poll { id },
            CreationRequest::Cancel { id } => WorkerRequest::Cancel { id },
        };
        self.work(connection, Kind::Create, request)
    }
    fn work(
        &mut self,
        connection: Id,
        kind: Kind,
        request: WorkerRequest,
    ) -> Result<Progress, Error> {
        self.tick();
        match request {
            WorkerRequest::Cancel { id } => {
                self.cancel((connection, kind, id));
                Ok(Progress::Cancelled)
            }
            WorkerRequest::Begin { id, work } => {
                if id == 0 || self.jobs.contains_key(&(connection, kind, id)) {
                    return Err(Error::Unavailable);
                }
                if self.jobs.len() + self.retiring.len() >= 32
                    || self
                        .jobs
                        .keys()
                        .filter(|(c, _, _)| *c == connection)
                        .count()
                        >= 2
                {
                    return Err(Error::Capacity);
                }
                let cancelled = Arc::new(AtomicBool::new(false));
                let token = Arc::clone(&cancelled);
                let (sender, result) = mpsc::sync_channel(1);
                let worker = std::thread::Builder::new()
                    .name("spaceterm-directory-operation".into())
                    .spawn(move || {
                        let value = match work {
                            Work::Read(operation) => read(operation, &token),
                            Work::Create(path) => create(&path, &token),
                        };
                        if !token.load(Ordering::Acquire) {
                            let _ = sender.send(value);
                        }
                    })
                    .map_err(|_| Error::Capacity)?;
                self.jobs.insert(
                    (connection, kind, id),
                    Job {
                        cancelled,
                        result,
                        worker,
                        deadline: Instant::now() + Duration::from_secs(10),
                    },
                );
                Ok(Progress::Pending)
            }
            WorkerRequest::Poll { id } => {
                let job = self
                    .jobs
                    .get(&(connection, kind, id))
                    .ok_or(Error::Cancelled)?;
                match job.result.try_recv() {
                    Ok(value) => {
                        self.cancel((connection, kind, id));
                        value.map(Progress::Ready)
                    }
                    Err(mpsc::TryRecvError::Empty) => Ok(Progress::Pending),
                    Err(_) => {
                        self.cancel((connection, kind, id));
                        Err(Error::Unavailable)
                    }
                }
            }
        }
    }
}
impl Drop for DirectoryOperations {
    fn drop(&mut self) {
        self.stop();
    }
}
fn checked_path(path: &Path) -> Result<(), Error> {
    if !path.is_absolute()
        || path.as_os_str().len() > MAX_PATH_BYTES
        || path
            .to_str()
            .is_none_or(|s| s.chars().any(char::is_control))
    {
        return Err(Error::Unavailable);
    }
    Ok(())
}
fn read(operation: Operation, cancelled: &AtomicBool) -> Result<Value, Error> {
    let filesystem = super::local_filesystem();
    let checkpoint = || {
        if cancelled.load(Ordering::Acquire) {
            Err(Error::Cancelled)
        } else {
            Ok(())
        }
    };
    checkpoint()?;
    match operation {
        Operation::Home => {
            let home = crate::platform::app_directories::AppDirectoryEnvironment::capture()
                .home
                .ok_or(Error::Unavailable)?;
            let home = std::path::PathBuf::from(home);
            checked_path(&home)?;
            Ok(Value::Directory(home))
        }
        Operation::Ancestor { mut path } => {
            checked_path(&path)?;
            for _ in 0..32 {
                checkpoint()?;
                match filesystem.validate_directory(&path) {
                    Ok(_) => return Ok(Value::Directory(path)),
                    Err(LocalFilesystemError::Missing) if path.pop() => {}
                    Err(error) => return Err(classify(error)),
                }
            }
            Err(Error::Unavailable)
        }
        Operation::Validate { path } => {
            checked_path(&path)?;
            filesystem.validate_directory(&path).map_err(classify)?;
            checkpoint()?;
            // Values stay server paths, never a client-side ValidatedLocalDirectory.
            Ok(Value::Directory(path))
        }
        Operation::List { path } => {
            checked_path(&path)?;
            let listing = filesystem
                .list_child_directories_cancellable(&path, MAX_ROWS, MAX_LIST_BYTES, || {
                    checkpoint().map_err(|_| LocalFilesystemError::Other)
                })
                .map_err(classify)?;
            Ok(Value::Listing {
                names: listing.names,
                truncated: listing.truncated,
            })
        }
    }
}
/// Cancellation never removes created parents, even if the result is no longer observed.
fn create(path: &Path, cancelled: &AtomicBool) -> Result<Value, Error> {
    checked_path(path)?;
    if cancelled.load(Ordering::Acquire) {
        return Err(Error::Cancelled);
    }
    let filesystem = super::local_filesystem();
    let creation_error = |error| match error {
        LocalFilesystemError::Missing => Error::Unavailable,
        error => classify(error),
    };
    filesystem
        .create_directory_all(path)
        .map_err(creation_error)?;
    if cancelled.load(Ordering::Acquire) {
        return Err(Error::Cancelled);
    }
    filesystem
        .validate_directory(path)
        .map_err(creation_error)?;
    if cancelled.load(Ordering::Acquire) {
        return Err(Error::Cancelled);
    }
    Ok(Value::Directory(path.to_owned()))
}
fn classify(error: LocalFilesystemError) -> Error {
    match error {
        LocalFilesystemError::Missing => Error::Missing,
        LocalFilesystemError::NotDirectory => Error::NotDirectory,
        LocalFilesystemError::PermissionDenied => Error::PermissionDenied,
        LocalFilesystemError::Capacity => Error::Capacity,
        _ => Error::Unavailable,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn creation_rejects_invalid_paths_and_files_without_creating_children() {
        let root = std::env::temp_dir().join(format!(
            "spaceterm-create-invalid-{:x}",
            u64::from_ne_bytes(super::super::nonce().unwrap()[..8].try_into().unwrap())
        ));
        std::fs::create_dir(&root).unwrap();
        let file = root.join("file");
        std::fs::write(&file, b"fixture").unwrap();
        for path in [file.clone(), file.join("child")] {
            assert!(matches!(
                create(&path, &AtomicBool::new(false)),
                Err(Error::NotDirectory)
            ));
        }
        for path in [
            std::path::PathBuf::from("relative"),
            root.join("bad\npath"),
            root.join("x".repeat(MAX_PATH_BYTES)),
        ] {
            assert!(matches!(
                create(&path, &AtomicBool::new(false)),
                Err(Error::Unavailable)
            ));
        }
        assert_eq!(std::fs::read(&file).unwrap(), b"fixture");
        assert_eq!(std::fs::read_dir(&root).unwrap().count(), 1);
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn cancelled_creation_preserves_created_directories_and_is_connection_bound() {
        let root = std::env::temp_dir().join(format!(
            "spaceterm-create-cancel-{:x}",
            u64::from_ne_bytes(super::super::nonce().unwrap()[..8].try_into().unwrap())
        ));
        std::fs::create_dir(&root).unwrap();
        let mut operations = DirectoryOperations::default();
        assert!(matches!(
            operations.create(
                [1; 32],
                CreationRequest::Begin {
                    id: 1,
                    path: root.join("parent/child")
                }
            ),
            Ok(Progress::Pending)
        ));
        // Wait for the actual write, leaving its result unobserved before cancellation.
        let until = Instant::now() + Duration::from_secs(2);
        while !operations
            .jobs
            .get(&([1; 32], Kind::Create, 1))
            .unwrap()
            .worker
            .is_finished()
        {
            assert!(Instant::now() < until, "creation worker did not finish");
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(matches!(
            operations.create([2; 32], CreationRequest::Cancel { id: 1 }),
            Ok(Progress::Cancelled)
        ));
        assert!(operations.jobs.contains_key(&([1; 32], Kind::Create, 1)));
        assert!(matches!(
            operations.request([1; 32], Request::Poll { id: 1 }),
            Err(Error::Cancelled)
        ));
        operations.cancel_connection([1; 32]);
        assert!(matches!(
            operations.create([1; 32], CreationRequest::Poll { id: 1 }),
            Err(Error::Cancelled)
        ));
        assert!(root.join("parent/child").is_dir());
        assert!(matches!(
            create(&root.join("cancelled"), &AtomicBool::new(true)),
            Err(Error::Cancelled)
        ));
        assert!(!root.join("cancelled").exists());
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn reads_and_creation_share_the_same_per_connection_capacity() {
        let mut operations = DirectoryOperations::default();
        for id in [1, 2] {
            assert!(matches!(
                operations.request(
                    [1; 32],
                    Request::Begin {
                        id,
                        operation: Operation::Home
                    }
                ),
                Ok(Progress::Pending)
            ));
        }
        assert!(matches!(
            operations.create(
                [1; 32],
                CreationRequest::Begin {
                    id: 3,
                    path: std::path::PathBuf::from("/unused")
                }
            ),
            Err(Error::Capacity)
        ));
    }
    #[test]
    fn cancelled_reads_are_fenced_to_their_own_connection() {
        let mut reads = DirectoryOperations::default();
        for connection in [[1; 32], [2; 32]] {
            assert!(matches!(
                reads.request(
                    connection,
                    Request::Begin {
                        id: 1,
                        operation: Operation::Home
                    }
                ),
                Ok(Progress::Pending)
            ));
        }
        reads.cancel_connection([1; 32]);
        assert!(matches!(
            reads.request([1; 32], Request::Poll { id: 1 }),
            Err(Error::Cancelled)
        ));
        let until = Instant::now() + Duration::from_secs(2);
        loop {
            match reads.request([2; 32], Request::Poll { id: 1 }) {
                Ok(Progress::Ready(Value::Directory(path))) => {
                    assert!(path.is_absolute());
                    break;
                }
                Ok(Progress::Pending) if Instant::now() < until => {
                    std::thread::sleep(Duration::from_millis(5))
                }
                _ => panic!("other connection's read was cancelled"),
            }
        }
    }
    #[test]
    fn listings_bound_rows_bytes_and_visited_entries_without_creating_anything() {
        let root =
            std::env::temp_dir().join(format!("spaceterm-directory-list-{}", std::process::id()));
        std::fs::create_dir(&root).unwrap();
        for index in 0..MAX_ROWS + 1 {
            std::fs::create_dir(root.join(format!("entry-{index}"))).unwrap();
        }
        let Value::Listing { names, truncated } = read(
            Operation::List { path: root.clone() },
            &AtomicBool::new(false),
        )
        .unwrap() else {
            panic!()
        };
        assert_eq!(names.len(), MAX_ROWS);
        assert!(truncated);
        assert!(names.iter().map(String::len).sum::<usize>() <= MAX_LIST_BYTES);
        assert!(matches!(
            read(
                Operation::Validate {
                    path: root.join("missing")
                },
                &AtomicBool::new(false)
            ),
            Err(Error::Missing)
        ));
        assert!(!root.join("missing").exists());
        std::fs::remove_dir_all(root).unwrap();
    }
}
