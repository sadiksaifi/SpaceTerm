//! Watches git directories through the Operating System's file events.

use notify::event::{AccessKind, AccessMode, EventKind};
use notify::{RecommendedWatcher, RecursiveMode, Watcher as _};

use crate::repository_status::{
    RepositoryWatch, RepositoryWatchError, RepositoryWatcher, WatchDirectory,
};

/// Reports changes below git directories through `notify`.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct NotifyRepositoryWatcher;

impl NotifyRepositoryWatcher {
    pub(crate) const fn new() -> Self {
        Self
    }
}

impl RepositoryWatcher for NotifyRepositoryWatcher {
    fn watch(
        &self,
        directories: Vec<WatchDirectory>,
        changed: Box<dyn Fn() + Send + Sync>,
    ) -> Result<Box<dyn RepositoryWatch>, RepositoryWatchError> {
        let mut watcher =
            notify::recommended_watcher(move |event: notify::Result<notify::Event>| match event {
                Ok(event) => {
                    if is_change(event.kind) {
                        changed();
                    }
                }
                // A lost or overflowed event may hide a change, so it counts as one.
                Err(_) => changed(),
            })
            .map_err(|_| RepositoryWatchError)?;
        for directory in directories {
            let mode = if directory.recursive {
                RecursiveMode::Recursive
            } else {
                RecursiveMode::NonRecursive
            };
            watcher
                .watch(&directory.path, mode)
                .map_err(|_| RepositoryWatchError)?;
        }
        Ok(Box::new(NotifyRepositoryWatch(watcher)))
    }
}

/// Repository Status's own git reads open files in the watched directories, and some hosts report
/// those opens. Only a close after writing is a change; every other access is not.
fn is_change(kind: EventKind) -> bool {
    match kind {
        EventKind::Access(AccessKind::Close(AccessMode::Write)) => true,
        EventKind::Access(_) => false,
        _ => true,
    }
}

/// Dropping the watcher stops its event stream.
struct NotifyRepositoryWatch(#[allow(dead_code, reason = "held for its drop")] RecommendedWatcher);

impl RepositoryWatch for NotifyRepositoryWatch {}

#[cfg(all(test, feature = "native-tests"))]
mod tests {
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::mpsc;
    use std::time::Duration;

    use super::*;
    use crate::platform::unix_adapter_tests::short_temporary_root;

    struct Fixture(PathBuf);

    impl Fixture {
        fn new() -> Self {
            static SEQUENCE: AtomicU64 = AtomicU64::new(0);
            let root = short_temporary_root().join(format!(
                "spaceterm-watch-{}-{}",
                std::process::id(),
                SEQUENCE.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir(&root).unwrap();
            Self(root)
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn watch(directories: Vec<WatchDirectory>) -> (Box<dyn RepositoryWatch>, mpsc::Receiver<()>) {
        let (sender, receiver) = mpsc::channel();
        let sender = std::sync::Mutex::new(sender);
        let watch = NotifyRepositoryWatcher::new()
            .watch(
                directories,
                Box::new(move || {
                    let _ = sender.lock().unwrap().send(());
                }),
            )
            .unwrap();
        // Some hosts deliver events from just before the watch began; they are not under test.
        std::thread::sleep(Duration::from_millis(200));
        while receiver.try_recv().is_ok() {}
        (watch, receiver)
    }

    #[test]
    fn unix_repository_watch_reports_a_file_write_in_a_watched_directory() {
        let fixture = Fixture::new();
        let (_watch, changes) = watch(vec![WatchDirectory {
            path: fixture.0.clone(),
            recursive: false,
        }]);

        std::fs::write(fixture.0.join("HEAD"), "ref: refs/heads/other\n").unwrap();

        assert!(changes.recv_timeout(Duration::from_secs(5)).is_ok());
    }

    #[test]
    fn unix_repository_watch_reports_nested_writes_below_a_recursive_directory() {
        let fixture = Fixture::new();
        let refs = fixture.0.join("refs/heads");
        std::fs::create_dir_all(&refs).unwrap();
        let (_watch, changes) = watch(vec![WatchDirectory {
            path: fixture.0.join("refs"),
            recursive: true,
        }]);

        std::fs::write(refs.join("main"), "0123456789abcdef\n").unwrap();

        assert!(changes.recv_timeout(Duration::from_secs(5)).is_ok());
    }

    #[test]
    fn unix_repository_watch_ignores_reads_of_watched_files() {
        let fixture = Fixture::new();
        let head = fixture.0.join("HEAD");
        std::fs::write(&head, "ref: refs/heads/main\n").unwrap();
        let (_watch, changes) = watch(vec![WatchDirectory {
            path: fixture.0.clone(),
            recursive: false,
        }]);

        std::fs::read(&head).unwrap();

        assert!(changes.recv_timeout(Duration::from_millis(500)).is_err());
    }

    #[test]
    fn unix_repository_watch_stops_when_dropped() {
        let fixture = Fixture::new();
        let (watch, changes) = watch(vec![WatchDirectory {
            path: fixture.0.clone(),
            recursive: false,
        }]);

        drop(watch);
        std::fs::write(fixture.0.join("HEAD"), "ref: refs/heads/other\n").unwrap();

        // The stopped watcher released the callback, which held the only sender.
        assert_eq!(
            changes.recv_timeout(Duration::from_secs(5)),
            Err(mpsc::RecvTimeoutError::Disconnected)
        );
    }

    #[test]
    fn unix_repository_watch_fails_for_a_missing_directory() {
        let fixture = Fixture::new();

        let result = NotifyRepositoryWatcher::new().watch(
            vec![WatchDirectory {
                path: fixture.0.join("missing"),
                recursive: false,
            }],
            Box::new(|| {}),
        );

        assert!(matches!(result, Err(RepositoryWatchError)));
    }
}
