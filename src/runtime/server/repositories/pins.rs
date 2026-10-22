//! Resolve physical pins off the terminal request lock, with at most one filesystem worker.
use super::*;
use spaceterm_server::hierarchy::Inventory;

trait PinResolver: Send + Sync {
    fn holds(
        &self,
        pins: &BTreeSet<PathBuf>,
        root: &Path,
    ) -> Result<bool, spaceterm_server::worktree::Error>;
}

struct HostPins;
impl PinResolver for HostPins {
    fn holds(
        &self,
        pins: &BTreeSet<PathBuf>,
        root: &Path,
    ) -> Result<bool, spaceterm_server::worktree::Error> {
        super::super::shared::pinned_in_worktree(pins.iter().map(PathBuf::as_path), root)
    }
}

#[derive(Default, Clone, PartialEq, Eq)]
struct Interests {
    pins: BTreeSet<PathBuf>,
    roots: BTreeSet<PathBuf>,
}

struct Resolution {
    interests: Interests,
    cancellation: SshCancellationToken,
    handle: JoinHandle<Option<Result<Vec<PathBuf>, ()>>>,
}

pub(super) struct PinMembership {
    interests: Interests,
    worker: Option<Resolution>,
    next_check: Option<Instant>,
    resolver: Arc<dyn PinResolver>,
    seen_revision: Option<u64>,
}

impl Default for PinMembership {
    fn default() -> Self {
        Self::new(Arc::new(HostPins))
    }
}

impl Drop for PinMembership {
    fn drop(&mut self) {
        // A filesystem call may wait on a mount. It holds no server state or request lock and
        // cannot be interrupted portably; cancel the remaining work without joining that call.
        if let Some(worker) = &self.worker {
            worker.cancellation.cancel();
        }
    }
}

impl PinMembership {
    fn new(resolver: Arc<dyn PinResolver>) -> Self {
        Self {
            interests: Interests::default(),
            worker: None,
            next_check: None,
            resolver,
            seen_revision: None,
        }
    }

    pub(super) fn update(&mut self, inventory: &Inventory, snapshot: &mut Snapshot, active: bool) {
        let pins = inventory
            .workspaces
            .iter()
            .filter_map(|w| w.pinned_directory.clone())
            .collect();
        let mut changed = self.interests.pins != pins;
        self.interests.pins = pins;
        if self.seen_revision != Some(snapshot.revision) {
            let roots = snapshot
                .directories
                .iter()
                .filter_map(|facts| facts.worktrees.as_ref())
                .flat_map(|list| &list.worktrees)
                .map(|record| record.root.clone())
                .collect();
            changed |= self.interests.roots != roots;
            self.interests.roots = roots;
            self.seen_revision = Some(snapshot.revision);
        }
        if changed {
            self.next_check = None;
            if let Some(worker) = &self.worker {
                worker.cancellation.cancel();
            }
            let state = if self.interests.pins.is_empty() || self.interests.roots.is_empty() {
                PinCheckState::Ready
            } else {
                PinCheckState::Checking
            };
            publish(snapshot, Vec::new(), state);
        }
        if self
            .worker
            .as_ref()
            .is_some_and(|worker| worker.handle.is_finished())
        {
            let worker = self.worker.take().unwrap();
            let current = worker.interests == self.interests && !worker.cancellation.is_cancelled();
            let result = worker.handle.join().unwrap_or(Some(Err(())));
            if current {
                match result {
                    Some(Ok(pinned)) => publish(snapshot, pinned, PinCheckState::Ready),
                    Some(Err(())) => publish(snapshot, Vec::new(), PinCheckState::Unavailable),
                    None => (),
                }
                self.next_check = Some(Instant::now() + REFRESH);
            }
        }
        if self.interests.pins.is_empty() || self.interests.roots.is_empty() {
            publish(snapshot, Vec::new(), PinCheckState::Ready);
            return;
        }
        if !active {
            if let Some(worker) = &self.worker {
                worker.cancellation.cancel();
            }
            return;
        }
        if self.worker.is_some() || self.next_check.is_some_and(|next| Instant::now() < next) {
            return;
        }
        let cancellation = SshCancellationToken::default();
        let token = cancellation.clone();
        let interests = self.interests.clone();
        let roots = interests.roots.clone();
        // The existing removal policy resolves only pin paths. Terminal content, sessions and
        // the host itself never enter this worker.
        let pins = interests.pins.clone();
        let resolver = Arc::clone(&self.resolver);
        match std::thread::Builder::new()
            .name("spaceterm-worktree-pins".into())
            .spawn(move || {
                let mut pinned = Vec::new();
                for root in roots {
                    if token.is_cancelled() {
                        return None;
                    }
                    match resolver.holds(&pins, &root) {
                        Ok(true) => pinned.push(root),
                        Ok(false) => (),
                        Err(_) => return Some(Err(())),
                    }
                }
                Some(Ok(pinned))
            }) {
            Ok(handle) => {
                self.worker = Some(Resolution {
                    interests,
                    cancellation,
                    handle,
                })
            }
            Err(_) => {
                publish(snapshot, Vec::new(), PinCheckState::Unavailable);
                self.next_check = Some(Instant::now() + REFRESH);
            }
        }
    }
}

fn publish(snapshot: &mut Snapshot, pinned: Vec<PathBuf>, state: PinCheckState) {
    if pinned == snapshot.pinned_worktrees && state == snapshot.pin_check {
        return;
    }
    let previous = std::mem::replace(&mut snapshot.pinned_worktrees, pinned);
    let old_state = std::mem::replace(&mut snapshot.pin_check, state);
    if serde_json::to_vec(snapshot).is_ok_and(|bytes| bytes.len() <= MAX_SNAPSHOT_BYTES) {
        snapshot.revision = snapshot.revision.saturating_add(1);
    } else {
        snapshot.pinned_worktrees = previous;
        snapshot.pin_check = old_state;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct DelayedFilesystem {
        started: mpsc::SyncSender<()>,
        release: Mutex<mpsc::Receiver<()>>,
        calls: AtomicUsize,
    }
    impl PinResolver for DelayedFilesystem {
        fn holds(
            &self,
            _: &BTreeSet<PathBuf>,
            _: &Path,
        ) -> Result<bool, spaceterm_server::worktree::Error> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            let _ = self.started.try_send(());
            self.release
                .lock()
                .unwrap()
                .recv_timeout(Duration::from_secs(3))
                .map_err(|_| spaceterm_server::worktree::Error::Unavailable)?;
            Ok(true)
        }
    }

    #[test]
    fn slow_filesystem_does_not_block_publication_or_restore_an_unpinned_restriction() {
        let (started_tx, started) = mpsc::sync_channel(1);
        let (release, release_rx) = mpsc::sync_channel(1);
        let filesystem = Arc::new(DelayedFilesystem {
            started: started_tx,
            release: Mutex::new(release_rx),
            calls: AtomicUsize::new(0),
        });
        let mut membership = PinMembership::new(filesystem.clone());
        let mut state = State::empty();
        state
            .ensure_first_workspace([1; 32], Path::new("/server/main"))
            .unwrap();
        state.inventory.workspaces[0].pinned_directory = Some("/server/pin-alias".into());
        let mut snapshot = Snapshot {
            boot: [1; 32],
            revision: 0,
            git: GitToolStatus::Unknown,
            pinned_worktrees: Vec::new(),
            pin_check: PinCheckState::Ready,
            directories: vec![DirectoryFacts {
                directory: "/server/main".into(),
                repository: None,
                failure: None,
                worktrees: Some(WorktreeInventory {
                    main_root: "/server/main".into(),
                    current: Some(0),
                    worktrees: vec![WorktreeRecord {
                        root: "/server/main".into(),
                        head: WorktreeHead::Branch("main".into()),
                        locked: false,
                        missing: false,
                    }],
                }),
            }],
        };
        let start = Instant::now();
        membership.update(&state.inventory, &mut snapshot, true);
        assert!(
            start.elapsed() < Duration::from_millis(250),
            "filesystem work must not run in synchronization"
        );
        started.recv_timeout(Duration::from_secs(1)).unwrap();
        assert_eq!(snapshot.pin_check, PinCheckState::Checking);
        assert!(snapshot.pinned_worktrees.is_empty());
        // An inventory change remains publishable while the old physical check is blocked.
        state.inventory.workspaces[0].pinned_directory = None;
        membership.update(&state.inventory, &mut snapshot, true);
        assert_eq!(snapshot.pin_check, PinCheckState::Ready);
        release.send(()).unwrap();
        let until = Instant::now() + Duration::from_secs(1);
        while membership.worker.is_some() && Instant::now() < until {
            membership.update(&state.inventory, &mut snapshot, true);
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(membership.worker.is_none());
        assert!(
            snapshot.pinned_worktrees.is_empty(),
            "a stale physical result grants no restriction to the new inventory"
        );
        state.inventory.workspaces[0].pinned_directory = Some("/server/another-pin".into());
        membership.update(&state.inventory, &mut snapshot, false);
        assert!(
            membership.worker.is_none(),
            "disconnected subscribers start no filesystem work"
        );
        assert_eq!(filesystem.calls.load(Ordering::Relaxed), 1);
    }
}
