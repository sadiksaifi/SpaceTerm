//! App-scoped Worktree listings shared by every window.
//!
//! The store owns the one [`WorktreeCatalog`], runs its reads on dedicated threads, and tells
//! sidebar rows when their Worktrees changed. Programs run only through the Repository Status
//! adapters host composition injected.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use gpui::{App, AppContext as _, Context, Entity, EventEmitter, Global, Task};

use super::repository_status_store::{ReadSpawner, RepositoryStatusAdapters, thread_spawner};
use crate::repository_status::tool_check::check_tools;
use crate::repository_status::{GitToolStatus, RepositoryWatch, ToolInventory};
use crate::ssh::cancellation::SshCancellationToken;
#[cfg(test)]
use crate::worktrees::WorktreeSnapshot;
use crate::worktrees::catalog::{
    CatalogEffect, CatalogUpdate, WorktreeCatalog, WorktreeInterestId, WorktreeListing,
};
use crate::worktrees::git::LocalWorktreeGit;

/// Some interest's Worktrees changed.
pub(crate) struct WorktreesChanged;

pub(crate) struct InstalledWorktrees(pub(crate) Entity<WorktreeStore>);

impl Global for InstalledWorktrees {}

impl InstalledWorktrees {
    pub(crate) fn store(cx: &App) -> Option<Entity<WorktreeStore>> {
        cx.try_global::<Self>().map(|installed| installed.0.clone())
    }
}

pub(crate) fn install(adapters: RepositoryStatusAdapters, cx: &mut App) {
    let store = cx.new(|cx| WorktreeStore::new(adapters, thread_spawner(), cx));
    cx.set_global(InstalledWorktrees(store));
}

#[derive(Default)]
struct GitState {
    checking: bool,
    checked: bool,
    git: Option<Arc<LocalWorktreeGit>>,
    /// Reads that arrived before the tool check finished.
    waiting: Vec<CatalogEffect>,
}

pub(crate) struct WorktreeStore {
    catalog: WorktreeCatalog,
    adapters: RepositoryStatusAdapters,
    spawn: ReadSpawner,
    git: GitState,
    watches: HashMap<PathBuf, Box<dyn RepositoryWatch>>,
    watch_events: async_channel::Sender<PathBuf>,
    next_interest: u64,
    cancellation: SshCancellationToken,
    _watch_task: Task<()>,
}

impl EventEmitter<WorktreesChanged> for WorktreeStore {}

impl Drop for WorktreeStore {
    fn drop(&mut self) {
        self.cancellation.cancel();
    }
}

impl WorktreeStore {
    pub(crate) fn new(
        adapters: RepositoryStatusAdapters,
        spawn: ReadSpawner,
        cx: &mut Context<Self>,
    ) -> Self {
        let (watch_events, watch_receiver) = async_channel::unbounded::<PathBuf>();
        let watch_task = cx.spawn(async move |store, cx| {
            while let Ok(common) = watch_receiver.recv().await {
                if store
                    .update(cx, |store, cx| store.repository_changed(&common, cx))
                    .is_err()
                {
                    break;
                }
            }
        });
        Self {
            catalog: WorktreeCatalog::default(),
            adapters,
            spawn,
            git: GitState::default(),
            watches: HashMap::new(),
            watch_events,
            next_interest: 0,
            cancellation: SshCancellationToken::default(),
            _watch_task: watch_task,
        }
    }

    /// The Worktrees `id` presents now.
    pub(crate) fn listing(&self, id: WorktreeInterestId) -> WorktreeListing {
        self.catalog.listing(id)
    }

    /// Starts following the Worktrees of the repository containing `directory`.
    pub(crate) fn register(
        &mut self,
        directory: PathBuf,
        cx: &mut Context<Self>,
    ) -> WorktreeInterestId {
        self.next_interest += 1;
        let id = WorktreeInterestId::new(self.next_interest);
        let update = self.catalog.set_directory(id, directory);
        self.apply(update, cx);
        id
    }

    pub(crate) fn set_directory(
        &mut self,
        id: WorktreeInterestId,
        directory: PathBuf,
        cx: &mut Context<Self>,
    ) {
        let update = self.catalog.set_directory(id, directory);
        self.apply(update, cx);
    }

    pub(crate) fn unregister(&mut self, id: WorktreeInterestId, cx: &mut Context<Self>) {
        let update = self.catalog.unregister(id);
        self.apply(update, cx);
    }

    pub(crate) fn window_activated(&mut self, cx: &mut Context<Self>) {
        let update = self.catalog.refresh_all();
        self.apply(update, cx);
    }

    /// Reads again every listing of the repository sharing `common`.
    pub(crate) fn repository_changed(&mut self, common: &PathBuf, cx: &mut Context<Self>) {
        let update = self.catalog.repository_changed(common);
        self.apply(update, cx);
    }

    fn apply(&mut self, update: CatalogUpdate, cx: &mut Context<Self>) {
        for effect in update.effects {
            self.run(effect, cx);
        }
        if update.changed {
            cx.emit(WorktreesChanged);
            cx.notify();
        }
    }

    fn run(&mut self, effect: CatalogEffect, cx: &mut Context<Self>) {
        match effect {
            CatalogEffect::Read {
                directory,
                generation,
            } => {
                let Some(git) = self.git.git.clone() else {
                    if self.git.checked {
                        let update = self.catalog.read_finished(&directory, generation, Ok(None));
                        self.apply(update, cx);
                    } else {
                        self.git.waiting.push(CatalogEffect::Read {
                            directory,
                            generation,
                        });
                        self.check_git(cx);
                    }
                    return;
                };
                let cancellation = self.cancellation.clone();
                let read_directory = directory.clone();
                self.read(
                    move || git.list(&read_directory, &cancellation),
                    move |store, result, cx| {
                        let result = result.unwrap_or(Err(
                            crate::repository_status::RepositoryReadError::Unavailable,
                        ));
                        let update = store.catalog.read_finished(&directory, generation, result);
                        store.apply(update, cx);
                    },
                    cx,
                );
            }
            CatalogEffect::Watch {
                common,
                directories,
            } => {
                let events = self.watch_events.clone();
                let changed = common.clone();
                let watch = self.adapters.watcher.watch(
                    directories,
                    Box::new(move || {
                        let _ = events.try_send(changed.clone());
                    }),
                );
                // A repository that cannot be watched still refreshes on the other triggers.
                match watch {
                    Ok(watch) => {
                        self.watches.insert(common, watch);
                    }
                    Err(_) => {
                        self.watches.remove(&common);
                    }
                }
            }
            CatalogEffect::Unwatch { common } => {
                self.watches.remove(&common);
            }
        }
    }

    fn check_git(&mut self, cx: &mut Context<Self>) {
        if self.git.checking {
            return;
        }
        self.git.checking = true;
        let adapters = self.adapters.clone();
        let cancellation = self.cancellation.clone();
        self.read(
            move || {
                let inventory = ToolInventory {
                    git: adapters.discovery.discover().git,
                    github_cli: None,
                };
                let (status, _) = check_tools(
                    adapters.runner.as_ref(),
                    &inventory,
                    &adapters.physical_home,
                    &[],
                    Instant::now() + GIT_CHECK_TIMEOUT,
                    &cancellation,
                );
                (inventory.git, status)
            },
            |store, result, cx| {
                let (executable, status) = result.unwrap_or((None, GitToolStatus::NotFound));
                store.git_checked(executable, status, cx);
            },
            cx,
        );
    }

    fn git_checked(
        &mut self,
        executable: Option<PathBuf>,
        status: GitToolStatus,
        cx: &mut Context<Self>,
    ) {
        self.git.checking = false;
        self.git.checked = true;
        self.git.git = match (status, executable) {
            (GitToolStatus::Ready(version), Some(executable)) => {
                Some(Arc::new(LocalWorktreeGit::new(
                    Arc::clone(&self.adapters.runner),
                    executable,
                    self.adapters.physical_home.clone(),
                    version,
                )))
            }
            _ => None,
        };
        for effect in std::mem::take(&mut self.git.waiting) {
            self.run(effect, cx);
        }
    }

    /// Runs `work` on its own thread and hands its result to `finish` on the UI thread. `finish`
    /// receives `None` when the work could not start or did not finish.
    fn read<T: Send + 'static>(
        &self,
        work: impl FnOnce() -> T + Send + 'static,
        finish: impl FnOnce(&mut Self, Option<T>, &mut Context<Self>) + 'static,
        cx: &mut Context<Self>,
    ) {
        let (sender, receiver) = async_channel::bounded(1);
        // A thread that never starts drops the sender, which ends the wait with `None`.
        let _ = (self.spawn)(Box::new(move || {
            let _ = sender.send_blocking(work());
        }));
        cx.spawn(async move |store, cx| {
            let result = receiver.recv().await.ok();
            let _ = store.update(cx, |store, cx| finish(store, result, cx));
        })
        .detach();
    }
}

const GIT_CHECK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

#[cfg(test)]
pub(crate) mod testing {
    //! A store whose listings tests present directly. It finds no git, so nothing runs.

    use std::path::Path;

    use super::*;
    use crate::repository_status::testing::FakeRunner;
    use crate::repository_status::{
        OperationMarkers, RepositoryMarkerReader, RepositoryToolDiscovery, RepositoryWatchError,
        RepositoryWatcher, WatchDirectory,
    };

    struct NoTools;

    impl RepositoryToolDiscovery for NoTools {
        fn discover(&self) -> ToolInventory {
            ToolInventory {
                git: None,
                github_cli: None,
            }
        }
    }

    impl RepositoryMarkerReader for NoTools {
        fn read(&self, _: &Path) -> OperationMarkers {
            OperationMarkers::default()
        }
    }

    impl RepositoryWatcher for NoTools {
        fn watch(
            &self,
            _: Vec<WatchDirectory>,
            _: Box<dyn Fn() + Send + Sync>,
        ) -> Result<Box<dyn RepositoryWatch>, RepositoryWatchError> {
            Err(RepositoryWatchError)
        }
    }

    pub(crate) fn install(cx: &mut App) -> Entity<WorktreeStore> {
        let adapters = RepositoryStatusAdapters {
            runner: FakeRunner::new([]),
            discovery: Arc::new(NoTools),
            markers: Arc::new(NoTools),
            watcher: Arc::new(NoTools),
            physical_home: "/home/person".into(),
            github_cli_environment: Vec::new(),
        };
        let store = cx.new(|cx| WorktreeStore::new(adapters, Arc::new(|_| false), cx));
        cx.set_global(InstalledWorktrees(store.clone()));
        store
    }

    impl WorktreeStore {
        /// Presents `snapshot` to every interest reading `directory`.
        pub(crate) fn present(
            &mut self,
            directory: &Path,
            snapshot: Option<WorktreeSnapshot>,
            cx: &mut Context<Self>,
        ) {
            let update = self.catalog.present(directory, snapshot);
            self.apply(update, cx);
        }
    }
}
