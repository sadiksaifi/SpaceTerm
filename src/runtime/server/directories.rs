//! Bounded, cancellable reads on the owning server, outside its terminal operation lock.
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
struct Read {
    cancelled: Arc<AtomicBool>,
    result: mpsc::Receiver<Result<Value, Error>>,
    worker: JoinHandle<()>,
    deadline: Instant,
}
#[derive(Default)]
pub(super) struct DirectoryReads {
    reads: BTreeMap<(Id, u64), Read>,
    retiring: Vec<Read>,
}
impl DirectoryReads {
    pub(super) fn cancel_connection(&mut self, connection: Id) {
        let keys: Vec<_> = self
            .reads
            .keys()
            .filter(|(c, _)| *c == connection)
            .copied()
            .collect();
        for key in keys {
            self.cancel(key);
        }
    }
    pub(super) fn stop(&mut self) {
        let reads = std::mem::take(&mut self.reads);
        for (_, read) in reads {
            read.cancelled.store(true, Ordering::Release);
            self.retiring.push(read);
        }
    }
    fn cancel(&mut self, key: (Id, u64)) {
        if let Some(read) = self.reads.remove(&key) {
            read.cancelled.store(true, Ordering::Release);
            self.retiring.push(read);
        }
    }
    pub(super) fn tick(&mut self) {
        let expired: Vec<_> = self
            .reads
            .iter()
            .filter(|(_, r)| Instant::now() >= r.deadline)
            .map(|(key, _)| *key)
            .collect();
        for key in expired {
            self.cancel(key);
        }
        self.retiring.retain(|read| !read.worker.is_finished());
    }
    pub(super) fn request(&mut self, connection: Id, request: Request) -> Result<Progress, Error> {
        self.tick();
        match request {
            Request::Cancel { id } => {
                self.cancel((connection, id));
                Ok(Progress::Cancelled)
            }
            Request::Begin { id, operation } => {
                if id == 0 || self.reads.contains_key(&(connection, id)) {
                    return Err(Error::Unavailable);
                }
                if self.reads.len() + self.retiring.len() >= 32
                    || self.reads.keys().filter(|(c, _)| *c == connection).count() >= 2
                {
                    return Err(Error::Capacity);
                }
                let cancelled = Arc::new(AtomicBool::new(false));
                let token = Arc::clone(&cancelled);
                let (sender, result) = mpsc::sync_channel(1);
                let worker = std::thread::Builder::new()
                    .name("spaceterm-directory-read".into())
                    .spawn(move || {
                        let value = read(operation, &token);
                        if !token.load(Ordering::Acquire) {
                            let _ = sender.send(value);
                        }
                    })
                    .map_err(|_| Error::Capacity)?;
                self.reads.insert(
                    (connection, id),
                    Read {
                        cancelled,
                        result,
                        worker,
                        deadline: Instant::now() + Duration::from_secs(10),
                    },
                );
                Ok(Progress::Pending)
            }
            Request::Poll { id } => {
                let read = self.reads.get(&(connection, id)).ok_or(Error::Cancelled)?;
                match read.result.try_recv() {
                    Ok(value) => {
                        self.cancel((connection, id));
                        value.map(Progress::Ready)
                    }
                    Err(mpsc::TryRecvError::Empty) => Ok(Progress::Pending),
                    Err(_) => {
                        self.cancel((connection, id));
                        Err(Error::Unavailable)
                    }
                }
            }
        }
    }
}
impl Drop for DirectoryReads {
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
    fn cancelled_reads_are_fenced_to_their_own_connection() {
        let mut reads = DirectoryReads::default();
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
