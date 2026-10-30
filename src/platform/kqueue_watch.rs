//! Watches directories with kqueue, one descriptor each, without FSEvents.
//!
//! A macOS server watches with kqueue rather than FSEvents. kqueue reports a write to a
//! directory when an entry is added, removed or renamed in it, which is how git writes every file
//! a Git facts watch follows. Only directories are watched, never recursively.

use std::collections::HashMap;
use std::ffi::CString;
use std::os::fd::RawFd;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use crate::repository_status::{
    ChangedPaths, RepositoryWatchError, WatchDirectory, WatchSet, WatchSets,
};

/// Watched directories one set holds at most, so watches never exhaust the server's descriptors.
const MAX_WATCHES: usize = 512;
/// Identifies the event that stops the event thread.
const STOP: usize = 0;

/// Starts kqueue watch sets.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct KqueueWatchSets;

impl WatchSets for KqueueWatchSets {
    fn start(&self, changed: ChangedPaths) -> Result<Box<dyn WatchSet>, RepositoryWatchError> {
        // SAFETY: `kqueue` takes no arguments and returns a new descriptor or -1.
        let queue = unsafe { libc::kqueue() };
        if queue < 0 {
            return Err(RepositoryWatchError);
        }
        let stop = libc::kevent {
            ident: STOP,
            filter: libc::EVFILT_USER,
            flags: libc::EV_ADD | libc::EV_CLEAR,
            fflags: 0,
            data: 0,
            udata: std::ptr::null_mut(),
        };
        if register(queue, &stop).is_err() {
            // SAFETY: `queue` is a descriptor this function owns.
            unsafe { libc::close(queue) };
            return Err(RepositoryWatchError);
        }
        let watched = Arc::new(Mutex::new(HashMap::new()));
        let thread = {
            let watched = Arc::clone(&watched);
            std::thread::Builder::new()
                .name("spaceterm-git-watch".into())
                .spawn(move || listen(queue, &watched, &changed))
        };
        match thread {
            Ok(thread) => Ok(Box::new(KqueueWatchSet {
                queue,
                watched,
                thread: Some(thread),
            })),
            Err(_) => {
                // SAFETY: `queue` is a descriptor this function owns.
                unsafe { libc::close(queue) };
                Err(RepositoryWatchError)
            }
        }
    }
}

/// Every watched directory by its descriptor, which is also its event's identifier.
type Watched = Arc<Mutex<HashMap<RawFd, PathBuf>>>;

struct KqueueWatchSet {
    queue: RawFd,
    watched: Watched,
    thread: Option<JoinHandle<()>>,
}

impl WatchSet for KqueueWatchSet {
    fn watch(&mut self, directory: &WatchDirectory) -> Result<(), RepositoryWatchError> {
        let mut watched = self.watched.lock().unwrap_or_else(|e| e.into_inner());
        if directory.recursive || watched.len() >= MAX_WATCHES {
            return Err(RepositoryWatchError);
        }
        let path = CString::new(directory.path.as_os_str().as_bytes())
            .map_err(|_| RepositoryWatchError)?;
        // SAFETY: `path` is a valid C string for the whole call.
        let descriptor = unsafe {
            libc::open(
                path.as_ptr(),
                libc::O_EVTONLY | libc::O_DIRECTORY | libc::O_CLOEXEC,
            )
        };
        if descriptor < 0 {
            return Err(RepositoryWatchError);
        }
        let event = libc::kevent {
            ident: descriptor as usize,
            filter: libc::EVFILT_VNODE,
            flags: libc::EV_ADD | libc::EV_CLEAR,
            fflags: libc::NOTE_WRITE
                | libc::NOTE_DELETE
                | libc::NOTE_RENAME
                | libc::NOTE_EXTEND
                | libc::NOTE_ATTRIB
                | libc::NOTE_LINK
                | libc::NOTE_REVOKE,
            data: 0,
            udata: std::ptr::null_mut(),
        };
        if register(self.queue, &event).is_err() {
            // SAFETY: `descriptor` was opened above and is not shared.
            unsafe { libc::close(descriptor) };
            return Err(RepositoryWatchError);
        }
        watched.insert(descriptor, directory.path.clone());
        Ok(())
    }

    fn unwatch(&mut self, path: &Path) {
        let mut watched = self.watched.lock().unwrap_or_else(|e| e.into_inner());
        watched.retain(|descriptor, watched| {
            if watched != path {
                return true;
            }
            // Closing the descriptor removes its event from the queue.
            // SAFETY: the set owns every descriptor in `watched`.
            unsafe { libc::close(*descriptor) };
            false
        });
    }
}

impl Drop for KqueueWatchSet {
    fn drop(&mut self) {
        let trigger = libc::kevent {
            ident: STOP,
            filter: libc::EVFILT_USER,
            flags: 0,
            fflags: libc::NOTE_TRIGGER,
            data: 0,
            udata: std::ptr::null_mut(),
        };
        if register(self.queue, &trigger).is_ok()
            && let Some(thread) = self.thread.take()
        {
            let _ = thread.join();
        }
        let watched = std::mem::take(&mut *self.watched.lock().unwrap_or_else(|e| e.into_inner()));
        for descriptor in watched.into_keys() {
            // SAFETY: the set owns every descriptor in `watched`.
            unsafe { libc::close(descriptor) };
        }
        // SAFETY: the event thread has ended, or never waits again once the queue closes.
        unsafe { libc::close(self.queue) };
    }
}

fn register(queue: RawFd, event: &libc::kevent) -> Result<(), RepositoryWatchError> {
    // SAFETY: `event` is one valid change, and no events are read back.
    let result =
        unsafe { libc::kevent(queue, event, 1, std::ptr::null_mut(), 0, std::ptr::null()) };
    if result < 0 {
        Err(RepositoryWatchError)
    } else {
        Ok(())
    }
}

/// Reports each changed directory until the set stops.
fn listen(queue: RawFd, watched: &Watched, changed: &ChangedPaths) {
    // SAFETY: an all-zero `kevent` is a valid, empty event.
    let mut events = [unsafe { std::mem::zeroed::<libc::kevent>() }; 32];
    loop {
        // SAFETY: `events` is writable for its whole length, and no changes are passed in.
        let count = unsafe {
            libc::kevent(
                queue,
                std::ptr::null(),
                0,
                events.as_mut_ptr(),
                events.len() as libc::c_int,
                std::ptr::null(),
            )
        };
        if count < 0 {
            if std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            // The queue failed, so a change may be missed. It counts as one.
            changed(&[]);
            return;
        }
        let mut paths = Vec::new();
        for event in &events[..count as usize] {
            if event.filter == libc::EVFILT_USER {
                return;
            }
            let watched = watched.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(path) = watched.get(&(event.ident as RawFd)) {
                paths.push(path.clone());
            }
        }
        if !paths.is_empty() {
            changed(&paths);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;
    use std::time::Duration;

    use super::*;

    fn watched(directory: &Path) -> (Box<dyn WatchSet>, mpsc::Receiver<Vec<PathBuf>>) {
        let (sender, receiver) = mpsc::channel();
        let sender = Mutex::new(sender);
        let mut set = KqueueWatchSets
            .start(Box::new(move |paths: &[PathBuf]| {
                let _ = sender.lock().unwrap().send(paths.to_vec());
            }))
            .unwrap();
        set.watch(&WatchDirectory {
            path: directory.to_owned(),
            recursive: false,
        })
        .unwrap();
        (set, receiver)
    }

    #[test]
    fn a_file_renamed_into_a_watched_directory_reports_that_directory() {
        let directory = tempfile_directory("rename");
        let (_set, changes) = watched(&directory);
        std::fs::write(directory.join("HEAD.lock"), "ref: refs/heads/other\n").unwrap();
        std::fs::rename(directory.join("HEAD.lock"), directory.join("HEAD")).unwrap();
        let paths = changes.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(paths, std::slice::from_ref(&directory));
        let _ = std::fs::remove_dir_all(&directory);
    }

    #[test]
    fn writes_in_a_subdirectory_and_after_unwatching_report_nothing() {
        let directory = tempfile_directory("quiet");
        std::fs::create_dir(directory.join("cookies")).unwrap();
        let (mut set, changes) = watched(&directory);
        std::fs::write(directory.join("cookies").join("1"), "").unwrap();
        assert!(changes.recv_timeout(Duration::from_millis(300)).is_err());
        set.unwatch(&directory);
        std::fs::write(directory.join("HEAD"), "").unwrap();
        assert!(changes.recv_timeout(Duration::from_millis(300)).is_err());
        drop(set);
        let _ = std::fs::remove_dir_all(&directory);
    }

    #[test]
    fn a_recursive_watch_is_refused() {
        let directory = tempfile_directory("recursive");
        let mut set = KqueueWatchSets.start(Box::new(|_: &[PathBuf]| {})).unwrap();
        assert!(
            set.watch(&WatchDirectory {
                path: directory.clone(),
                recursive: true,
            })
            .is_err()
        );
        drop(set);
        let _ = std::fs::remove_dir_all(&directory);
    }

    fn tempfile_directory(name: &str) -> PathBuf {
        static SEQUENCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let directory = std::env::temp_dir().join(format!(
            "spaceterm-kqueue-{name}-{}-{}",
            std::process::id(),
            SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::canonicalize(directory).unwrap()
    }
}
