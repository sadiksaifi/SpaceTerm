//! Bounded directory reads and explicit idempotent writes outside the terminal operation lock.
use super::dispatch::{HostRead, SettledRead};
use crate::platform::local_filesystem::LocalFilesystemError;
use spaceterm_server::connection::Wake;
use spaceterm_server::pane::{Refusal, directories::*};
use spaceterm_server::protocol::{ReplyBody, RequestId};
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
/// One directory operation the owning server performs for a client.
pub(super) enum Work {
    Read(Operation),
    Create(std::path::PathBuf),
}
struct Job {
    cancelled: Arc<AtomicBool>,
    result: mpsc::Receiver<Result<Value, Error>>,
    worker: JoinHandle<()>,
    deadline: Instant,
}
/// Directory reads and creations, each on its own worker thread. A worker wakes the host when
/// it settles, and the host collects what settled with [`DirectoryOperations::settled`].
#[derive(Default)]
pub(super) struct DirectoryOperations {
    jobs: BTreeMap<(Id, RequestId), Job>,
    retiring: Vec<Job>,
    wake: Option<Arc<dyn Wake>>,
}
impl DirectoryOperations {
    /// Every operation started from now on wakes `wake` when it settles.
    pub(super) fn set_wake(&mut self, wake: Arc<dyn Wake>) {
        self.wake = Some(wake);
    }
    pub(super) fn cancel_connection(&mut self, connection: Id) {
        let keys: Vec<_> = self
            .jobs
            .keys()
            .filter(|(c, _)| *c == connection)
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
    /// Stops operation `key`. A creation that already started keeps what it created.
    pub(super) fn cancel(&mut self, key: (Id, RequestId)) {
        if let Some(job) = self.jobs.remove(&key) {
            job.cancelled.store(true, Ordering::Release);
            self.retiring.push(job);
        }
    }
    /// When the earliest running operation outlasts its deadline.
    pub(super) fn due(&self) -> Option<Instant> {
        self.jobs.values().map(|job| job.deadline).min()
    }
    /// Forgets workers that finished after their operation was cancelled.
    pub(super) fn tick(&mut self) {
        self.retiring.retain(|job| !job.worker.is_finished());
    }
    /// Starts `work` as operation `id` of `connection`. Its result arrives through
    /// [`Self::settled`].
    pub(super) fn begin(&mut self, connection: Id, id: RequestId, work: Work) -> Result<(), Error> {
        self.tick();
        if self.jobs.contains_key(&(connection, id)) {
            return Err(Error::Unavailable);
        }
        if self.jobs.len() + self.retiring.len() >= 32
            || self.jobs.keys().filter(|(c, _)| *c == connection).count() >= 2
        {
            return Err(Error::Capacity);
        }
        let cancelled = Arc::new(AtomicBool::new(false));
        let token = Arc::clone(&cancelled);
        let wake = self.wake.clone();
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
                    if let Some(wake) = wake {
                        wake.wake();
                    }
                }
            })
            .map_err(|_| Error::Capacity)?;
        self.jobs.insert(
            (connection, id),
            Job {
                cancelled,
                result,
                worker,
                deadline: Instant::now() + Duration::from_secs(10),
            },
        );
        Ok(())
    }
    /// The operations that finished, and those that outlasted their deadline as
    /// [`Error::Cancelled`], each under its connection and id.
    pub(super) fn settled(&mut self) -> Vec<((Id, RequestId), Result<Value, Error>)> {
        let now = Instant::now();
        let mut settled = Vec::new();
        let mut expired = Vec::new();
        self.jobs.retain(|key, job| match job.result.try_recv() {
            Ok(value) => {
                settled.push((*key, value));
                false
            }
            Err(mpsc::TryRecvError::Empty) if now < job.deadline => true,
            Err(mpsc::TryRecvError::Empty) => {
                expired.push(*key);
                true
            }
            Err(mpsc::TryRecvError::Disconnected) => {
                settled.push((*key, Err(Error::Unavailable)));
                false
            }
        });
        for key in expired {
            self.cancel(key);
            settled.push((key, Err(Error::Cancelled)));
        }
        settled
    }
    /// Starts a directory read or creation. `None` means it runs and settles through
    /// [`Self::settled_reads`].
    pub(super) fn read(
        &mut self,
        connection: Id,
        id: RequestId,
        read: HostRead,
    ) -> Option<Result<ReplyBody, Refusal>> {
        let work = match read {
            HostRead::Directory(operation) => Work::Read(operation),
            HostRead::Creation(creation) => Work::Create(creation.path),
            HostRead::Worktree(_) => return Some(Err(Refusal::Unsupported)),
        };
        let started = self.begin(connection, id, work);
        started
            .err()
            .map(|error| Ok(ReplyBody::Directory(Err(error))))
    }
    pub(super) fn settled_reads(&mut self) -> Vec<SettledRead> {
        self.settled()
            .into_iter()
            .map(|((connection, id), result)| SettledRead {
                connection,
                id,
                reply: ReplyBody::Directory(result),
            })
            .collect()
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
        Operation::Match { path, candidates } => {
            if candidates.len() > spaceterm_server::hierarchy::MAX_WORKSPACES {
                return Err(Error::Capacity);
            }
            checked_path(&path)?;
            for candidate in &candidates {
                checked_path(candidate)?;
            }
            let chosen = filesystem.validate_directory(&path).map_err(classify)?;
            checkpoint()?;
            for (index, candidate) in candidates.iter().enumerate() {
                checkpoint()?;
                let candidate = match filesystem.validate_directory(candidate) {
                    Ok(candidate) => candidate,
                    Err(
                        LocalFilesystemError::Missing
                        | LocalFilesystemError::NotDirectory
                        | LocalFilesystemError::PermissionDenied,
                    ) => continue,
                    Err(error) => return Err(classify(error)),
                };
                checkpoint()?;
                if candidate.identity() == chosen.identity() {
                    return Ok(Value::Match {
                        index: Some(u16::try_from(index).map_err(|_| Error::Capacity)?),
                    });
                }
                // Each candidate's retained identity is released before opening the next.
            }
            Ok(Value::Match { index: None })
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
    /// Operations that wake `woken` when one settles.
    fn operations() -> (DirectoryOperations, mpsc::Receiver<()>) {
        let (sender, woken) = mpsc::channel();
        let mut operations = DirectoryOperations::default();
        operations.set_wake(Arc::new(move || {
            let _ = sender.send(());
        }));
        (operations, woken)
    }
    /// Waits for operation `key` to settle.
    fn settle(
        operations: &mut DirectoryOperations,
        woken: &mpsc::Receiver<()>,
        key: (Id, RequestId),
    ) -> Result<Value, Error> {
        loop {
            if let Some((_, result)) = operations
                .settled()
                .into_iter()
                .find(|(settled, _)| *settled == key)
            {
                return result;
            }
            woken
                .recv_timeout(Duration::from_secs(3))
                .expect("directory operation did not settle");
        }
    }
    fn directory_match(operation: Operation) -> Result<Value, Error> {
        let (mut operations, woken) = operations();
        operations
            .begin([1; 32], RequestId(1), Work::Read(operation))
            .unwrap();
        settle(&mut operations, &woken, ([1; 32], RequestId(1)))
    }

    #[test]
    fn directory_matching_resolves_real_symlink_identity_in_candidate_order() {
        let root = std::env::temp_dir().join(format!(
            "spaceterm-directory-match-{:x}",
            u64::from_ne_bytes(super::super::nonce().unwrap()[..8].try_into().unwrap())
        ));
        std::fs::create_dir(&root).unwrap();
        let chosen = root.join("real");
        let unrelated = root.join("other/real");
        let alias = root.join("alias");
        std::fs::create_dir(&chosen).unwrap();
        std::fs::create_dir_all(&unrelated).unwrap();
        std::os::unix::fs::symlink(&chosen, &alias).unwrap();
        let file = root.join("file");
        std::fs::write(&file, b"fixture").unwrap();
        let result = directory_match(Operation::Match {
            path: chosen.clone(),
            candidates: vec![
                root.join("missing"),
                file,
                unrelated.clone(),
                alias.clone(),
                chosen.clone(),
            ],
        });
        assert!(matches!(result, Ok(Value::Match { index: Some(3) })));
        // The chosen spelling can itself be the symlink; the first physical match wins.
        assert!(matches!(
            directory_match(Operation::Match {
                path: alias.clone(),
                candidates: vec![chosen.clone(), alias.clone()],
            }),
            Ok(Value::Match { index: Some(0) })
        ));
        assert!(matches!(
            directory_match(Operation::Match {
                path: alias.clone(),
                candidates: vec![unrelated],
            }),
            Ok(Value::Match { index: None })
        ));
        assert!(std::fs::symlink_metadata(&alias).unwrap().is_symlink());
        assert!(chosen.is_dir());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn directory_matching_rejects_bounds_invalid_paths_and_unavailable_chosen_directories() {
        let root = std::env::temp_dir().join(format!(
            "spaceterm-directory-match-invalid-{:x}",
            u64::from_ne_bytes(super::super::nonce().unwrap()[..8].try_into().unwrap())
        ));
        std::fs::create_dir(&root).unwrap();
        assert!(matches!(
            directory_match(Operation::Match {
                path: root.clone(),
                candidates: vec![root.clone(); spaceterm_server::hierarchy::MAX_WORKSPACES + 1],
            }),
            Err(Error::Capacity)
        ));
        for bad in [
            std::path::PathBuf::from("relative"),
            root.join("bad\npath"),
            root.join("x".repeat(MAX_PATH_BYTES)),
        ] {
            assert!(matches!(
                directory_match(Operation::Match {
                    path: root.clone(),
                    candidates: vec![bad.clone()],
                }),
                Err(Error::Unavailable)
            ));
            assert!(matches!(
                directory_match(Operation::Match {
                    path: bad,
                    candidates: vec![],
                }),
                Err(Error::Unavailable)
            ));
        }
        assert!(matches!(
            directory_match(Operation::Match {
                path: root.join("missing"),
                candidates: vec![root.clone()],
            }),
            Err(Error::Missing)
        ));
        let file = root.join("file");
        std::fs::write(&file, b"fixture").unwrap();
        assert!(matches!(
            directory_match(Operation::Match {
                path: file,
                candidates: vec![root.clone()],
            }),
            Err(Error::NotDirectory)
        ));
        assert!(matches!(
            read(
                Operation::Match {
                    path: root.clone(),
                    candidates: vec![root.clone()]
                },
                &AtomicBool::new(true)
            ),
            Err(Error::Cancelled)
        ));
        assert!(matches!(
            directory_match(Operation::Match {
                path: root.clone(),
                candidates: vec![root.clone(); spaceterm_server::hierarchy::MAX_WORKSPACES],
            }),
            Ok(Value::Match { index: Some(0) })
        ));
        std::fs::remove_dir_all(root).unwrap();
    }

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
        let (mut operations, _woken) = operations();
        operations
            .begin(
                [1; 32],
                RequestId(1),
                Work::Create(root.join("parent/child")),
            )
            .unwrap();
        // Wait for the actual write, leaving its result unobserved before cancellation.
        let until = Instant::now() + Duration::from_secs(2);
        while !operations
            .jobs
            .get(&([1; 32], RequestId(1)))
            .unwrap()
            .worker
            .is_finished()
        {
            assert!(Instant::now() < until, "creation worker did not finish");
            std::thread::sleep(Duration::from_millis(5));
        }
        operations.cancel(([2; 32], RequestId(1)));
        assert!(operations.jobs.contains_key(&([1; 32], RequestId(1))));
        operations.cancel_connection([1; 32]);
        assert!(operations.settled().is_empty());
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
            operations
                .begin([1; 32], RequestId(id), Work::Read(Operation::Home))
                .unwrap();
        }
        assert!(matches!(
            operations.begin(
                [1; 32],
                RequestId(3),
                Work::Create(std::path::PathBuf::from("/unused"))
            ),
            Err(Error::Capacity)
        ));
    }
    #[test]
    fn cancelled_reads_are_fenced_to_their_own_connection() {
        let (mut reads, woken) = operations();
        for connection in [[1; 32], [2; 32]] {
            reads
                .begin(connection, RequestId(1), Work::Read(Operation::Home))
                .unwrap();
        }
        reads.cancel_connection([1; 32]);
        let Ok(Value::Directory(path)) = settle(&mut reads, &woken, ([2; 32], RequestId(1))) else {
            panic!("other connection's read was cancelled");
        };
        assert!(path.is_absolute());
        assert!(reads.settled().is_empty());
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
