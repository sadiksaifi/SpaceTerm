//! Cancellable, bounded reads of validated host directories, shared by every inventory subscriber.
//!
//! A subscriber's first read covers every source. After that a directory is read again only when
//! something can have changed its facts: a change below its repository's git directories, a
//! command finishing in a Pane that shows it, or that Pane gaining focus. Nothing reads on a
//! timer, so an idle server runs no git. The reader rings the host when it has results or ends,
//! so the host never ticks to look for them.
use crate::{
    cancellation::CancellationToken,
    repository_status::{
        self as policy, ProbeOutcome, ProgramError, ProgramExit, ProgramRequest,
        RepositoryProgramRunner, RepositoryRoot, RepositoryToolDiscovery, WatchDirectory, WatchSet,
        WatchSets,
    },
};
use spaceterm_server::{
    connection::Wake,
    hierarchy::{Id, State, WorktreeMembership},
    repository::*,
};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
    sync::{Arc, Mutex, OnceLock, mpsc},
    thread::JoinHandle,
    time::{Duration, Instant},
};

const DISCOVERY_WORKERS: usize = 4;
const COUNT_WORKERS: usize = 2;
const COUNTING_DELAY: Duration = Duration::from_secs(1);
/// How long a reader waits before offering a result again while the host has not drained the
/// last ones. It waits only under that backpressure, never while idle.
const BACKPRESSURE: Duration = Duration::from_millis(10);
/// Results waiting for the host. The host drains them each time the reader rings it.
const RESULTS: usize = 64;
/// How long after its last read a watched repository is read again. A checkout or rebase writes
/// many git files, and each write fires the watch. Reading at most this often bounds the git a
/// busy repository costs, and nothing reads while it stays quiet.
const SETTLE: Duration = Duration::from_secs(2);
/// Directories of a repository's `refs/` tree that are watched. A ref in a directory past it,
/// such as an unusually deep branch name, is read on the other triggers.
const MAX_REF_DIRECTORIES: usize = 64;
/// Changed paths kept between two looks of the supervisor. More means every repository changed.
const MAX_CHANGED_PATHS: usize = 256;
const MAX_DIRECTORIES: usize = 512;
const MAX_WORKTREES: usize = 256;
const MAX_SNAPSHOT_BYTES: usize = 8 * 1024 * 1024;

mod pins;

type Sources = BTreeSet<PathBuf>;

/// Rings the host's wake once the host has set it. Reader threads hold a clone.
#[derive(Clone, Default)]
pub(super) struct Ring(Arc<OnceLock<Arc<dyn Wake>>>);
impl Ring {
    fn ring(&self) {
        if let Some(wake) = self.0.get() {
            wake.wake();
        }
    }
}

/// What the reader's supervisor waits for. It waits for nothing else.
enum Input {
    /// The directories subscribers see now.
    Sources(Sources),
    /// Read these directories and their repositories' changes again.
    Reread(Sources),
    /// The watch set saw paths change. They wait in the supervisor's `Changed`.
    Changed,
    /// A discovery or count job finished a step.
    Read(ReadEvent),
    /// The reader's cancellation token was cancelled.
    Stop,
}

struct Worker {
    cancellation: CancellationToken,
    handle: JoinHandle<()>,
    results: mpsc::Receiver<(GitToolStatus, DirectoryFacts)>,
    inputs: mpsc::Sender<Input>,
    sent_sources: Sources,
}
pub(super) struct ServerRepositories {
    subscribers: BTreeSet<Id>,
    sources: Sources,
    snapshot: Snapshot,
    worker: Option<Worker>,
    /// When a subscriber's read starts while none runs. `None` waits for a new subscriber, a
    /// source change or a refresh, so a reader that ended because git is missing stays ended.
    start: Option<Instant>,
    pin_membership: pins::PinMembership,
    /// Each live Pane's finished command count, as last observed.
    commands: BTreeMap<Id, u64>,
    ring: Ring,
    watcher: Arc<dyn WatchSets>,
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
            start: Some(Instant::now()),
            pin_membership: pins::PinMembership::default(),
            commands: BTreeMap::new(),
            ring: Ring::default(),
            watcher: crate::platform::server_watch_sets(),
        }
    }
    /// The host's wake. Readers ring it when they have results or end.
    pub(super) fn set_wake(&mut self, wake: Arc<dyn Wake>) {
        let _ = self.ring.0.set(wake);
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
        if self.worker.is_none() {
            self.start = Some(Instant::now());
        }
    }
    /// Reads `directories` and their repositories' changes again. Without a running read, it
    /// starts one for the subscribers, which reads every source; a read that ended, for
    /// example because git was missing, starts again this way.
    pub(super) fn reread(&mut self, directories: Sources) {
        let directories: Sources = directories
            .into_iter()
            .filter(|directory| self.sources.contains(directory))
            .collect();
        if directories.is_empty() {
            return;
        }
        match &self.worker {
            Some(worker) => {
                let _ = worker.inputs.send(Input::Reread(directories));
            }
            None if !self.subscribers.is_empty() => self.start = Some(Instant::now()),
            None => {}
        }
    }
    /// Notes each live Pane's finished command count. A command that finished since the last
    /// observation can have changed its directory's files, which no git directory watch sees, so
    /// that directory and the rest of its repository are read again.
    pub(super) fn commands(&mut self, counts: BTreeMap<Id, u64>, state: &State) {
        let mut directories = Sources::new();
        for (pane, count) in &counts {
            if self.commands.get(pane).is_some_and(|seen| count > seen)
                && let Some((_, _, pane)) = state.pane(*pane)
            {
                directories.insert(pane_directory(pane).to_owned());
            }
        }
        self.commands = counts;
        self.reread(directories);
    }
    /// A Pane gained focus, perhaps after its files changed in another application. Its
    /// directory and the rest of its repository are read again, as a desktop reads local
    /// Repository Status on activation.
    pub(super) fn focused(&mut self, pane: Id, state: &State) {
        if let Some((_, _, pane)) = state.pane(pane) {
            self.reread([pane_directory(pane).to_owned()].into());
        }
        self.pin_membership.retry_now();
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
            let _ = worker.inputs.send(Input::Stop);
            let _ = worker.handle.join();
        }
        self.start = Some(Instant::now());
    }
    /// When the next update has work: a subscriber's read that has not started, or a pin
    /// check that could not finish. A running read rings the host instead. `None` without
    /// subscribers.
    pub(super) fn due(&self, now: Instant) -> Option<Instant> {
        if self.subscribers.is_empty() {
            return None;
        }
        let start = self
            .start
            .filter(|_| self.worker.is_none())
            .map(|start| start.max(now));
        let pins = self.pin_membership.due().map(|due| due.max(now));
        start.into_iter().chain(pins).min()
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
            if self.worker.is_none() {
                self.start = Some(Instant::now());
            }
        }
        if let Some(worker) = &mut self.worker {
            if worker.sent_sources != self.sources
                && worker
                    .inputs
                    .send(Input::Sources(self.sources.clone()))
                    .is_ok()
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
            &self.ring,
        );
        reconcile_membership(state, &self.snapshot);
        if !self.subscribers.is_empty()
            && self.worker.is_none()
            && self.start.is_some_and(|start| Instant::now() >= start)
        {
            self.start = None;
            let cancellation = CancellationToken::default();
            let sources = self.sources.clone();
            let sent_sources = sources.clone();
            let (inputs, receiver) = mpsc::channel();
            let (sender, results) = mpsc::sync_channel(RESULTS);
            let reader = Reader {
                inputs: (inputs.clone(), receiver),
                cancellation: cancellation.clone(),
                sender,
                ring: self.ring.clone(),
                watcher: Arc::clone(&self.watcher),
            };
            if let Ok(handle) = std::thread::Builder::new()
                .name("spaceterm-server-git".into())
                .spawn(move || read(sources, reader))
            {
                self.worker = Some(Worker {
                    cancellation,
                    handle,
                    results,
                    inputs,
                    sent_sources,
                });
            }
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
            let follows = follows_root && tab.id == workspace.root_tab;
            // Outside is authoritative only after a successful read. Pending reads and
            // unavailable directories preserve the saved membership for recovery. Leaving the
            // repository also ends Opened and Repaired membership, so the Root Tab follows its
            // next repository.
            if follows
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
                changed |=
                    std::mem::take(&mut tab.worktree_membership) != WorktreeMembership::Following;
                continue;
            }
            // Membership belongs to the saved Tab, not its focused Pane or a later cd in any
            // non-Root Tab. Only the unpinned Root Tab follows the Workspace's directory.
            if tab.worktree_membership != WorktreeMembership::Following
                || (tab.worktree.is_some() && !follows)
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
        token: &CancellationToken,
    ) -> Result<ProgramExit, ProgramError> {
        let mut request = request.clone();
        request.deadline = Some(Instant::now() + Duration::from_secs(5));
        crate::platform::unix_repository_program::UnixRepositoryProgramRunner::new()
            .run(&request, stdout, token)
    }
}
/// What a reader thread owns besides its sources.
struct Reader {
    inputs: (mpsc::Sender<Input>, mpsc::Receiver<Input>),
    cancellation: CancellationToken,
    sender: mpsc::SyncSender<(GitToolStatus, DirectoryFacts)>,
    ring: Ring,
    watcher: Arc<dyn WatchSets>,
}

fn read(sources: Sources, reader: Reader) {
    let (git, home, status) = discover_git(&reader.cancellation);
    let runner: Arc<dyn RepositoryProgramRunner> = Arc::new(DeadlineRunner);
    let readers = match (git, status) {
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
    read_sources(sources, readers, status, reader);
}

pub(super) fn git_status(cancellation: &CancellationToken) -> GitToolStatus {
    discover_git(cancellation).2
}

fn discover_git(cancellation: &CancellationToken) -> (Option<PathBuf>, PathBuf, GitToolStatus) {
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
    let status = policy::tool_check::check_git(
        runner.as_ref(),
        tools.git.as_deref(),
        &home,
        Instant::now() + Duration::from_secs(5),
        cancellation,
    );
    (tools.git, home, status)
}

type Readers = (
    policy::local_read::LocalRepositoryReader,
    crate::worktrees::git::LocalWorktreeGit,
);
type Changes = Result<ChangeSummary, RepositoryReadError>;

struct ActiveRead {
    // Reject late results after a source or physical root is removed and reintroduced.
    id: u64,
    cancellation: CancellationToken,
}
struct DiscoveryJob {
    directory: PathBuf,
    /// The git directories the directory's last successful discovery found.
    known: Option<GitDirectories>,
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
        /// The repository's git directories, once discovery is complete.
        git: Option<GitDirectories>,
        discovered: bool,
    },
    CountFinished {
        id: u64,
        root: PathBuf,
        changes: Changes,
    },
}
/// A repository's git directories, canonical: the one a directory's Worktree uses and the
/// common one. They are the same for a main Worktree.
#[derive(Clone, PartialEq, Eq)]
struct GitDirectories {
    own: PathBuf,
    common: PathBuf,
    /// The root of the Worktree they belong to.
    root: PathBuf,
}
impl GitDirectories {
    /// Whether this linked Worktree is being removed. `rm -rf` and `git worktree remove` delete
    /// its `.git` file with the rest of its work tree, and its own git directory only afterwards
    /// or at prune. Until then git finds the repository that contains the Worktree, or none.
    fn removing(&self) -> bool {
        self.own != self.common
            && self.own.is_dir()
            && std::fs::symlink_metadata(self.root.join(".git")).is_err()
    }
    /// Adds what a repository's facts follow, and whether recursively: the Worktree's own git
    /// directory with its `HEAD`, `index` and operation state; the common directory with
    /// `packed-refs` and the main `HEAD` and `index`; each directory of the `refs/` tree; and
    /// the list of linked Worktrees. Never a work tree.
    ///
    /// Git writes each of these files by renaming a lock file into its directory, so a watch on
    /// the directory alone sees every write. Nothing is watched recursively, which keeps a
    /// kqueue watch to one descriptor per directory however many refs a repository holds.
    fn watched(&self, into: &mut BTreeMap<PathBuf, bool>) {
        into.insert(self.own.clone(), false);
        into.insert(self.common.clone(), false);
        let mut pending = vec![self.common.join("refs")];
        let mut directories = 0;
        while let Some(directory) = pending.pop() {
            if directories == MAX_REF_DIRECTORIES {
                break;
            }
            let Ok(entries) = std::fs::read_dir(&directory) else {
                continue;
            };
            directories += 1;
            pending.extend(
                entries
                    .flatten()
                    .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
                    .map(|entry| entry.path()),
            );
            into.insert(directory, false);
        }
        // A repository without linked Worktrees has no `worktrees/` yet. Creating it changes
        // the common directory, and the discovery that follows watches it.
        let worktrees = self.common.join("worktrees");
        if worktrees.is_dir() {
            into.entry(worktrees).or_insert(false);
        }
    }
    /// Whether a change at `path` can change facts read from this repository.
    fn concerns(&self, path: &Path) -> bool {
        [&self.own, &self.common]
            .into_iter()
            .any(|root| path.strip_prefix(root).is_ok_and(|rest| !incidental(rest)))
    }
}
/// Git writes objects, reflogs and lock files during ordinary work, and its built-in fsmonitor
/// daemon writes cookies during a read. None of them changes what a read reports, and following
/// the cookies would make each read start the next.
fn incidental(within_git: &Path) -> bool {
    within_git.components().any(|component| {
        let component = component.as_os_str().to_string_lossy();
        component == "objects" || component == "logs" || component.contains("fsmonitor--daemon")
    }) || within_git
        .extension()
        .is_some_and(|extension| extension == "lock")
}
/// Paths the watch set saw change since the supervisor last looked.
#[derive(Default)]
struct Changed {
    paths: BTreeSet<PathBuf>,
    /// Too many paths, or a watch error: every repository may have changed.
    unknown: bool,
    /// An input already tells the supervisor to look.
    sent: bool,
}
impl Changed {
    fn note(&mut self, paths: &[PathBuf], inputs: &mpsc::Sender<Input>) {
        if paths.is_empty() || self.paths.len() + paths.len() > MAX_CHANGED_PATHS {
            self.unknown = true;
            self.paths.clear();
        } else if !self.unknown {
            self.paths.extend(paths.iter().cloned());
        }
        if !std::mem::replace(&mut self.sent, true) {
            let _ = inputs.send(Input::Changed);
        }
    }
}
struct DirectoryRead {
    facts: Option<DirectoryFacts>,
    active: Option<ActiveRead>,
    /// Since when this directory waits to be read. `None` while nothing asked for a read.
    next_read: Option<Instant>,
    /// Its repository's git directories, as of the last discovery that did not fail.
    git: Option<GitDirectories>,
    /// When its last discovery ended.
    finished: Option<Instant>,
}
struct CountRead {
    fsmonitor: FsmonitorPolicy,
    active: Option<ActiveRead>,
    /// Since when this root waits to be counted. `None` while nothing asked for a count.
    next_read: Option<Instant>,
    // Queue time counts toward progress, including when both scan workers are occupied.
    requested: Option<Instant>,
    counting: Option<ChangeCounting>,
    changes: Changes,
    /// When its last count ended.
    finished: Option<Instant>,
}
impl CountRead {
    fn apply(&self, facts: &mut DirectoryFacts) {
        if let Some(repository) = &mut facts.repository {
            repository.counting = self.counting;
            repository.changes.clone_from(&self.changes);
        }
    }
}

/// One subscriber lifetime owns fixed discovery and count pools. Repeated discovery never
/// waits for a full scan; source edits update interests without restarting scans still in use.
/// The supervisor waits only for inputs and the counting indicator, never on a timer.
fn read_sources(sources: Sources, readers: Option<Readers>, status: GitToolStatus, reader: Reader) {
    let Reader {
        inputs: (inputs, receiver),
        cancellation,
        sender,
        ring,
        watcher,
    } = reader;
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
                &ring,
            ) {
                break;
            }
        }
        // The host joins the ended reader once it hears this.
        ring.ring();
        return;
    };
    // Queues hold only a bounded window of jobs. The supervisor retains at most one pending
    // discovery per interested source and one pending count per physical Worktree root, so
    // each job sends at most two events to the supervisor's inputs.
    let (discoveries, discovery_receiver) = mpsc::sync_channel::<DiscoveryJob>(DISCOVERY_WORKERS);
    let discovery_receiver = Mutex::new(discovery_receiver);
    let (counts, count_receiver) = mpsc::sync_channel::<CountJob>(COUNT_WORKERS);
    let count_receiver = Mutex::new(count_receiver);
    std::thread::scope(|scope| {
        for _ in 0..DISCOVERY_WORKERS {
            let jobs = &discovery_receiver;
            let events = &inputs;
            let cancellation = &cancellation;
            let repository = &repository;
            let worktrees = &worktrees;
            scope.spawn(move || {
                while !cancellation.is_cancelled() {
                    let job = jobs.lock().unwrap().recv();
                    match job {
                        Ok(job) if !job.active.cancellation.is_cancelled() => discover(
                            job.directory,
                            job.known,
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
            let events = &inputs;
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
                    if !report(
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
        // One watcher follows every repository. Without it, reads follow the other triggers.
        let changed = Arc::new(Mutex::new(Changed::default()));
        let watch = watcher
            .start({
                let changed = Arc::clone(&changed);
                let inputs = inputs.clone();
                Box::new(move |paths: &[PathBuf]| {
                    changed
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .note(paths, &inputs);
                })
            })
            .ok();
        let mut supervisor = ReadSupervisor {
            directories: BTreeMap::new(),
            counts: BTreeMap::new(),
            next_id: 0,
            discoveries,
            count_jobs: counts,
            sender,
            status,
            cancellation: cancellation.clone(),
            ring: ring.clone(),
            watch,
            watching: BTreeMap::new(),
            unwatchable: BTreeMap::new(),
            changed,
        };
        supervisor.set_sources(sources);
        supervisor.run(&receiver);
        // Every queued or running job observes this token. Join the fixed pools before the
        // owning worker exits, including when publication or source delivery disconnects.
        cancellation.cancel();
    });
    // The host joins the ended reader once it hears this.
    ring.ring();
}

struct ReadSupervisor {
    directories: BTreeMap<PathBuf, DirectoryRead>,
    counts: BTreeMap<PathBuf, CountRead>,
    next_id: u64,
    discoveries: mpsc::SyncSender<DiscoveryJob>,
    count_jobs: mpsc::SyncSender<CountJob>,
    sender: mpsc::SyncSender<(GitToolStatus, DirectoryFacts)>,
    status: GitToolStatus,
    cancellation: CancellationToken,
    ring: Ring,
    /// The one watch set for every repository, absent when it could not start.
    watch: Option<Box<dyn WatchSet>>,
    /// Watched directories, and whether recursively.
    watching: BTreeMap<PathBuf, bool>,
    /// Directories whose watch failed. They are not tried again while they stay requested.
    unwatchable: BTreeMap<PathBuf, bool>,
    changed: Arc<Mutex<Changed>>,
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
                    next_read: Some(Instant::now()),
                    git: None,
                    finished: None,
                });
        }
        self.prune_counts();
        self.rewatch();
    }
    fn prune_counts(&mut self) {
        // A failed read keeps the repository it last found, and with it that repository's count.
        let roots: Sources = self
            .directories
            .values()
            .filter_map(|read| {
                read.facts
                    .as_ref()
                    .and_then(|facts| facts.repository.as_ref())
                    .map(|repository| repository.root.clone())
                    .or_else(|| read.git.as_ref().map(|git| git.root.clone()))
            })
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
    /// Watches the git directories of each repository a source belongs to, and stops watching
    /// the others. A directory whose watch fails is read on the other triggers, and is tried
    /// again only once it has stopped being requested and is requested again.
    fn rewatch(&mut self) {
        let mut requested = BTreeMap::new();
        for read in self.directories.values() {
            if let Some(git) = &read.git {
                git.watched(&mut requested);
            }
        }
        let Some(watch) = &mut self.watch else {
            return;
        };
        self.unwatchable
            .retain(|path, recursive| requested.get(path) == Some(recursive));
        self.watching.retain(|path, recursive| {
            let keep = requested.get(path) == Some(recursive);
            if !keep {
                watch.unwatch(path);
            }
            keep
        });
        let mut started = Vec::new();
        for (path, recursive) in requested {
            if self.watching.contains_key(&path) || self.unwatchable.contains_key(&path) {
                continue;
            }
            let directory = WatchDirectory {
                path: path.clone(),
                recursive,
            };
            match watch.watch(&directory) {
                Ok(()) => {
                    self.watching.insert(path.clone(), recursive);
                    started.push(path);
                }
                Err(_) => {
                    self.unwatchable.insert(path, recursive);
                }
            }
        }
        if started.is_empty() {
            return;
        }
        // A file event can be lost while a watch starts. One more read once it settles covers
        // changes made between the read that found the repository and its watch.
        let settled = Instant::now() + SETTLE;
        self.reread(
            |_, read| {
                read.git.as_ref().is_some_and(|git| {
                    started
                        .iter()
                        .any(|path| path.starts_with(&git.common) || *path == git.own)
                })
            },
            |_| settled,
        );
    }
    /// Asks for another read of each directory `directories` selects, and another count of its
    /// repository, no sooner than `due` gives for its last read. A read in flight may predate
    /// the change, so it runs again once it ends.
    fn reread(
        &mut self,
        directories: impl Fn(&Path, &DirectoryRead) -> bool,
        due: impl Fn(Option<Instant>) -> Instant,
    ) {
        let mut roots = Sources::new();
        for (directory, read) in &mut self.directories {
            if !directories(directory, read) {
                continue;
            }
            let at = due(read.finished);
            read.next_read = Some(read.next_read.map_or(at, |next| next.min(at)));
            if let Some(repository) = read.facts.as_ref().and_then(|f| f.repository.as_ref()) {
                roots.insert(repository.root.clone());
            }
        }
        for root in roots {
            if let Some(count) = self.counts.get_mut(&root) {
                let at = due(count.finished);
                count.next_read = Some(count.next_read.map_or(at, |next| next.min(at)));
            }
        }
    }
    fn run(&mut self, inputs: &mpsc::Receiver<Input>) {
        while !self.cancellation.is_cancelled() {
            if !self.schedule() {
                return;
            }
            // Only the counting indicator and a settling repository have deadlines; every
            // other read waits for an input.
            let input = match self.deadline() {
                Some(due) => {
                    match inputs.recv_timeout(due.saturating_duration_since(Instant::now())) {
                        Ok(input) => input,
                        Err(mpsc::RecvTimeoutError::Timeout) => continue,
                        Err(mpsc::RecvTimeoutError::Disconnected) => return,
                    }
                }
                None => match inputs.recv() {
                    Ok(input) => input,
                    Err(_) => return,
                },
            };
            let proceed = match input {
                Input::Sources(sources) => {
                    self.set_sources(sources);
                    true
                }
                Input::Reread(directories) => {
                    // A change seen in one Worktree can change what its repository lists for
                    // the others, such as a Worktree deleted outside git.
                    let repositories: BTreeSet<PathBuf> = directories
                        .iter()
                        .filter_map(|directory| self.directories.get(directory)?.git.as_ref())
                        .map(|git| git.common.clone())
                        .collect();
                    let now = Instant::now();
                    self.reread(
                        |directory, read| {
                            directories.contains(directory)
                                || read
                                    .git
                                    .as_ref()
                                    .is_some_and(|git| repositories.contains(&git.common))
                        },
                        |_| now,
                    );
                    true
                }
                Input::Changed => {
                    // Taking the paths lets the next change send another input.
                    let Changed { paths, unknown, .. } = std::mem::take(
                        &mut *self.changed.lock().unwrap_or_else(|e| e.into_inner()),
                    );
                    let now = Instant::now();
                    self.reread(
                        |_, read| {
                            read.git.as_ref().is_some_and(|git| {
                                unknown || paths.iter().any(|path| git.concerns(path))
                            })
                        },
                        |finished| finished.map_or(now, |finished| (finished + SETTLE).max(now)),
                    );
                    true
                }
                Input::Read(event) => self.accept(event),
                Input::Stop => false,
            };
            if !proceed {
                return;
            }
        }
    }
    /// When the next requested count announces counting, or the next settling read is due. Work
    /// already due but waiting for a full queue is scheduled when a job in flight ends.
    fn deadline(&self) -> Option<Instant> {
        let now = Instant::now();
        let indicator = self
            .counts
            .values()
            .filter(|read| read.counting != Some(ChangeCounting::Counting))
            .filter_map(|read| read.requested)
            .map(|requested| requested + COUNTING_DELAY);
        let settling = self
            .directories
            .values()
            .filter(|read| read.active.is_none())
            .filter_map(|read| read.next_read)
            .chain(
                self.counts
                    .values()
                    .filter(|read| read.active.is_none())
                    .filter_map(|read| read.next_read),
            )
            .filter(|due| *due > now);
        indicator.chain(settling).min()
    }
    fn schedule(&mut self) -> bool {
        let now = Instant::now();
        // Keep the longest waiting work first, including after a bounded queue fills.
        // Unscheduled work keeps its place.
        let mut directories: Vec<_> = self
            .directories
            .iter()
            .filter(|(_, read)| read.active.is_none())
            .filter_map(|(directory, read)| Some((read.next_read?, directory.clone())))
            .filter(|(due, _)| *due <= now)
            .collect();
        directories.sort_unstable();
        for (_, directory) in directories {
            let read = self.directories.get_mut(&directory).unwrap();
            self.next_id = self.next_id.saturating_add(1);
            let cancellation =
                CancellationToken::linked(&self.cancellation, &CancellationToken::default());
            let job = DiscoveryJob {
                directory: directory.clone(),
                known: read.git.clone(),
                active: ActiveRead {
                    id: self.next_id,
                    cancellation: cancellation.clone(),
                },
            };
            match self.discoveries.try_send(job) {
                Ok(()) => {
                    read.next_read = None;
                    read.active = Some(ActiveRead {
                        id: self.next_id,
                        cancellation,
                    })
                }
                // A job in flight ends with an input, which schedules the rest.
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
            if read.active.is_none() && read.next_read.is_some_and(|due| due <= now) {
                if read.requested.is_none() {
                    read.requested = Some(now);
                    // A first count shows that it is pending. A count that follows a finished
                    // one keeps the known changes and says nothing unless it runs long enough
                    // to announce counting, so a repository that did not change publishes
                    // nothing.
                    if read.finished.is_none() && read.counting.is_none() {
                        read.counting = Some(ChangeCounting::Pending);
                        progress.push(root.clone());
                    }
                }
                if count_capacity {
                    self.next_id = self.next_id.saturating_add(1);
                    let cancellation = CancellationToken::linked(
                        &self.cancellation,
                        &CancellationToken::default(),
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
                            read.next_read = None;
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
            if read.counting != Some(ChangeCounting::Counting)
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
                git,
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
                let failed = facts.failure.is_some();
                read.facts = Some(*facts);
                if discovered {
                    read.active = None;
                    // A failed read keeps the repository it last found, as the host keeps its
                    // facts. Its git directories stay watched, and a reread of the directory
                    // still reads the rest of that repository, which lists a removed Worktree
                    // as missing.
                    if !failed {
                        read.git = git;
                    }
                    read.finished = Some(Instant::now());
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
                            next_read: Some(Instant::now()),
                            requested: Some(Instant::now()),
                            counting: Some(ChangeCounting::Pending),
                            changes: Err(RepositoryReadError::Unavailable),
                            finished: None,
                        })
                        .fsmonitor = fsmonitor;
                }
                if discovered {
                    self.rewatch();
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
                read.finished = Some(Instant::now());
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
            &self.ring,
        )
    }
}

fn discover(
    directory: PathBuf,
    known: Option<GitDirectories>,
    id: u64,
    repository: &policy::local_read::LocalRepositoryReader,
    worktrees: &crate::worktrees::git::LocalWorktreeGit,
    events: &mpsc::Sender<Input>,
    cancellation: &CancellationToken,
) {
    let mut git = None;
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
        // Facts read while the directory's Worktree is being removed describe the repository
        // that contains it, or none. They must not change saved membership.
        let removing = || known.as_ref().is_some_and(GitDirectories::removing);
        if removing() {
            return Err(RepositoryReadError::Unavailable);
        }
        if let ProbeOutcome::Repository(probe) = repository.probe(directory, cancellation)? {
            let RepositoryRoot::Local(root) = &probe.root else {
                return Err(RepositoryReadError::InvalidResponse);
            };
            // Git can preserve an alias spelling. Full scans share the physical Worktree root.
            let root = std::fs::canonicalize(root).map_err(|_| RepositoryReadError::Unavailable)?;
            fsmonitor = probe.config.fsmonitor;
            // Watches compare canonical paths: Git may spell a common directory as
            // `<dir>/../.git`, or through a symbolic link such as `/tmp`.
            if let (RepositoryRoot::Local(own), RepositoryRoot::Local(common)) =
                (&probe.git_directory, &probe.common_directory)
            {
                git = std::fs::canonicalize(own)
                    .ok()
                    .zip(std::fs::canonicalize(common).ok())
                    .map(|(own, common)| GitDirectories {
                        own,
                        common,
                        root: root.clone(),
                    });
            }
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
            // Early facts show only the repository the directory last belonged to. Another one
            // waits for the whole read, which a removal fails.
            if (known.is_none() || known == git)
                && !report(
                    events,
                    ReadEvent::Facts {
                        id,
                        facts: Box::new(facts.clone()),
                        fsmonitor,
                        git: None,
                        discovered: false,
                    },
                    cancellation,
                )
            {
                return Err(RepositoryReadError::Cancelled);
            }
            // Every Pane needs its actual membership from Git, including inactive Tabs.
            if let Some(mut list) = worktrees.list(directory, cancellation)? {
                if list.worktrees.len() > MAX_WORKTREES {
                    return Err(RepositoryReadError::OutputTooLarge);
                }
                // Each git run finds the repository again, so a directory read for the first
                // time can change repository between runs. A list that names another Worktree
                // is not this directory's.
                if let Some(record) = list.current.and_then(|current| list.worktrees.get(current))
                    && std::fs::canonicalize(&record.root).ok().as_ref()
                        != facts.repository.as_ref().map(|repository| &repository.root)
                {
                    return Err(RepositoryReadError::Unavailable);
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
        // Git also reports no repository, or the one that contains the directory, while the
        // directory is being removed. Only a read of a directory that outlasts the read can
        // change saved membership.
        super::local_filesystem()
            .revalidate_directory(&validated)
            .map_err(|_| RepositoryReadError::Unavailable)?;
        if removing() {
            return Err(RepositoryReadError::Unavailable);
        }
        Ok(())
    })();
    // A failed read reports no repository. The host keeps the last one it published.
    if let Err(failure) = result {
        facts.failure = Some(failure);
        facts.repository = None;
        facts.worktrees = None;
        git = None;
    }
    report(
        events,
        ReadEvent::Facts {
            id,
            facts: Box::new(facts),
            fsmonitor,
            git,
            discovered: true,
        },
        cancellation,
    );
}

/// Hands a job's step to the supervisor.
fn report(
    events: &mpsc::Sender<Input>,
    event: ReadEvent,
    cancellation: &CancellationToken,
) -> bool {
    !cancellation.is_cancelled() && events.send(Input::Read(event)).is_ok()
}

/// Publishes a result to the host and rings it. Bounded backpressure must remain cancellable
/// when a subscriber stops draining publications.
fn send<T>(
    sender: &mpsc::SyncSender<T>,
    mut value: T,
    cancellation: &CancellationToken,
    ring: &Ring,
) -> bool {
    loop {
        if cancellation.is_cancelled() {
            return false;
        }
        match sender.try_send(value) {
            Ok(()) => {
                ring.ring();
                return true;
            }
            Err(mpsc::TrySendError::Disconnected(_)) => return false,
            Err(mpsc::TrySendError::Full(pending)) => {
                value = pending;
                ring.ring();
                std::thread::sleep(BACKPRESSURE);
            }
        }
    }
}

#[cfg(test)]
mod tests;
