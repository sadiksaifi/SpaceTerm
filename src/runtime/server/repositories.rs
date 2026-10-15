//! Cancellable, bounded reads of validated host directories, shared by every inventory subscriber.
use crate::{
    repository_status::{
        self as policy, ProbeOutcome, ProgramError, ProgramExit, ProgramRequest,
        RepositoryProgramRunner, RepositoryRoot, RepositoryToolDiscovery,
    },
    ssh::cancellation::SshCancellationToken,
};
use spaceterm_server::{
    hierarchy::{Id, State},
    repository::*,
};
use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
    sync::{Arc, mpsc},
    thread::JoinHandle,
    time::{Duration, Instant},
};

const REFRESH: Duration = Duration::from_secs(2);
const MAX_DIRECTORIES: usize = 512;
const MAX_WORKTREES: usize = 256;
const MAX_SNAPSHOT_BYTES: usize = 8 * 1024 * 1024;

type Sources = BTreeSet<PathBuf>;
struct Worker {
    cancellation: SshCancellationToken,
    handle: JoinHandle<()>,
    results: mpsc::Receiver<(GitToolStatus, DirectoryFacts)>,
}
pub(super) struct ServerRepositories {
    subscribers: BTreeSet<Id>,
    sources: Sources,
    snapshot: Snapshot,
    worker: Option<Worker>,
    next_read: Instant,
}
impl ServerRepositories {
    pub(super) fn new(boot: Id) -> Self {
        Self {
            subscribers: BTreeSet::new(),
            sources: BTreeSet::new(),
            snapshot: Snapshot {
                boot,
                revision: 0,
                git: GitToolStatus::Unknown,
                directories: Vec::new(),
            },
            worker: None,
            next_read: Instant::now(),
        }
    }
}
impl Drop for ServerRepositories {
    fn drop(&mut self) {
        self.stop();
    }
}
impl ServerRepositories {
    pub(super) fn subscribe(&mut self, connection: Id) {
        self.subscribers.insert(connection);
    }
    pub(super) fn disconnected(&mut self, connection: Id) {
        self.subscribers.remove(&connection);
        if self.subscribers.is_empty() {
            self.stop();
        }
    }
    pub(super) fn shutdown(&mut self) {
        self.subscribers.clear();
        self.stop();
    }
    fn stop(&mut self) {
        if let Some(worker) = self.worker.take() {
            worker.cancellation.cancel();
            let _ = worker.handle.join();
        }
        self.next_read = Instant::now();
    }
    pub(super) fn snapshot(&self) -> Snapshot {
        self.snapshot.clone()
    }
    pub(super) fn update(&mut self, state: &mut State) {
        let mut sources = Sources::new();
        for workspace in &state.inventory.workspaces {
            for tab in &workspace.tabs {
                for pane in &tab.panes {
                    sources.insert(pane_directory(pane).to_owned());
                }
                // A missing Root Pane directory must not prevent listing from a surviving sibling Worktree.
                if let Some(root) = &tab.worktree {
                    sources.insert(root.clone());
                }
            }
            if let Some(directory) = workspace_directory(workspace) {
                sources.insert(directory.to_owned());
            }
        }
        // Git supplies additional row sources and a surviving discovery root after external removal.
        for facts in &self.snapshot.directories {
            if sources.contains(&facts.directory)
                && let Some(list) = &facts.worktrees
            {
                for record in &list.worktrees {
                    if sources.len() >= MAX_DIRECTORIES {
                        break;
                    }
                    if !record.missing && record.head != WorktreeHead::Bare {
                        sources.insert(record.root.clone());
                    }
                }
            }
        }
        // The retained hierarchy is bounded independently, and Worktree-derived interests share this cap.
        while sources.len() > MAX_DIRECTORIES {
            sources.pop_last();
        }
        if sources != self.sources {
            self.sources = sources;
            self.snapshot
                .directories
                .retain(|facts| self.sources.contains(&facts.directory));
            self.snapshot.revision = self.snapshot.revision.saturating_add(1);
            if let Some(worker) = &self.worker {
                worker.cancellation.cancel();
            }
            self.next_read = Instant::now();
        }
        if let Some(worker) = &self.worker {
            let finished = worker.handle.is_finished();
            for (git, mut facts) in worker.results.try_iter() {
                if worker.cancellation.is_cancelled() || !self.sources.contains(&facts.directory) {
                    continue;
                }
                let old = self
                    .snapshot
                    .directories
                    .iter()
                    .position(|old| old.directory == facts.directory);
                if facts.failure.is_some()
                    && let Some(old) = old
                {
                    facts
                        .repository
                        .clone_from(&self.snapshot.directories[old].repository);
                    facts
                        .worktrees
                        .clone_from(&self.snapshot.directories[old].worktrees);
                }
                if old.is_some_and(|old| self.snapshot.directories[old] == facts)
                    && self.snapshot.git == git
                {
                    continue;
                }
                // Reject oversized publications before the authenticated transport allocates a frame.
                let mut next = self.snapshot.clone();
                if let Some(old) = old {
                    next.directories[old] = facts;
                } else {
                    next.directories.push(facts);
                }
                next.git = git;
                if serde_json::to_vec(&next).is_ok_and(|bytes| bytes.len() <= MAX_SNAPSHOT_BYTES) {
                    next.revision = next.revision.saturating_add(1);
                    self.snapshot = next;
                }
            }
            if finished {
                let worker = self.worker.take();
                if let Some(worker) = worker {
                    let _ = worker.handle.join();
                }
            }
        }
        reconcile_membership(state, &self.snapshot);
        if !self.subscribers.is_empty() && self.worker.is_none() && Instant::now() >= self.next_read
        {
            let cancellation = SshCancellationToken::default();
            let token = cancellation.clone();
            let sources = self.sources.clone();
            let (sender, results) = mpsc::sync_channel(1);
            if let Ok(handle) = std::thread::Builder::new()
                .name("spaceterm-server-git".into())
                .spawn(move || read(sources, token, sender))
            {
                self.worker = Some(Worker {
                    cancellation,
                    handle,
                    results,
                });
            }
            self.next_read = Instant::now() + REFRESH;
        }
    }
}
pub(super) fn pane_directory(pane: &spaceterm_server::hierarchy::Pane) -> &Path {
    pane.confirmed_directory
        .as_deref()
        .unwrap_or(&pane.starting_directory)
}
pub(super) fn workspace_directory(
    workspace: &spaceterm_server::hierarchy::Workspace,
) -> Option<&Path> {
    workspace.pinned_directory.as_deref().or_else(|| {
        let tab = workspace
            .tabs
            .iter()
            .find(|tab| tab.id == workspace.root_tab)?;
        tab.panes
            .iter()
            .find(|pane| pane.id == tab.root_pane)
            .map(pane_directory)
    })
}
fn reconcile_membership(state: &mut State, snapshot: &Snapshot) -> bool {
    let mut changed = false;
    for workspace in &mut state.inventory.workspaces {
        let workspace_current = workspace_directory(workspace)
            .and_then(|directory| current_worktree(snapshot, directory));
        let follows_root = workspace.pinned_directory.is_none();
        for tab in &mut workspace.tabs {
            // Membership belongs to the saved Tab, not its focused Pane or a later cd in any
            // non-Root Tab. Only the unpinned Root Tab follows the Workspace's directory.
            if tab.worktree.is_some() && !(follows_root && tab.id == workspace.root_tab) {
                continue;
            }
            // Outside is authoritative only after a successful read. Pending reads and
            // unavailable directories preserve the saved membership for recovery.
            if follows_root
                && tab.id == workspace.root_tab
                && tab
                    .panes
                    .iter()
                    .find(|pane| pane.id == tab.root_pane)
                    .is_some_and(|pane| {
                        snapshot.directories.iter().any(|facts| {
                            facts.directory == pane_directory(pane)
                                && facts.failure.is_none()
                                && facts.repository.is_none()
                        })
                    })
            {
                changed |= tab.worktree.take().is_some();
                continue;
            }
            let current = tab
                .panes
                .iter()
                .find(|pane| pane.id == tab.root_pane)
                .and_then(|pane| current_worktree(snapshot, pane_directory(pane)))
                .or_else(|| {
                    tab.worktree
                        .is_none()
                        .then_some(workspace_current)
                        .flatten()
                });
            if let Some(current) = current
                && tab.worktree.as_deref() != Some(current)
            {
                tab.worktree = Some(current.to_owned());
                changed = true;
            }
        }
    }
    if changed {
        state.inventory.revision = state.inventory.revision.saturating_add(1);
    }
    changed
}
fn current_worktree<'a>(snapshot: &'a Snapshot, directory: &Path) -> Option<&'a Path> {
    let facts = snapshot
        .directories
        .iter()
        .find(|facts| facts.directory == directory && facts.failure.is_none())?;
    let list = facts.worktrees.as_ref()?;
    let record = list.worktrees.get(list.current?)?;
    (!record.missing && record.head != WorktreeHead::Bare).then_some(record.root.as_path())
}
/// Existing readers already bound their parsers and honor cancellation. This adapter also gives
/// every server read a deadline, including filters and fsmonitor helpers under account Git config.
struct DeadlineRunner;
impl RepositoryProgramRunner for DeadlineRunner {
    fn run(
        &self,
        request: &ProgramRequest,
        stdout: &mut dyn FnMut(&[u8]),
        token: &SshCancellationToken,
    ) -> Result<ProgramExit, ProgramError> {
        let mut request = request.clone();
        request.deadline = Some(Instant::now() + Duration::from_secs(5));
        crate::platform::unix_repository_program::UnixRepositoryProgramRunner::new()
            .run(&request, stdout, token)
    }
}
fn read(
    sources: Sources,
    cancellation: SshCancellationToken,
    sender: mpsc::SyncSender<(GitToolStatus, DirectoryFacts)>,
) {
    #[cfg(target_os = "macos")]
    let discovery = crate::platform::macos_repository_tools::MacosRepositoryToolDiscovery::new();
    #[cfg(target_os = "linux")]
    let discovery = crate::platform::linux_repository_tools::LinuxRepositoryToolDiscovery::new(
        std::env::var_os("PATH").as_deref(),
    );
    let tools = discovery.discover();
    let home = crate::platform::app_directories::AppDirectoryEnvironment::capture()
        .home
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/"));
    let home = std::fs::canonicalize(&home).unwrap_or(home);
    let runner: Arc<dyn RepositoryProgramRunner> = Arc::new(DeadlineRunner);
    // Deliberately exclude the GitHub CLI: its authentication and lookup belong to each client.
    let inventory = policy::ToolInventory {
        git: tools.git.clone(),
        github_cli: None,
    };
    let (status, _) = policy::tool_check::check_tools(
        runner.as_ref(),
        &inventory,
        &home,
        &[],
        Instant::now() + Duration::from_secs(5),
        &cancellation,
    );
    let readers = match (tools.git, status) {
        (Some(git), GitToolStatus::Ready(version)) => Some((
            policy::local_read::LocalRepositoryReader::new(
                Arc::clone(&runner),
                Arc::new(
                    crate::platform::repository_marker_reader::UnixRepositoryMarkerReader::new(),
                ),
                git.clone(),
                home.clone(),
                version,
            ),
            crate::worktrees::git::LocalWorktreeGit::new(runner, git, home, version),
        )),
        _ => None,
    };
    for directory in sources {
        if cancellation.is_cancelled() {
            return;
        }
        let mut facts = DirectoryFacts {
            directory,
            repository: None,
            worktrees: None,
            failure: None,
        };
        let result = (|| {
            let (repository, worktrees) =
                readers.as_ref().ok_or(RepositoryReadError::ToolMissing)?;
            let validated = super::local_filesystem()
                .validate_directory(&facts.directory)
                .map_err(|_| RepositoryReadError::Unavailable)?;
            let directory = validated.path();
            if let ProbeOutcome::Repository(probe) = repository.probe(directory, &cancellation)? {
                let RepositoryRoot::Local(root) = &probe.root else {
                    return Err(RepositoryReadError::InvalidResponse);
                };
                let changes = repository
                    .count(root, probe.config.fsmonitor, &cancellation)?
                    .changes;
                facts.repository = Some(Repository {
                    root: root.clone(),
                    pull_request: policy::pull_request::pull_request_query(
                        &probe.config,
                        &probe.head,
                    ),
                    head: probe.head,
                    commit: probe.commit,
                    upstream: probe.upstream,
                    operation: probe.operation,
                    changes,
                });
                // Every Pane needs its actual membership from Git, including inactive Tabs.
                if let Some(mut list) = worktrees.list(directory, &cancellation)? {
                    if list.worktrees.len() > MAX_WORKTREES {
                        return Err(RepositoryReadError::OutputTooLarge);
                    }
                    for record in &mut list.worktrees {
                        record.missing |= matches!(
                            super::local_filesystem().validate_directory(&record.root),
                            Err(super::LocalFilesystemError::Missing
                                | super::LocalFilesystemError::NotDirectory)
                        );
                    }
                    facts.worktrees = Some(WorktreeInventory {
                        main_root: list.repository.main_root().to_owned(),
                        current: list.current,
                        worktrees: list.worktrees,
                    });
                }
            }
            Ok(())
        })();
        facts.failure = result.err();
        let mut publication = (status, facts);
        loop {
            if cancellation.is_cancelled() {
                return;
            }
            match sender.try_send(publication) {
                Ok(()) => break,
                Err(mpsc::TrySendError::Disconnected(_)) => return,
                Err(mpsc::TrySendError::Full(pending)) => {
                    publication = pending;
                    std::thread::sleep(Duration::from_millis(10));
                }
            }
        }
    }
}
