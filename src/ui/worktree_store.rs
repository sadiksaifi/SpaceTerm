//! App-scoped Worktree listings shared by every window.
//!
//! The store owns the one [`WorktreeCatalog`], runs its reads on dedicated threads, and tells
//! sidebar rows when their Worktrees changed. Programs run only through the Repository Status
//! adapters host composition injected.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use gpui::{App, AppContext as _, Context, Entity, EventEmitter, Global, Task};

use super::repository_status_store::{ReadSpawner, RepositoryStatusAdapters, thread_spawner};
use crate::platform::local_filesystem::LocalFilesystemAuthority;
use crate::repository_status::tool_check::check_tools;
use crate::repository_status::{GitToolStatus, RepositoryWatch, ToolInventory};
use crate::settings::Settings;
use crate::ssh::cancellation::SshCancellationToken;
#[cfg(test)]
use crate::worktrees::WorktreeSnapshot;
use crate::worktrees::catalog::{
    CatalogEffect, CatalogUpdate, WorktreeCatalog, WorktreeInterestId, WorktreeListing,
};
use crate::worktrees::git::{
    BranchList, LocalWorktreeGit, RemovalCheck, RemovalExpectation, WorktreeBranch,
    WorktreeCreateError, WorktreeRemoveError,
};

/// Some interest's Worktrees changed.
pub(crate) struct WorktreesChanged;

pub(crate) struct InstalledWorktrees(pub(crate) Entity<WorktreeStore>);

impl Global for InstalledWorktrees {}

impl InstalledWorktrees {
    pub(crate) fn store(cx: &App) -> Option<Entity<WorktreeStore>> {
        cx.try_global::<Self>().map(|installed| installed.0.clone())
    }
}

pub(crate) fn install(settings: &Settings, adapters: RepositoryStatusAdapters, cx: &mut App) {
    let settings = settings.clone();
    let store = cx.new(|cx| {
        let mut store = WorktreeStore::new(adapters, thread_spawner(), cx);
        store.settings = Some(settings);
        store
    });
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
    /// Supplies the Worktree Path Template.
    settings: Option<Settings>,
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
            settings: None,
            _watch_task: watch_task,
        }
    }

    /// The Worktrees `id` presents now.
    pub(crate) fn listing(&self, id: WorktreeInterestId) -> WorktreeListing {
        self.catalog.listing(id)
    }

    /// What `id`'s row shows, which keeps its last listing while a moved interest is read.
    pub(crate) fn presented(&self, id: WorktreeInterestId) -> WorktreeListing {
        self.catalog.presented(id)
    }

    /// Where the person wants new Worktrees.
    pub(crate) fn path_template(&self) -> String {
        self.settings.as_ref().map_or_else(
            || crate::worktrees::path_template::DEFAULT_WORKTREE_PATH_TEMPLATE.to_owned(),
            |settings| {
                settings
                    .snapshot()
                    .candidate
                    .git
                    .worktree_path_template
                    .clone()
            },
        )
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

    /// Lists the branches a new Worktree of the repository at `root` can use, or `None` when git
    /// cannot read them.
    pub(crate) fn branches(
        &self,
        root: PathBuf,
        cx: &mut Context<Self>,
    ) -> Task<Option<BranchList>> {
        let cancellation = self.cancellation.clone();
        let task = self.run_git(move |git| git.branches(&root, &cancellation).ok(), cx);
        cx.spawn(async move |_, _| task.await.flatten())
    }

    /// Creates a Worktree at `path`, then reads again every listing of the repository sharing
    /// `common`. A started write runs to its end.
    pub(crate) fn create(
        &self,
        root: PathBuf,
        common: PathBuf,
        path: PathBuf,
        branch: WorktreeBranch,
        cx: &mut Context<Self>,
    ) -> Task<Result<PathBuf, WorktreeCreateError>> {
        let task = self.run_git(
            move |git| {
                git.create(&root, &path, &branch, &SshCancellationToken::default())?;
                path.canonicalize().map_err(|_| WorktreeCreateError::Failed)
            },
            cx,
        );
        cx.spawn(async move |store, cx| {
            let result = task.await.unwrap_or(Err(WorktreeCreateError::Failed));
            let _ = store.update(cx, |store, cx| store.repository_changed(&common, cx));
            result
        })
    }

    /// Which Worktree is at `path` and whether removing it discards changes, or an error when git
    /// cannot tell. `filesystem` retains the Worktree's directory for the later checks.
    pub(crate) fn check_removal(
        &self,
        path: PathBuf,
        filesystem: LocalFilesystemAuthority,
        pinned_directories: Vec<PathBuf>,
        cx: &mut Context<Self>,
    ) -> Task<Result<RemovalCheck, WorktreeRemoveError>> {
        let cancellation = self.cancellation.clone();
        let task = self.run_git(
            move |git| {
                protect_pinned_directories(&path, &pinned_directories, &filesystem)?;
                git.check_removal(&path, &filesystem, &cancellation)
                    .map_err(|_| WorktreeRemoveError::Unchecked)
            },
            cx,
        );
        cx.spawn(async move |_, _| task.await.unwrap_or(Err(WorktreeRemoveError::Unchecked)))
    }

    /// Whether `path` still shows what the person confirmed removing.
    pub(crate) fn confirm_location(
        &self,
        path: PathBuf,
        expected: RemovalExpectation,
        filesystem: LocalFilesystemAuthority,
        pinned_directories: Vec<PathBuf>,
        cx: &mut Context<Self>,
    ) -> Task<Result<(), WorktreeRemoveError>> {
        let cancellation = self.cancellation.clone();
        let task = self.run_git(
            move |git| {
                protect_pinned_directories(&path, &pinned_directories, &filesystem)?;
                git.confirm_location(&path, &expected, &cancellation)
            },
            cx,
        );
        cx.spawn(async move |_, _| task.await.unwrap_or(Err(WorktreeRemoveError::Failed)))
    }

    /// Removes the Worktree at `path`, then reads the repository's listings again. Git removes
    /// nothing unless `path` still shows what the person confirmed.
    pub(crate) fn remove(
        &self,
        root: PathBuf,
        common: PathBuf,
        path: PathBuf,
        force: bool,
        expected: RemovalExpectation,
        cx: &mut Context<Self>,
    ) -> Task<Result<(), WorktreeRemoveError>> {
        let task = self.run_git(
            move |git| {
                git.remove(
                    &root,
                    &path,
                    force,
                    &expected,
                    &SshCancellationToken::default(),
                )
            },
            cx,
        );
        cx.spawn(async move |store, cx| {
            let result = task.await.unwrap_or(Err(WorktreeRemoveError::Failed));
            let _ = store.update(cx, |store, cx| store.repository_changed(&common, cx));
            result
        })
    }

    /// Runs `work` with the checked git on its own thread, or finishes with `None` when no git
    /// is ready or the thread did not finish.
    fn run_git<T: Send + 'static>(
        &self,
        work: impl FnOnce(&LocalWorktreeGit) -> T + Send + 'static,
        cx: &mut Context<Self>,
    ) -> Task<Option<T>> {
        let Some(git) = self.git.git.clone() else {
            return Task::ready(None);
        };
        let (sender, receiver) = async_channel::bounded(1);
        let _ = (self.spawn)(Box::new(move || {
            let _ = sender.send_blocking(work(&git));
        }));
        cx.spawn(async move |_, _| receiver.recv().await.ok())
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

fn protect_pinned_directories(
    root: &Path,
    pinned_directories: &[PathBuf],
    filesystem: &LocalFilesystemAuthority,
) -> Result<(), WorktreeRemoveError> {
    if pinned_directories
        .iter()
        .any(|pinned| filesystem.physically_contains(root, pinned))
    {
        Err(WorktreeRemoveError::HoldsPinnedDirectory)
    } else {
        Ok(())
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

    /// A git that answers only removal's commands: every Worktree is administered from
    /// `/repository/.git/worktrees/x` and clean, and `worktree remove` fails. Everything else
    /// fails, so listings come only from `present`.
    struct RemovalGit;

    impl crate::repository_status::RepositoryProgramRunner for RemovalGit {
        fn run(
            &self,
            request: &crate::repository_status::ProgramRequest,
            stdout: &mut dyn FnMut(&[u8]),
            _: &SshCancellationToken,
        ) -> Result<crate::repository_status::ProgramExit, crate::repository_status::ProgramError>
        {
            let arguments: Vec<&str> = request
                .arguments
                .iter()
                .filter_map(|argument| argument.to_str())
                .collect();
            let code = match arguments.as_slice() {
                [.., "rev-parse", "--absolute-git-dir"] => {
                    stdout(b"/repository/.git/worktrees/x\n");
                    0
                }
                [
                    ..,
                    "status",
                    "--porcelain",
                    "-z",
                    "--untracked-files=normal",
                    "--ignore-submodules=none",
                ] => 0,
                _ => 128,
            };
            Ok(crate::repository_status::ProgramExit { code: Some(code) })
        }
    }

    /// Like `install`, with a git that can check a Worktree for removal and runs at once.
    pub(crate) fn install_with_removal_git(cx: &mut App) -> Entity<WorktreeStore> {
        let store = install(cx);
        store.update(cx, |store, _| {
            store.spawn = Arc::new(|work| {
                work();
                true
            });
            store.git.checked = true;
            store.git.git = Some(Arc::new(LocalWorktreeGit::new(
                Arc::new(RemovalGit),
                "/usr/bin/git".into(),
                "/home/person".into(),
                crate::repository_status::ToolVersion {
                    major: 2,
                    minor: 47,
                    patch: 0,
                },
            )));
        });
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
