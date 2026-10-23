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
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
    sync::{Arc, Mutex, mpsc},
    thread::JoinHandle,
    time::{Duration, Instant},
};

const DISCOVERY_WORKERS: usize = 4;
const COUNT_WORKERS: usize = 2;
const COUNTING_DELAY: Duration = Duration::from_secs(1);
const POLL: Duration = Duration::from_millis(10);
const REFRESH: Duration = Duration::from_secs(2);
const MAX_DIRECTORIES: usize = 512;
const MAX_WORKTREES: usize = 256;
const MAX_SNAPSHOT_BYTES: usize = 8 * 1024 * 1024;

mod pins;

type Sources = BTreeSet<PathBuf>;
struct Worker {
    cancellation: SshCancellationToken,
    handle: JoinHandle<()>,
    results: mpsc::Receiver<(GitToolStatus, DirectoryFacts)>,
    interests: mpsc::SyncSender<Sources>,
    sent_sources: Sources,
}
pub(super) struct ServerRepositories {
    subscribers: BTreeSet<Id>,
    sources: Sources,
    snapshot: Snapshot,
    worker: Option<Worker>,
    next_read: Instant,
    pin_membership: pins::PinMembership,
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
                pinned_worktrees: Vec::new(),
                pin_check: PinCheckState::Ready,
            },
            worker: None,
            next_read: Instant::now(),
            pin_membership: pins::PinMembership::default(),
        }
    }
}
impl Drop for ServerRepositories {
    fn drop(&mut self) {
        self.stop();
    }
}
impl ServerRepositories {
    pub(super) fn refresh(&mut self) {
        self.stop();
    }
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
                if let Some(locator) = &tab.worktree_repository {
                    sources.insert(locator.clone());
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
            self.next_read = Instant::now();
        }
        if let Some(worker) = &mut self.worker {
            // Retry the latest interests on the next update if the bounded mailbox is full.
            if worker.sent_sources != self.sources
                && worker.interests.try_send(self.sources.clone()).is_ok()
            {
                worker.sent_sources.clone_from(&self.sources);
            }
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
                if let Some(old) = old {
                    let previous = &self.snapshot.directories[old];
                    if let (Some(current), Some(previous_repository)) =
                        (&mut facts.repository, &previous.repository)
                        && current.root == previous_repository.root
                    {
                        if current.counting.is_some() && previous_repository.changes.is_ok() {
                            current.changes.clone_from(&previous_repository.changes);
                        }
                        if facts.worktrees.is_none() {
                            facts.worktrees.clone_from(&previous.worktrees);
                        }
                    }
                }
                if facts.failure.is_some()
                    && let Some(old) = old
                {
                    let previous = &self.snapshot.directories[old];
                    if facts.repository.is_none() {
                        facts.repository.clone_from(&previous.repository);
                        if let Some(repository) = &mut facts.repository {
                            repository.counting = None;
                        }
                    }
                    if facts.worktrees.is_none() {
                        facts.worktrees.clone_from(&previous.worktrees);
                    }
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
        self.pin_membership.update(
            &state.inventory,
            &mut self.snapshot,
            !self.subscribers.is_empty(),
        );
        reconcile_membership(state, &self.snapshot);
        if !self.subscribers.is_empty() && self.worker.is_none() && Instant::now() >= self.next_read
        {
            let cancellation = SshCancellationToken::default();
            let token = cancellation.clone();
            let sources = self.sources.clone();
            let sent_sources = sources.clone();
            let (interests, updates) = mpsc::sync_channel(1);
            let (sender, results) = mpsc::sync_channel(1);
            if let Ok(handle) = std::thread::Builder::new()
                .name("spaceterm-server-git".into())
                .spawn(move || read(sources, updates, token, sender))
            {
                self.worker = Some(Worker {
                    cancellation,
                    handle,
                    results,
                    interests,
                    sent_sources,
                });
            }
            self.next_read = Instant::now() + REFRESH;
        }
    }
}
pub(super) fn pane_directory(pane: &spaceterm_server::hierarchy::Pane) -> &Path {
    pane.repository_directory()
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
            if tab.worktree_membership_fixed
                || (tab.worktree.is_some() && !(follows_root && tab.id == workspace.root_tab))
            {
                // Older Development records gain a locator without changing saved membership.
                if let Some(main) = snapshot.directories.iter().find_map(|facts| {
                    let list = facts
                        .worktrees
                        .as_ref()
                        .filter(|_| facts.failure.is_none())?;
                    list.worktrees
                        .iter()
                        .any(|record| Some(&record.root) == tab.worktree.as_ref())
                        .then_some(&list.main_root)
                }) && tab.worktree_repository.as_ref() != Some(main)
                {
                    tab.worktree_repository = Some(main.clone());
                    changed = true;
                }
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
                changed |= tab.worktree_repository.take().is_some();
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
            if let Some((root, main)) = current {
                if tab.worktree.as_deref() != Some(root) {
                    tab.worktree = Some(root.to_owned());
                    changed = true;
                }
                if tab.worktree_repository.as_deref() != Some(main) {
                    tab.worktree_repository = Some(main.to_owned());
                    changed = true;
                }
            }
        }
    }
    if changed {
        state.inventory.revision = state.inventory.revision.saturating_add(1);
    }
    changed
}
fn current_worktree<'a>(snapshot: &'a Snapshot, directory: &Path) -> Option<(&'a Path, &'a Path)> {
    let facts = snapshot
        .directories
        .iter()
        .find(|facts| facts.directory == directory && facts.failure.is_none())?;
    let list = facts.worktrees.as_ref()?;
    let record = list.worktrees.get(list.current?)?;
    (!record.missing && record.head != WorktreeHead::Bare)
        .then_some((record.root.as_path(), list.main_root.as_path()))
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
    interests: mpsc::Receiver<Sources>,
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
    read_sources(sources, interests, readers, status, cancellation, sender);
}

type Readers = (
    policy::local_read::LocalRepositoryReader,
    crate::worktrees::git::LocalWorktreeGit,
);
type Changes = Result<ChangeSummary, RepositoryReadError>;

struct ActiveRead {
    // Reject late results after a source or physical root is removed and reintroduced.
    id: u64,
    cancellation: SshCancellationToken,
}
struct DiscoveryJob {
    directory: PathBuf,
    active: ActiveRead,
}
struct CountJob {
    root: PathBuf,
    fsmonitor: FsmonitorPolicy,
    active: ActiveRead,
}
enum ReadEvent {
    Facts {
        id: u64,
        facts: Box<DirectoryFacts>,
        fsmonitor: FsmonitorPolicy,
        discovered: bool,
    },
    CountFinished {
        id: u64,
        root: PathBuf,
        changes: Changes,
    },
}
struct DirectoryRead {
    facts: Option<DirectoryFacts>,
    active: Option<ActiveRead>,
    next_read: Instant,
}
struct CountRead {
    fsmonitor: FsmonitorPolicy,
    active: Option<ActiveRead>,
    next_read: Instant,
    // Queue time counts toward progress, including when both scan workers are occupied.
    requested: Option<Instant>,
    counting: Option<ChangeCounting>,
    changes: Changes,
}
impl CountRead {
    fn apply(&self, facts: &mut DirectoryFacts) {
        if let Some(repository) = &mut facts.repository {
            repository.counting = self.counting;
            repository.changes.clone_from(&self.changes);
        }
    }
}

/// One subscriber lifetime owns fixed discovery and count pools. Recurring discovery never
/// waits for a full scan; source edits update interests without restarting scans still in use.
fn read_sources(
    sources: Sources,
    interests: mpsc::Receiver<Sources>,
    readers: Option<Readers>,
    status: GitToolStatus,
    cancellation: SshCancellationToken,
    sender: mpsc::SyncSender<(GitToolStatus, DirectoryFacts)>,
) {
    let Some((repository, worktrees)) = readers else {
        for directory in sources {
            if !send(
                &sender,
                (
                    status,
                    DirectoryFacts {
                        directory,
                        repository: None,
                        worktrees: None,
                        failure: Some(RepositoryReadError::ToolMissing),
                    },
                ),
                &cancellation,
            ) {
                return;
            }
        }
        return;
    };
    // Queues hold only a bounded window of jobs. The supervisor retains at most one pending
    // discovery per interested source and one pending count per physical Worktree root.
    let (discoveries, discovery_receiver) = mpsc::sync_channel::<DiscoveryJob>(DISCOVERY_WORKERS);
    let discovery_receiver = Mutex::new(discovery_receiver);
    let (counts, count_receiver) = mpsc::sync_channel::<CountJob>(COUNT_WORKERS);
    let count_receiver = Mutex::new(count_receiver);
    let (events, receiver) = mpsc::sync_channel(8);
    std::thread::scope(|scope| {
        for _ in 0..DISCOVERY_WORKERS {
            let jobs = &discovery_receiver;
            let events = &events;
            let cancellation = &cancellation;
            let repository = &repository;
            let worktrees = &worktrees;
            scope.spawn(move || {
                while !cancellation.is_cancelled() {
                    let job = jobs.lock().unwrap().recv();
                    match job {
                        Ok(job) if !job.active.cancellation.is_cancelled() => discover(
                            job.directory,
                            job.active.id,
                            repository,
                            worktrees,
                            events,
                            &job.active.cancellation,
                        ),
                        Ok(_) => {}
                        Err(_) => return,
                    }
                }
            });
        }
        for _ in 0..COUNT_WORKERS {
            let jobs = &count_receiver;
            let events = &events;
            let repository = &repository;
            let cancellation = &cancellation;
            scope.spawn(move || {
                while !cancellation.is_cancelled() {
                    let job = match jobs.lock().unwrap().recv() {
                        Ok(job) => job,
                        Err(_) => return,
                    };
                    if job.active.cancellation.is_cancelled() {
                        continue;
                    }
                    let changes = repository
                        .count(&job.root, job.fsmonitor, &job.active.cancellation)
                        .map(|summary| summary.changes);
                    if !send(
                        events,
                        ReadEvent::CountFinished {
                            id: job.active.id,
                            root: job.root,
                            changes,
                        },
                        cancellation,
                    ) {
                        return;
                    }
                }
            });
        }
        let mut supervisor = ReadSupervisor {
            directories: BTreeMap::new(),
            counts: BTreeMap::new(),
            next_id: 0,
            discoveries,
            count_jobs: counts,
            sender,
            status,
            cancellation: cancellation.clone(),
        };
        supervisor.set_sources(sources);
        supervisor.run(interests, receiver);
        // Every queued or running job observes this token. Join the fixed pools before the
        // owning worker exits, including when publication or source delivery disconnects.
        cancellation.cancel();
    });
}

struct ReadSupervisor {
    directories: BTreeMap<PathBuf, DirectoryRead>,
    counts: BTreeMap<PathBuf, CountRead>,
    next_id: u64,
    discoveries: mpsc::SyncSender<DiscoveryJob>,
    count_jobs: mpsc::SyncSender<CountJob>,
    sender: mpsc::SyncSender<(GitToolStatus, DirectoryFacts)>,
    status: GitToolStatus,
    cancellation: SshCancellationToken,
}
impl ReadSupervisor {
    fn set_sources(&mut self, sources: Sources) {
        let sources: Sources = sources.into_iter().take(MAX_DIRECTORIES).collect();
        self.directories.retain(|directory, read| {
            if sources.contains(directory) {
                return true;
            }
            if let Some(active) = &read.active {
                active.cancellation.cancel();
            }
            false
        });
        // Also bound injected source updates at this worker boundary, rather than relying only
        // on the hierarchy's independently bounded inventory.
        for directory in sources {
            self.directories
                .entry(directory)
                .or_insert_with(|| DirectoryRead {
                    facts: None,
                    active: None,
                    next_read: Instant::now(),
                });
        }
        self.prune_counts();
    }
    fn prune_counts(&mut self) {
        let roots: Sources = self
            .directories
            .values()
            .filter_map(|read| Some(read.facts.as_ref()?.repository.as_ref()?.root.clone()))
            .collect();
        self.counts.retain(|root, read| {
            if roots.contains(root) {
                return true;
            }
            if let Some(active) = &read.active {
                active.cancellation.cancel();
            }
            false
        });
    }
    fn run(&mut self, interests: mpsc::Receiver<Sources>, events: mpsc::Receiver<ReadEvent>) {
        while !self.cancellation.is_cancelled() {
            match interests.try_recv() {
                Ok(sources) => self.set_sources(sources),
                Err(mpsc::TryRecvError::Empty) => {}
                Err(mpsc::TryRecvError::Disconnected) => return,
            }
            if !self.schedule() {
                return;
            }
            match events.recv_timeout(POLL) {
                Ok(event) => {
                    if !self.accept(event) {
                        return;
                    }
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => return,
            }
        }
    }
    fn schedule(&mut self) -> bool {
        let now = Instant::now();
        // Keep waiting work ahead of recurring refreshes, including after a bounded queue
        // fills. A completed read receives a new due time; unscheduled work keeps its place.
        let mut directories: Vec<_> = self
            .directories
            .iter()
            .filter(|(_, read)| read.active.is_none() && now >= read.next_read)
            .map(|(directory, read)| (read.next_read, directory.clone()))
            .collect();
        directories.sort_unstable();
        for (_, directory) in directories {
            let read = self.directories.get_mut(&directory).unwrap();
            self.next_id = self.next_id.saturating_add(1);
            let cancellation =
                SshCancellationToken::linked(&self.cancellation, &SshCancellationToken::default());
            let job = DiscoveryJob {
                directory: directory.clone(),
                active: ActiveRead {
                    id: self.next_id,
                    cancellation: cancellation.clone(),
                },
            };
            match self.discoveries.try_send(job) {
                Ok(()) => {
                    read.active = Some(ActiveRead {
                        id: self.next_id,
                        cancellation,
                    })
                }
                Err(mpsc::TrySendError::Full(_)) => break,
                Err(mpsc::TrySendError::Disconnected(_)) => return false,
            }
        }
        let mut progress = Vec::new();
        let mut count_capacity = true;
        let mut roots: Vec<_> = self
            .counts
            .iter()
            .map(|(root, read)| (read.next_read, root.clone()))
            .collect();
        roots.sort_unstable();
        for (_, root) in roots {
            let read = self.counts.get_mut(&root).unwrap();
            if read.active.is_none() && now >= read.next_read {
                if read.counting.is_none() {
                    read.counting = Some(ChangeCounting::Pending);
                    read.requested = Some(now);
                    progress.push(root.clone());
                }
                if count_capacity {
                    self.next_id = self.next_id.saturating_add(1);
                    let cancellation = SshCancellationToken::linked(
                        &self.cancellation,
                        &SshCancellationToken::default(),
                    );
                    let job = CountJob {
                        root: root.clone(),
                        fsmonitor: read.fsmonitor,
                        active: ActiveRead {
                            id: self.next_id,
                            cancellation: cancellation.clone(),
                        },
                    };
                    match self.count_jobs.try_send(job) {
                        Ok(()) => {
                            read.active = Some(ActiveRead {
                                id: self.next_id,
                                cancellation,
                            })
                        }
                        Err(mpsc::TrySendError::Full(_)) => count_capacity = false,
                        Err(mpsc::TrySendError::Disconnected(_)) => return false,
                    }
                }
            }
            if read.counting == Some(ChangeCounting::Pending)
                && read
                    .requested
                    .is_some_and(|requested| now.duration_since(requested) >= COUNTING_DELAY)
            {
                read.counting = Some(ChangeCounting::Counting);
                progress.push(root.clone());
            }
        }
        for root in progress {
            if !self.publish_root(&root) {
                return false;
            }
        }
        true
    }
    fn accept(&mut self, event: ReadEvent) -> bool {
        match event {
            ReadEvent::Facts {
                id,
                facts,
                fsmonitor,
                discovered,
            } => {
                let Some(read) = self.directories.get_mut(&facts.directory) else {
                    return true;
                };
                if read.active.as_ref().map(|active| active.id) != Some(id) {
                    return true;
                }
                let directory = facts.directory.clone();
                let missing_tool = facts.failure == Some(RepositoryReadError::ToolMissing);
                read.facts = Some(*facts);
                if discovered {
                    read.active = None;
                    read.next_read = Instant::now() + REFRESH;
                }
                self.prune_counts();
                if let Some(root) = self.directories[&directory]
                    .facts
                    .as_ref()
                    .and_then(|facts| facts.repository.as_ref())
                    .map(|repository| repository.root.clone())
                {
                    self.counts
                        .entry(root)
                        .or_insert_with(|| CountRead {
                            fsmonitor,
                            active: None,
                            next_read: Instant::now(),
                            requested: Some(Instant::now()),
                            counting: Some(ChangeCounting::Pending),
                            changes: Err(RepositoryReadError::Unavailable),
                        })
                        .fsmonitor = fsmonitor;
                }
                self.publish_directory(&directory) && !missing_tool
            }
            ReadEvent::CountFinished { id, root, changes } => {
                let Some(read) = self.counts.get_mut(&root) else {
                    return true;
                };
                if read.active.as_ref().map(|active| active.id) != Some(id) {
                    return true;
                }
                read.active = None;
                read.requested = None;
                read.counting = None;
                read.changes = changes;
                read.next_read = Instant::now() + REFRESH;
                self.publish_root(&root)
            }
        }
    }
    fn publish_root(&mut self, root: &Path) -> bool {
        let directories: Vec<_> = self
            .directories
            .iter()
            .filter_map(|(directory, read)| {
                if read.facts.as_ref()?.repository.as_ref()?.root == root {
                    Some(directory.clone())
                } else {
                    None
                }
            })
            .collect();
        for directory in directories {
            if !self.publish_directory(&directory) {
                return false;
            }
        }
        true
    }
    fn publish_directory(&mut self, directory: &Path) -> bool {
        let facts = self
            .directories
            .get_mut(directory)
            .unwrap()
            .facts
            .as_mut()
            .unwrap();
        if let Some(count) = facts
            .repository
            .as_ref()
            .and_then(|repository| self.counts.get(&repository.root))
        {
            count.apply(facts);
        }
        send(
            &self.sender,
            (self.status, facts.clone()),
            &self.cancellation,
        )
    }
}

fn discover(
    directory: PathBuf,
    id: u64,
    repository: &policy::local_read::LocalRepositoryReader,
    worktrees: &crate::worktrees::git::LocalWorktreeGit,
    events: &mpsc::SyncSender<ReadEvent>,
    cancellation: &SshCancellationToken,
) {
    let mut facts = DirectoryFacts {
        directory,
        repository: None,
        worktrees: None,
        failure: None,
    };
    let mut fsmonitor = FsmonitorPolicy::Disabled;
    let result = (|| {
        let validated = super::local_filesystem()
            .validate_directory(&facts.directory)
            .map_err(|_| RepositoryReadError::Unavailable)?;
        let directory = validated.path();
        if let ProbeOutcome::Repository(probe) = repository.probe(directory, cancellation)? {
            let RepositoryRoot::Local(root) = &probe.root else {
                return Err(RepositoryReadError::InvalidResponse);
            };
            // Git can preserve an alias spelling. Full scans share the physical Worktree root.
            let root = std::fs::canonicalize(root).map_err(|_| RepositoryReadError::Unavailable)?;
            fsmonitor = probe.config.fsmonitor;
            facts.repository = Some(Repository {
                root,
                pull_request: policy::pull_request::pull_request_query(&probe.config, &probe.head),
                head: probe.head,
                commit: probe.commit,
                upstream: probe.upstream,
                operation: probe.operation,
                changes: Err(RepositoryReadError::Unavailable),
                counting: Some(ChangeCounting::Pending),
            });
            if !send(
                events,
                ReadEvent::Facts {
                    id,
                    facts: Box::new(facts.clone()),
                    fsmonitor,
                    discovered: false,
                },
                cancellation,
            ) {
                return Err(RepositoryReadError::Cancelled);
            }
            // Every Pane needs its actual membership from Git, including inactive Tabs.
            if let Some(mut list) = worktrees.list(directory, cancellation)? {
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
    send(
        events,
        ReadEvent::Facts {
            id,
            facts: Box::new(facts),
            fsmonitor,
            discovered: true,
        },
        cancellation,
    );
}

/// Bounded backpressure must remain cancellable when a subscriber stops draining publications.
fn send<T>(
    sender: &mpsc::SyncSender<T>,
    mut value: T,
    cancellation: &SshCancellationToken,
) -> bool {
    loop {
        if cancellation.is_cancelled() {
            return false;
        }
        match sender.try_send(value) {
            Ok(()) => return true,
            Err(mpsc::TrySendError::Disconnected(_)) => return false,
            Err(mpsc::TrySendError::Full(pending)) => {
                value = pending;
                std::thread::sleep(POLL);
            }
        }
    }
}

#[cfg(test)]
mod tests;
