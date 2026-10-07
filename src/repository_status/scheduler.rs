//! The Repository Status read scheduler: a synchronous state machine a UI store drives.
//!
//! Callers report Interests, triggers, and read results with an explicit `Instant`, then run the
//! returned effects and re-render the Interests whose view changed. The scheduler owns the shared
//! cache per repository, probe deduplication, count coalescing, generation guards, the remote
//! debounce, the counting indicator delay, and last-known state. It reads no clock and performs no
//! IO.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use super::{
    ChangeState, ChangeSummary, FsmonitorPolicy, Freshness, GitHubCliStatus, ProbeOutcome,
    ProbedRepository, PullRequest, PullRequestError, RepositoryConfig, RepositoryHead,
    RepositoryKey, RepositoryMachine, RepositoryReadError, RepositoryRoot, RepositoryStatus,
    RepositoryStatusPreferences, RepositoryView, WatchDirectory,
};

/// How long remote triggers for one repository or Interest gather before one read runs.
pub(crate) const REMOTE_READ_DEBOUNCE: Duration = Duration::from_secs(2);

/// How long a first count runs before the view announces counting.
pub(crate) const COUNTING_INDICATOR_DELAY: Duration = Duration::from_secs(1);

/// How long a probe's directory-to-repository answer places newly arriving Interests.
pub(crate) const PROBE_CACHE_DURATION: Duration = Duration::from_secs(2);

/// One Pane or sidebar row that presents Repository Status. The caller chooses the value.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub(crate) struct InterestId(u64);

impl InterestId {
    pub(crate) const fn new(value: u64) -> Self {
        Self(value)
    }
}

/// The directory an Interest reads: a Pane's Repository Source Directory or a sidebar row's
/// directory.
///
/// A remote directory is an opaque remote spelling. It never becomes a local path.
#[derive(Clone, Eq, Hash, PartialEq)]
pub(crate) enum SourceDirectory {
    Local(PathBuf),
    Remote(Arc<str>),
}

impl SourceDirectory {
    fn of_root(root: &RepositoryRoot) -> Self {
        match root {
            RepositoryRoot::Local(path) => Self::Local(path.clone()),
            RepositoryRoot::Remote(path) => Self::Remote(path.clone()),
        }
    }

    /// Whether the spelling lies at or below `root`. A spelling outside it has left the
    /// repository; one inside may still be in a nested repository, which only a probe can tell.
    fn is_within(&self, root: &RepositoryRoot) -> bool {
        match (self, root) {
            (Self::Local(directory), RepositoryRoot::Local(root)) => directory.starts_with(root),
            (Self::Remote(directory), RepositoryRoot::Remote(root)) => {
                let root = root.trim_end_matches('/');
                root.is_empty()
                    || directory
                        .strip_prefix(root)
                        .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
            }
            _ => false,
        }
    }
}

impl fmt::Debug for SourceDirectory {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Local(_) => "SourceDirectory::Local(<redacted>)",
            Self::Remote(_) => "SourceDirectory::Remote(<redacted>)",
        })
    }
}

/// What a Pane or sidebar row reports when it registers.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Interest {
    pub(crate) machine: RepositoryMachine,
    pub(crate) directory: SourceDirectory,
    pub(crate) visible: bool,
    /// Whether the Control Connection can run reads. Local Interests pass `true`.
    pub(crate) available: bool,
    /// The Pane's finished command count at registration. Sidebar rows pass 0.
    pub(crate) finished_commands: u64,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct ProbeTarget {
    machine: RepositoryMachine,
    directory: SourceDirectory,
}

/// Identifies one probe effect. Pass it back unchanged with the result.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ProbeTicket {
    target: ProbeTarget,
    generation: u64,
}

/// Identifies one count effect. Pass it back unchanged with the result.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct CountTicket {
    key: RepositoryKey,
    generation: u64,
}

/// Identifies one Pull Request lookup effect. Pass it back unchanged with the result.
#[derive(Clone, Eq, PartialEq)]
pub(crate) struct PullRequestTicket {
    key: RepositoryKey,
    branch: Arc<str>,
    generation: u64,
}

impl fmt::Debug for PullRequestTicket {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PullRequestTicket")
            .field("generation", &self.generation)
            .finish_non_exhaustive()
    }
}

/// Work the caller runs for the scheduler.
#[derive(Clone, Eq, PartialEq)]
pub(crate) enum RepositoryEffect {
    /// Probe `directory` on `machine`, then call [`RepositoryScheduler::probe_finished`].
    Probe {
        ticket: ProbeTicket,
        machine: RepositoryMachine,
        directory: SourceDirectory,
    },
    /// Count the changes in `key`'s work tree, then call [`RepositoryScheduler::count_finished`].
    Count {
        ticket: CountTicket,
        key: RepositoryKey,
        fsmonitor: FsmonitorPolicy,
    },
    /// Look up the open or draft Pull Request for `branch` with the local GitHub CLI, then call
    /// [`RepositoryScheduler::pull_request_finished`]. `config` names the remotes.
    LookupPullRequest {
        ticket: PullRequestTicket,
        key: RepositoryKey,
        branch: Arc<str>,
        config: RepositoryConfig,
    },
    /// Watch a local repository's git directories and call
    /// [`RepositoryScheduler::repository_changed`] on any change.
    StartWatch {
        key: RepositoryKey,
        directories: Vec<WatchDirectory>,
    },
    StopWatch { key: RepositoryKey },
    /// Call [`RepositoryScheduler::tick`] at or after this instant.
    WakeAt(Instant),
}

impl fmt::Debug for RepositoryEffect {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Probe { machine, .. } => write!(formatter, "Probe({machine:?})"),
            Self::Count { fsmonitor, .. } => write!(formatter, "Count({fsmonitor:?})"),
            Self::LookupPullRequest { .. } => formatter.write_str("LookupPullRequest"),
            Self::StartWatch { directories, .. } => {
                write!(formatter, "StartWatch({} directories)", directories.len())
            }
            Self::StopWatch { .. } => formatter.write_str("StopWatch"),
            Self::WakeAt(instant) => write!(formatter, "WakeAt({instant:?})"),
        }
    }
}

/// What one scheduler input produced.
#[derive(Debug, Default)]
pub(crate) struct SchedulerUpdate {
    pub(crate) effects: Vec<RepositoryEffect>,
    /// Interests whose [`RepositoryScheduler::view`] changed, in id order.
    pub(crate) changed: Vec<InterestId>,
}

/// Where an Interest's directory resolved.
#[derive(Clone, Debug, Eq, PartialEq)]
enum Binding {
    Unknown,
    /// Not a repository, or one Repository Status deliberately hides.
    Outside,
    Repository(RepositoryKey),
}

struct InterestState {
    machine: RepositoryMachine,
    directory: SourceDirectory,
    visible: bool,
    available: bool,
    /// The Pane exited or its remote input is blocked: it keeps `frozen` and stops reading.
    stopped: bool,
    frozen: RepositoryView,
    finished_commands: u64,
    binding: Binding,
    /// A trigger was skipped while this Interest could not read.
    stale: bool,
    debounce: Option<Instant>,
    view: RepositoryView,
}

impl InterestState {
    fn new(interest: Interest) -> Self {
        Self {
            machine: interest.machine,
            directory: interest.directory,
            visible: interest.visible,
            available: interest.available,
            stopped: false,
            frozen: RepositoryView::Hidden,
            finished_commands: interest.finished_commands,
            binding: Binding::Unknown,
            stale: false,
            debounce: None,
            view: RepositoryView::Hidden,
        }
    }

    fn target(&self) -> ProbeTarget {
        ProbeTarget {
            machine: self.machine.clone(),
            directory: self.directory.clone(),
        }
    }

    fn is_at(&self, target: &ProbeTarget) -> bool {
        self.machine == target.machine && self.directory == target.directory
    }

    fn is_bound_to(&self, key: &RepositoryKey) -> bool {
        matches!(&self.binding, Binding::Repository(bound) if bound == key)
    }

    /// Whether a remote read may run for this Interest now.
    fn can_read(&self) -> bool {
        !self.stopped && self.visible && self.available
    }
}

#[derive(Clone, Copy)]
struct InFlight {
    generation: u64,
    started: Instant,
}

struct RepositoryState {
    status: Arc<RepositoryStatus>,
    /// The generation of the probe whose facts `status` carries.
    facts_generation: u64,
    config: RepositoryConfig,
    count: Option<InFlight>,
    follow_up: bool,
    debounce: Option<Instant>,
    watched: bool,
}

struct ProbeState {
    in_flight: InFlight,
    /// A trigger arrived after the probe started, so another probe follows it.
    again: bool,
    /// Repositories whose root this probe reads for a repository-wide trigger.
    for_keys: HashSet<RepositoryKey>,
}

struct CachedProbe {
    binding: Binding,
    at: Instant,
}

#[derive(Default)]
struct PullRequestState {
    value: Option<PullRequest>,
    in_flight: Option<InFlight>,
    again: bool,
}

/// Schedules every Repository Status read for one application.
pub(crate) struct RepositoryScheduler {
    toggles: RepositoryStatusPreferences,
    github_cli: GitHubCliStatus,
    interests: BTreeMap<InterestId, InterestState>,
    repositories: HashMap<RepositoryKey, RepositoryState>,
    probes: HashMap<ProbeTarget, ProbeState>,
    probe_cache: HashMap<ProbeTarget, CachedProbe>,
    pull_requests: HashMap<(RepositoryKey, Arc<str>), PullRequestState>,
    generation: u64,
    effects: Vec<RepositoryEffect>,
}

impl RepositoryScheduler {
    pub(crate) fn new(toggles: RepositoryStatusPreferences, github_cli: GitHubCliStatus) -> Self {
        Self {
            toggles,
            github_cli,
            interests: BTreeMap::new(),
            repositories: HashMap::new(),
            probes: HashMap::new(),
            probe_cache: HashMap::new(),
            pull_requests: HashMap::new(),
            generation: 0,
            effects: Vec::new(),
        }
    }

    /// What `interest` presents now. An unknown Interest presents nothing.
    pub(crate) fn view(&self, interest: InterestId) -> RepositoryView {
        self.interests
            .get(&interest)
            .map(|state| state.view.clone())
            .unwrap_or_default()
    }

    /// Registers or replaces an Interest and reads its directory.
    pub(crate) fn register(
        &mut self,
        id: InterestId,
        interest: Interest,
        now: Instant,
    ) -> SchedulerUpdate {
        self.remove_interest(id);
        self.interests.insert(id, InterestState::new(interest));
        self.place(id, now, true);
        self.finish()
    }

    pub(crate) fn unregister(&mut self, id: InterestId) -> SchedulerUpdate {
        self.remove_interest(id);
        self.finish()
    }

    /// The Interest's directory changed. A spelling outside its repository's root clears the
    /// view at once.
    pub(crate) fn set_source(
        &mut self,
        id: InterestId,
        directory: SourceDirectory,
        now: Instant,
    ) -> SchedulerUpdate {
        if let Some(interest) = self.interests.get_mut(&id)
            && interest.directory != directory
        {
            interest.directory = directory;
            self.place(id, now, false);
        }
        self.finish()
    }

    /// A hidden Interest that missed a trigger reads once when it is shown.
    pub(crate) fn set_visible(
        &mut self,
        id: InterestId,
        visible: bool,
        now: Instant,
    ) -> SchedulerUpdate {
        if let Some(interest) = self.interests.get_mut(&id) {
            let shown = visible && !interest.visible;
            interest.visible = visible;
            if shown && interest.stale {
                self.trigger(id, now, true);
            }
        }
        self.finish()
    }

    /// The Interest's Control Connection became able or unable to run reads. A restored
    /// connection reads at once when the Interest is visible.
    pub(crate) fn set_available(
        &mut self,
        id: InterestId,
        available: bool,
        now: Instant,
    ) -> SchedulerUpdate {
        if let Some(interest) = self.interests.get_mut(&id) {
            let restored = available && !interest.available;
            interest.available = available;
            if restored {
                interest.stale = true;
                if interest.visible {
                    self.trigger(id, now, true);
                }
            }
        }
        self.finish()
    }

    /// A stopped Interest, such as an exited Pane, keeps its last view as last known and stops
    /// reading.
    pub(crate) fn set_stopped(
        &mut self,
        id: InterestId,
        stopped: bool,
        now: Instant,
    ) -> SchedulerUpdate {
        if let Some(interest) = self.interests.get_mut(&id)
            && interest.stopped != stopped
        {
            interest.stopped = stopped;
            interest.debounce = None;
            if stopped {
                interest.frozen = last_known(&interest.view);
                self.rebind(id, Binding::Unknown);
            } else {
                interest.frozen = RepositoryView::Hidden;
                self.place(id, now, true);
            }
        }
        self.finish()
    }

    /// A Pane's monotonic finished command count. Counts at or below the last one are ignored.
    pub(crate) fn command_finished(
        &mut self,
        id: InterestId,
        finished_commands: u64,
        now: Instant,
    ) -> SchedulerUpdate {
        if let Some(interest) = self.interests.get_mut(&id)
            && finished_commands > interest.finished_commands
        {
            interest.finished_commands = finished_commands;
            self.trigger(id, now, false);
        }
        self.finish()
    }

    /// A watch on `key`'s git directories fired.
    pub(crate) fn repository_changed(&mut self, key: &RepositoryKey, now: Instant) -> SchedulerUpdate {
        self.request_repository(key, now, false);
        self.finish()
    }

    /// Reads every visible Interest's repository and looks up its Pull Request. Hidden Interests
    /// read when shown.
    pub(crate) fn window_activated(&mut self, now: Instant) -> SchedulerUpdate {
        if !self.toggles.show_repository_status {
            return self.finish();
        }
        let mut keys = Vec::new();
        let mut directories = Vec::new();
        for (id, interest) in &mut self.interests {
            if interest.stopped {
                continue;
            }
            if !interest.visible {
                interest.stale = true;
                continue;
            }
            match &interest.binding {
                Binding::Repository(key) if !keys.contains(key) => keys.push(key.clone()),
                Binding::Repository(_) => {}
                Binding::Unknown | Binding::Outside => directories.push(*id),
            }
        }
        for key in keys {
            self.request_repository(&key, now, false);
            if self.is_readable(&key) {
                self.lookup_pull_request(&key, now);
            }
        }
        for id in directories {
            self.request_directory(id, now, false);
        }
        self.finish()
    }

    /// Runs due debounces and the counting indicator. Call at each [`RepositoryEffect::WakeAt`].
    pub(crate) fn tick(&mut self, now: Instant) -> SchedulerUpdate {
        let due_repositories: Vec<RepositoryKey> = self
            .repositories
            .iter_mut()
            .filter(|(_, repository)| repository.debounce.is_some_and(|due| due <= now))
            .map(|(key, repository)| {
                repository.debounce = None;
                key.clone()
            })
            .collect();
        for key in due_repositories {
            self.request_repository(&key, now, true);
        }
        let due_interests: Vec<InterestId> = self
            .interests
            .iter_mut()
            .filter(|(_, interest)| interest.debounce.is_some_and(|due| due <= now))
            .map(|(id, interest)| {
                interest.debounce = None;
                *id
            })
            .collect();
        for id in due_interests {
            self.request_directory(id, now, true);
        }
        for repository in self.repositories.values_mut() {
            let announce = repository.status.changes == ChangeState::NotCounted
                && repository
                    .count
                    .is_some_and(|count| now >= count.started + COUNTING_INDICATOR_DELAY);
            if announce {
                Arc::make_mut(&mut repository.status).changes = ChangeState::Counting;
            }
        }
        self.finish()
    }

    /// Turning Repository Status off clears every view and forgets every read. Turning it on
    /// reads every Interest again.
    pub(crate) fn set_toggles(
        &mut self,
        toggles: RepositoryStatusPreferences,
        now: Instant,
    ) -> SchedulerUpdate {
        let pull_requests = self.pull_requests_enabled();
        let was_enabled = self.toggles.show_repository_status;
        self.toggles = toggles;
        match (was_enabled, toggles.show_repository_status) {
            (true, false) => self.forget_everything(),
            (false, true) => {
                let ids: Vec<InterestId> = self.interests.keys().copied().collect();
                for id in ids {
                    self.place(id, now, true);
                }
            }
            _ => {}
        }
        self.pull_requests_toggled(pull_requests, now);
        self.finish()
    }

    /// Pull Request lookups run only while the GitHub CLI is signed in.
    pub(crate) fn set_github_cli(&mut self, status: GitHubCliStatus, now: Instant) -> SchedulerUpdate {
        let pull_requests = self.pull_requests_enabled();
        self.github_cli = status;
        self.pull_requests_toggled(pull_requests, now);
        self.finish()
    }

    pub(crate) fn probe_finished(
        &mut self,
        ticket: ProbeTicket,
        result: Result<ProbeOutcome, RepositoryReadError>,
        now: Instant,
    ) -> SchedulerUpdate {
        let current = self
            .probes
            .get(&ticket.target)
            .is_some_and(|probe| probe.in_flight.generation == ticket.generation);
        if !current {
            return self.finish();
        }
        let Some(probe) = self.probes.remove(&ticket.target) else {
            return self.finish();
        };
        match result {
            Ok(outcome) => {
                self.apply_probe(&ticket.target, ticket.generation, outcome, &probe.for_keys, now)
            }
            Err(RepositoryReadError::Cancelled) => {}
            Err(error) => {
                let mut keys: HashSet<RepositoryKey> = self
                    .interests
                    .values()
                    .filter(|interest| interest.is_at(&ticket.target))
                    .filter_map(|interest| match &interest.binding {
                        Binding::Repository(key) => Some(key.clone()),
                        Binding::Unknown | Binding::Outside => None,
                    })
                    .collect();
                keys.extend(probe.for_keys.iter().cloned());
                for key in keys {
                    self.fail(&key, error);
                }
            }
        }
        if probe.again && self.is_wanted(&ticket.target, &probe.for_keys) {
            self.probe(ticket.target.clone(), None, now);
            if let Some(next) = self.probes.get_mut(&ticket.target) {
                next.for_keys.extend(probe.for_keys);
            }
        }
        self.finish()
    }

    pub(crate) fn count_finished(
        &mut self,
        ticket: CountTicket,
        result: Result<ChangeSummary, RepositoryReadError>,
        now: Instant,
    ) -> SchedulerUpdate {
        let Some(repository) = self.repositories.get_mut(&ticket.key) else {
            return self.finish();
        };
        if repository.count.map(|count| count.generation) != Some(ticket.generation) {
            return self.finish();
        }
        repository.count = None;
        match result {
            Ok(changes) => {
                let status = Arc::make_mut(&mut repository.status);
                status.changes = ChangeState::Known(changes);
                status.freshness = Freshness::Current;
                status.read_failure = None;
                status.read_at = now;
            }
            Err(RepositoryReadError::Cancelled) => {
                if repository.status.changes == ChangeState::Counting {
                    Arc::make_mut(&mut repository.status).changes = ChangeState::NotCounted;
                }
            }
            Err(error) => self.fail(&ticket.key, error),
        }
        if let Some(repository) = self.repositories.get_mut(&ticket.key)
            && std::mem::take(&mut repository.follow_up)
        {
            self.start_count(&ticket.key, now);
        }
        self.finish()
    }

    /// `Ok(None)` means the branch has no open or draft Pull Request. A failed lookup keeps the
    /// last known Pull Request; a signed-out or missing GitHub CLI clears every one.
    pub(crate) fn pull_request_finished(
        &mut self,
        ticket: PullRequestTicket,
        result: Result<Option<PullRequest>, PullRequestError>,
        now: Instant,
    ) -> SchedulerUpdate {
        let entry_key = (ticket.key.clone(), ticket.branch.clone());
        let Some(entry) = self.pull_requests.get_mut(&entry_key) else {
            return self.finish();
        };
        if entry.in_flight.map(|lookup| lookup.generation) != Some(ticket.generation) {
            return self.finish();
        }
        entry.in_flight = None;
        let again = std::mem::take(&mut entry.again);
        match result {
            Ok(value) => {
                entry.value = value;
                self.attach_pull_request(&ticket.key);
            }
            Err(PullRequestError::NotLoggedIn) => {
                return self.set_github_cli(GitHubCliStatus::SignedOut, now);
            }
            Err(PullRequestError::ToolMissing) => {
                return self.set_github_cli(GitHubCliStatus::NotFound, now);
            }
            Err(
                PullRequestError::Unavailable
                | PullRequestError::InvalidResponse
                | PullRequestError::Cancelled,
            ) => {}
        }
        if again {
            self.lookup_pull_request(&ticket.key, now);
        }
        self.finish()
    }
}

// Reads -----------------------------------------------------------------------------------------

impl RepositoryScheduler {
    fn next_generation(&mut self) -> u64 {
        self.generation += 1;
        self.generation
    }

    /// Resolves a newly placed directory: from a fresh cached probe when one exists, otherwise by
    /// probing. A directory outside the bound repository's root clears the binding at once.
    fn place(&mut self, id: InterestId, now: Instant, immediate: bool) {
        if !self.toggles.show_repository_status {
            return;
        }
        let Some(interest) = self.interests.get(&id) else {
            return;
        };
        if interest.stopped {
            return;
        }
        let target = interest.target();
        let left = matches!(
            &interest.binding,
            Binding::Repository(key) if !interest.directory.is_within(&key.root)
        );
        match self.cached(&target, now) {
            Some(Binding::Outside) => {
                self.rebind(id, Binding::Outside);
                return;
            }
            Some(Binding::Repository(key)) if self.repositories.contains_key(&key) => {
                self.rebind(id, Binding::Repository(key));
                return;
            }
            _ => {}
        }
        if left {
            self.rebind(id, Binding::Unknown);
        }
        self.request_directory(id, now, immediate);
    }

    /// Reads after a trigger for one Interest: its repository when it has one, else its
    /// directory.
    fn trigger(&mut self, id: InterestId, now: Instant, immediate: bool) {
        if !self.toggles.show_repository_status {
            return;
        }
        let Some(interest) = self.interests.get(&id) else {
            return;
        };
        if interest.stopped {
            return;
        }
        match interest.binding.clone() {
            Binding::Repository(key) => self.request_repository(&key, now, immediate),
            Binding::Unknown | Binding::Outside => self.request_directory(id, now, immediate),
        }
    }

    /// Probes an Interest's directory: at once locally, and after the debounce remotely unless
    /// `immediate`. A remote Interest that cannot read now becomes stale instead.
    fn request_directory(&mut self, id: InterestId, now: Instant, immediate: bool) {
        if !self.toggles.show_repository_status {
            return;
        }
        let Some(interest) = self.interests.get_mut(&id) else {
            return;
        };
        if interest.stopped {
            return;
        }
        let target = interest.target();
        if matches!(interest.machine, RepositoryMachine::Remote(_)) {
            if !interest.can_read() {
                interest.stale = true;
                return;
            }
            if !immediate {
                if interest.debounce.is_none() {
                    let due = now + REMOTE_READ_DEBOUNCE;
                    interest.debounce = Some(due);
                    self.effects.push(RepositoryEffect::WakeAt(due));
                }
                return;
            }
        }
        interest.debounce = None;
        self.probe(target, None, now);
    }

    /// Reads a repository from its root: at once locally, and after the debounce remotely unless
    /// `immediate`. A remote repository no Interest can read now leaves its Interests stale.
    fn request_repository(&mut self, key: &RepositoryKey, now: Instant, immediate: bool) {
        if !self.repositories.contains_key(key) {
            return;
        }
        if matches!(key.machine, RepositoryMachine::Remote(_)) {
            if !self.is_readable(key) {
                for interest in self.interests.values_mut() {
                    if interest.is_bound_to(key) {
                        interest.stale = true;
                    }
                }
                return;
            }
            if !immediate {
                if let Some(repository) = self.repositories.get_mut(key)
                    && repository.debounce.is_none()
                {
                    let due = now + REMOTE_READ_DEBOUNCE;
                    repository.debounce = Some(due);
                    self.effects.push(RepositoryEffect::WakeAt(due));
                }
                return;
            }
        }
        if let Some(repository) = self.repositories.get_mut(key) {
            repository.debounce = None;
        }
        let target = ProbeTarget {
            machine: key.machine.clone(),
            directory: SourceDirectory::of_root(&key.root),
        };
        self.probe(target, Some(key), now);
    }

    /// Whether any Interest can read `key` now. Local repositories always can.
    fn is_readable(&self, key: &RepositoryKey) -> bool {
        match key.machine {
            RepositoryMachine::Local => true,
            RepositoryMachine::Remote(_) => self
                .interests
                .values()
                .any(|interest| interest.is_bound_to(key) && interest.can_read()),
        }
    }

    /// Starts one probe per directory. A request after the running probe started probes again
    /// once it finishes; a request at the same instant joins it.
    fn probe(&mut self, target: ProbeTarget, for_key: Option<&RepositoryKey>, now: Instant) {
        for interest in self.interests.values_mut() {
            if interest.is_at(&target) || for_key.is_some_and(|key| interest.is_bound_to(key)) {
                interest.stale = false;
            }
        }
        if let Some(probe) = self.probes.get_mut(&target) {
            if probe.in_flight.started < now {
                probe.again = true;
            }
            probe.for_keys.extend(for_key.cloned());
            return;
        }
        let generation = self.next_generation();
        self.probes.insert(
            target.clone(),
            ProbeState {
                in_flight: InFlight {
                    generation,
                    started: now,
                },
                again: false,
                for_keys: for_key.cloned().into_iter().collect(),
            },
        );
        self.effects.push(RepositoryEffect::Probe {
            ticket: ProbeTicket {
                target: target.clone(),
                generation,
            },
            machine: target.machine,
            directory: target.directory,
        });
    }

    /// Whether a finished probe still has a reader that may read now.
    fn is_wanted(&self, target: &ProbeTarget, for_keys: &HashSet<RepositoryKey>) -> bool {
        if !self.toggles.show_repository_status {
            return false;
        }
        let local = matches!(target.machine, RepositoryMachine::Local);
        self.interests.values().any(|interest| {
            interest.is_at(target) && !interest.stopped && (local || interest.can_read())
        }) || for_keys
            .iter()
            .any(|key| self.repositories.contains_key(key) && self.is_readable(key))
    }

    fn apply_probe(
        &mut self,
        target: &ProbeTarget,
        generation: u64,
        outcome: ProbeOutcome,
        for_keys: &HashSet<RepositoryKey>,
        now: Instant,
    ) {
        let resolved = match &outcome {
            ProbeOutcome::Repository(repository) => Binding::Repository(RepositoryKey {
                machine: target.machine.clone(),
                root: repository.root.clone(),
            }),
            ProbeOutcome::NotRepository | ProbeOutcome::Hidden => Binding::Outside,
        };
        self.probe_cache
            .retain(|_, cached| now.saturating_duration_since(cached.at) < PROBE_CACHE_DURATION);
        self.probe_cache.insert(
            target.clone(),
            CachedProbe {
                binding: resolved.clone(),
                at: now,
            },
        );
        let placed: Vec<InterestId> = self
            .interests
            .iter()
            .filter(|(_, interest)| interest.is_at(target) && !interest.stopped)
            .map(|(id, _)| *id)
            .collect();
        for id in placed {
            self.rebind(id, resolved.clone());
        }
        if let (ProbeOutcome::Repository(repository), Binding::Repository(key)) =
            (outcome, &resolved)
            && self.is_bound(key)
        {
            self.update_repository(key, *repository, generation, now);
        }
        // A repository-wide read whose root no longer answers with that repository cannot speak
        // for directories below it, so each of them is probed on its own.
        for key in for_keys {
            if resolved == Binding::Repository(key.clone()) || !self.repositories.contains_key(key)
            {
                continue;
            }
            let bound: Vec<InterestId> = self
                .interests
                .iter()
                .filter(|(_, interest)| interest.is_bound_to(key))
                .map(|(id, _)| *id)
                .collect();
            for id in bound {
                self.request_directory(id, now, true);
            }
        }
    }

    fn update_repository(
        &mut self,
        key: &RepositoryKey,
        repository: ProbedRepository,
        generation: u64,
        now: Instant,
    ) {
        match self.repositories.get_mut(key) {
            None => {
                let directories = match key.machine {
                    RepositoryMachine::Local => watch_directories(&repository),
                    RepositoryMachine::Remote(_) => Vec::new(),
                };
                let watched = !directories.is_empty();
                if watched {
                    self.effects.push(RepositoryEffect::StartWatch {
                        key: key.clone(),
                        directories,
                    });
                }
                let status = RepositoryStatus {
                    key: key.clone(),
                    head: repository.head,
                    commit: repository.commit,
                    upstream: repository.upstream,
                    operation: repository.operation,
                    changes: ChangeState::NotCounted,
                    freshness: Freshness::Current,
                    read_at: now,
                    pull_request: None,
                    read_failure: None,
                };
                self.repositories.insert(
                    key.clone(),
                    RepositoryState {
                        status: Arc::new(status),
                        facts_generation: generation,
                        config: repository.config,
                        count: None,
                        follow_up: false,
                        debounce: None,
                        watched,
                    },
                );
                self.attach_pull_request(key);
                self.lookup_pull_request(key, now);
            }
            Some(state) if generation > state.facts_generation => {
                let status = Arc::make_mut(&mut state.status);
                let head_changed = status.head != repository.head;
                let upstream_changed = status.upstream != repository.upstream;
                status.head = repository.head;
                status.commit = repository.commit;
                status.upstream = repository.upstream;
                status.operation = repository.operation;
                status.freshness = Freshness::Current;
                status.read_failure = None;
                status.read_at = now;
                state.config = repository.config;
                state.facts_generation = generation;
                if head_changed {
                    self.attach_pull_request(key);
                }
                if head_changed || upstream_changed {
                    self.lookup_pull_request(key, now);
                }
            }
            Some(_) => {}
        }
        self.request_count(key, now);
    }

    /// Runs at most one count per repository. A request after the running count started runs
    /// exactly one more once it finishes.
    fn request_count(&mut self, key: &RepositoryKey, now: Instant) {
        let Some(repository) = self.repositories.get_mut(key) else {
            return;
        };
        match repository.count {
            Some(count) => {
                if count.started < now {
                    repository.follow_up = true;
                }
            }
            None => self.start_count(key, now),
        }
    }

    fn start_count(&mut self, key: &RepositoryKey, now: Instant) {
        let generation = self.next_generation();
        let Some(repository) = self.repositories.get_mut(key) else {
            return;
        };
        repository.count = Some(InFlight {
            generation,
            started: now,
        });
        self.effects.push(RepositoryEffect::Count {
            ticket: CountTicket {
                key: key.clone(),
                generation,
            },
            key: key.clone(),
            fsmonitor: repository.config.fsmonitor,
        });
        if repository.status.changes == ChangeState::NotCounted {
            self.effects
                .push(RepositoryEffect::WakeAt(now + COUNTING_INDICATOR_DELAY));
        }
    }

    /// Keeps the last value as last known and records the failure. Nothing retries until the
    /// next trigger.
    fn fail(&mut self, key: &RepositoryKey, error: RepositoryReadError) {
        let Some(repository) = self.repositories.get_mut(key) else {
            return;
        };
        let status = Arc::make_mut(&mut repository.status);
        status.freshness = Freshness::LastKnown {
            as_of: as_of(status),
        };
        status.read_failure = Some(error);
        if status.changes == ChangeState::Counting {
            status.changes = ChangeState::NotCounted;
        }
    }

    fn cached(&self, target: &ProbeTarget, now: Instant) -> Option<Binding> {
        self.probe_cache
            .get(target)
            .filter(|cached| now.saturating_duration_since(cached.at) < PROBE_CACHE_DURATION)
            .map(|cached| cached.binding.clone())
    }

    fn is_bound(&self, key: &RepositoryKey) -> bool {
        self.interests
            .values()
            .any(|interest| interest.is_bound_to(key))
    }

    fn rebind(&mut self, id: InterestId, binding: Binding) {
        let Some(interest) = self.interests.get_mut(&id) else {
            return;
        };
        if interest.binding == binding {
            return;
        }
        if let Binding::Repository(previous) = std::mem::replace(&mut interest.binding, binding) {
            self.release_if_unused(&previous);
        }
    }

    fn remove_interest(&mut self, id: InterestId) {
        if let Some(InterestState {
            binding: Binding::Repository(key),
            ..
        }) = self.interests.remove(&id)
        {
            self.release_if_unused(&key);
        }
    }

    /// Forgets a repository nobody presents, stopping its watch. Its in-flight results are
    /// rejected from then on.
    fn release_if_unused(&mut self, key: &RepositoryKey) {
        if self.is_bound(key) {
            return;
        }
        if let Some(repository) = self.repositories.remove(key)
            && repository.watched
        {
            self.effects
                .push(RepositoryEffect::StopWatch { key: key.clone() });
        }
        self.pull_requests.retain(|(entry, _), _| entry != key);
    }

    fn forget_everything(&mut self) {
        for (key, repository) in self.repositories.drain() {
            if repository.watched {
                self.effects.push(RepositoryEffect::StopWatch { key });
            }
        }
        self.probes.clear();
        self.probe_cache.clear();
        self.pull_requests.clear();
        for interest in self.interests.values_mut() {
            interest.binding = Binding::Unknown;
            interest.debounce = None;
            interest.stale = false;
        }
    }

    /// Recomputes every view and reports the ones that changed.
    fn finish(&mut self) -> SchedulerUpdate {
        let views: Vec<(InterestId, RepositoryView)> = self
            .interests
            .iter()
            .map(|(id, interest)| (*id, self.present(interest)))
            .collect();
        let mut changed = Vec::new();
        for (id, view) in views {
            if let Some(interest) = self.interests.get_mut(&id) {
                if interest.view != view {
                    changed.push(id);
                }
                // An equal view still takes the shared value, so every Interest on one
                // repository holds the same allocation.
                interest.view = view;
            }
        }
        SchedulerUpdate {
            effects: std::mem::take(&mut self.effects),
            changed,
        }
    }

    fn present(&self, interest: &InterestState) -> RepositoryView {
        if !self.toggles.show_repository_status {
            return RepositoryView::Hidden;
        }
        if interest.stopped {
            return interest.frozen.clone();
        }
        let Binding::Repository(key) = &interest.binding else {
            return RepositoryView::Hidden;
        };
        let Some(repository) = self.repositories.get(key) else {
            return RepositoryView::Hidden;
        };
        let view = RepositoryView::Repository(repository.status.clone());
        if interest.available {
            view
        } else {
            last_known(&view)
        }
    }
}

// Pull Requests ---------------------------------------------------------------------------------

impl RepositoryScheduler {
    fn pull_requests_enabled(&self) -> bool {
        self.toggles.pull_requests_enabled() && self.github_cli == GitHubCliStatus::SignedIn
    }

    fn pull_requests_toggled(&mut self, was_enabled: bool, now: Instant) {
        let enabled = self.pull_requests_enabled();
        if was_enabled && !enabled {
            self.pull_requests.clear();
            for repository in self.repositories.values_mut() {
                if repository.status.pull_request.is_some() {
                    Arc::make_mut(&mut repository.status).pull_request = None;
                }
            }
        } else if enabled && !was_enabled {
            let keys: Vec<RepositoryKey> = self.repositories.keys().cloned().collect();
            for key in keys {
                self.lookup_pull_request(&key, now);
            }
        }
    }

    /// One lookup per repository and branch at a time. A request after the running lookup
    /// started looks up again once it finishes.
    fn lookup_pull_request(&mut self, key: &RepositoryKey, now: Instant) {
        if !self.pull_requests_enabled() {
            return;
        }
        let Some(repository) = self.repositories.get(key) else {
            return;
        };
        let RepositoryHead::Branch(branch) = &repository.status.head else {
            return;
        };
        let branch = branch.clone();
        let config = repository.config.clone();
        let generation = self.next_generation();
        let entry = self
            .pull_requests
            .entry((key.clone(), branch.clone()))
            .or_default();
        if let Some(lookup) = entry.in_flight {
            if lookup.started < now {
                entry.again = true;
            }
            return;
        }
        entry.in_flight = Some(InFlight {
            generation,
            started: now,
        });
        self.effects.push(RepositoryEffect::LookupPullRequest {
            ticket: PullRequestTicket {
                key: key.clone(),
                branch: branch.clone(),
                generation,
            },
            key: key.clone(),
            branch,
            config,
        });
    }

    /// Presents the cached Pull Request for the repository's current branch.
    fn attach_pull_request(&mut self, key: &RepositoryKey) {
        let enabled = self.pull_requests_enabled();
        let Some(repository) = self.repositories.get_mut(key) else {
            return;
        };
        let value = match &repository.status.head {
            RepositoryHead::Branch(branch) if enabled => self
                .pull_requests
                .get(&(key.clone(), branch.clone()))
                .and_then(|entry| entry.value.clone()),
            _ => None,
        };
        if repository.status.pull_request != value {
            Arc::make_mut(&mut repository.status).pull_request = value;
        }
    }
}

fn as_of(status: &RepositoryStatus) -> Instant {
    match status.freshness {
        Freshness::LastKnown { as_of } => as_of,
        Freshness::Current => status.read_at,
    }
}

/// The view presented dimmed, as of its last successful read, without a read failure.
fn last_known(view: &RepositoryView) -> RepositoryView {
    match view {
        RepositoryView::Hidden => RepositoryView::Hidden,
        RepositoryView::Repository(status) => {
            let mut status = RepositoryStatus::clone(status);
            status.freshness = Freshness::LastKnown {
                as_of: as_of(&status),
            };
            status.read_failure = None;
            RepositoryView::Repository(Arc::new(status))
        }
    }
}

/// The git directories whose changes a local repository's facts follow: `HEAD`, `index`, and
/// operation state in the per-worktree git directory, and `packed-refs` and `refs/` in the
/// common directory. The work tree is never watched.
fn watch_directories(repository: &ProbedRepository) -> Vec<WatchDirectory> {
    let (RepositoryRoot::Local(git), RepositoryRoot::Local(common)) =
        (&repository.git_directory, &repository.common_directory)
    else {
        return Vec::new();
    };
    let mut directories = vec![WatchDirectory {
        path: git.clone(),
        recursive: false,
    }];
    if common != git {
        directories.push(WatchDirectory {
            path: common.clone(),
            recursive: false,
        });
    }
    directories.push(WatchDirectory {
        path: common.join("refs"),
        recursive: true,
    });
    directories
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    use super::*;
    use crate::repository_status::{
        ChangeTotal, Divergence, RemoteMachineKey, RepositoryOperation, Upstream,
    };

    const PANE: InterestId = InterestId::new(1);
    const OTHER: InterestId = InterestId::new(2);
    const ROW: InterestId = InterestId::new(3);

    fn local_key(root: &str) -> RepositoryKey {
        RepositoryKey {
            machine: RepositoryMachine::Local,
            root: RepositoryRoot::Local(PathBuf::from(root)),
        }
    }

    fn build_machine() -> RepositoryMachine {
        RepositoryMachine::Remote(RemoteMachineKey::new("build-01"))
    }

    fn remote_key(root: &str) -> RepositoryKey {
        RepositoryKey {
            machine: build_machine(),
            root: RepositoryRoot::Remote(root.into()),
        }
    }

    fn local(directory: &str) -> SourceDirectory {
        SourceDirectory::Local(PathBuf::from(directory))
    }

    fn remote(directory: &str) -> SourceDirectory {
        SourceDirectory::Remote(directory.into())
    }

    fn local_interest(directory: &str) -> Interest {
        Interest {
            machine: RepositoryMachine::Local,
            directory: local(directory),
            visible: true,
            available: true,
            finished_commands: 0,
        }
    }

    fn remote_interest(directory: &str) -> Interest {
        Interest {
            machine: build_machine(),
            directory: remote(directory),
            visible: true,
            available: true,
            finished_commands: 0,
        }
    }

    fn probed(key: &RepositoryKey, head: RepositoryHead) -> ProbedRepository {
        let git = match &key.root {
            RepositoryRoot::Local(root) => RepositoryRoot::Local(root.join(".git")),
            RepositoryRoot::Remote(root) => RepositoryRoot::Remote(format!("{root}/.git").into()),
        };
        ProbedRepository {
            root: key.root.clone(),
            git_directory: git.clone(),
            common_directory: git,
            head,
            commit: Some("a1b2c3d".into()),
            upstream: None,
            operation: None,
            config: RepositoryConfig::default(),
        }
    }

    fn on_branch(key: &RepositoryKey, branch: &str) -> ProbeOutcome {
        ProbeOutcome::Repository(Box::new(probed(
            key,
            RepositoryHead::Branch(branch.into()),
        )))
    }

    fn with_upstream(key: &RepositoryKey, branch: &str, ahead: u32) -> ProbeOutcome {
        let mut repository = probed(key, RepositoryHead::Branch(branch.into()));
        repository.upstream = Some(Upstream {
            name: "origin/main".into(),
            divergence: Some(Divergence { ahead, behind: 0 }),
        });
        ProbeOutcome::Repository(Box::new(repository))
    }

    fn changes(total: u32) -> ChangeSummary {
        ChangeSummary {
            total: ChangeTotal::Exact(total),
            modified: total,
            ..ChangeSummary::default()
        }
    }

    fn pull_request(number: u32) -> PullRequest {
        PullRequest {
            number,
            title: "Show Repository Status".into(),
            draft: true,
            url: format!("https://github.com/sadiksaifi/spaceterm/pull/{number}").into(),
            head: "main".into(),
            base: "trunk".into(),
        }
    }

    struct Harness {
        scheduler: RepositoryScheduler,
        start: Instant,
        now: Instant,
        effects: Vec<RepositoryEffect>,
        changed: Vec<InterestId>,
    }

    impl Harness {
        fn new() -> Self {
            Self::with(RepositoryStatusPreferences::default(), GitHubCliStatus::NotFound)
        }

        fn signed_in() -> Self {
            Self::with(RepositoryStatusPreferences::default(), GitHubCliStatus::SignedIn)
        }

        fn with(toggles: RepositoryStatusPreferences, github_cli: GitHubCliStatus) -> Self {
            let start = Instant::now();
            Self {
                scheduler: RepositoryScheduler::new(toggles, github_cli),
                start,
                now: start,
                effects: Vec::new(),
                changed: Vec::new(),
            }
        }

        fn record(&mut self, update: SchedulerUpdate) {
            self.effects.extend(update.effects);
            self.changed = update.changed;
        }

        fn advance(&mut self, milliseconds: u64) {
            self.now += Duration::from_millis(milliseconds);
        }

        fn at(&self, milliseconds: u64) -> Instant {
            self.start + Duration::from_millis(milliseconds)
        }

        fn register(&mut self, id: InterestId, interest: Interest) {
            let update = self.scheduler.register(id, interest, self.now);
            self.record(update);
        }

        fn unregister(&mut self, id: InterestId) {
            let update = self.scheduler.unregister(id);
            self.record(update);
        }

        fn set_source(&mut self, id: InterestId, directory: SourceDirectory) {
            let update = self.scheduler.set_source(id, directory, self.now);
            self.record(update);
        }

        fn set_visible(&mut self, id: InterestId, visible: bool) {
            let update = self.scheduler.set_visible(id, visible, self.now);
            self.record(update);
        }

        fn set_available(&mut self, id: InterestId, available: bool) {
            let update = self.scheduler.set_available(id, available, self.now);
            self.record(update);
        }

        fn set_stopped(&mut self, id: InterestId, stopped: bool) {
            let update = self.scheduler.set_stopped(id, stopped, self.now);
            self.record(update);
        }

        fn command_finished(&mut self, id: InterestId, finished_commands: u64) {
            let update = self
                .scheduler
                .command_finished(id, finished_commands, self.now);
            self.record(update);
        }

        fn repository_changed(&mut self, key: &RepositoryKey) {
            let update = self.scheduler.repository_changed(key, self.now);
            self.record(update);
        }

        fn window_activated(&mut self) {
            let update = self.scheduler.window_activated(self.now);
            self.record(update);
        }

        fn tick(&mut self) {
            let update = self.scheduler.tick(self.now);
            self.record(update);
        }

        fn set_toggles(&mut self, show_repository_status: bool, show_pull_requests: bool) {
            let toggles = RepositoryStatusPreferences {
                show_repository_status,
                show_pull_requests,
            };
            let update = self.scheduler.set_toggles(toggles, self.now);
            self.record(update);
        }

        fn finish_probe(
            &mut self,
            ticket: ProbeTicket,
            result: Result<ProbeOutcome, RepositoryReadError>,
        ) {
            let update = self.scheduler.probe_finished(ticket, result, self.now);
            self.record(update);
        }

        fn finish_count(
            &mut self,
            ticket: CountTicket,
            result: Result<ChangeSummary, RepositoryReadError>,
        ) {
            let update = self.scheduler.count_finished(ticket, result, self.now);
            self.record(update);
        }

        fn finish_lookup(
            &mut self,
            ticket: PullRequestTicket,
            result: Result<Option<PullRequest>, PullRequestError>,
        ) {
            let update = self
                .scheduler
                .pull_request_finished(ticket, result, self.now);
            self.record(update);
        }

        fn drain<T>(&mut self, mut pick: impl FnMut(&RepositoryEffect) -> Option<T>) -> Vec<T> {
            let mut picked = Vec::new();
            self.effects.retain(|effect| match pick(effect) {
                Some(value) => {
                    picked.push(value);
                    false
                }
                None => true,
            });
            picked
        }

        fn probes(&mut self) -> Vec<(ProbeTicket, SourceDirectory)> {
            self.drain(|effect| match effect {
                RepositoryEffect::Probe {
                    ticket, directory, ..
                } => Some((ticket.clone(), directory.clone())),
                _ => None,
            })
        }

        fn only_probe(&mut self) -> (ProbeTicket, SourceDirectory) {
            let mut probes = self.probes();
            assert_eq!(probes.len(), 1, "expected exactly one probe");
            probes.remove(0)
        }

        fn counts(&mut self) -> Vec<(CountTicket, RepositoryKey)> {
            self.drain(|effect| match effect {
                RepositoryEffect::Count { ticket, key, .. } => Some((ticket.clone(), key.clone())),
                _ => None,
            })
        }

        fn only_count(&mut self) -> CountTicket {
            let mut counts = self.counts();
            assert_eq!(counts.len(), 1, "expected exactly one count");
            counts.remove(0).0
        }

        fn lookups(&mut self) -> Vec<(PullRequestTicket, Arc<str>)> {
            self.drain(|effect| match effect {
                RepositoryEffect::LookupPullRequest { ticket, branch, .. } => {
                    Some((ticket.clone(), branch.clone()))
                }
                _ => None,
            })
        }

        fn wakes(&mut self) -> Vec<Instant> {
            self.drain(|effect| match effect {
                RepositoryEffect::WakeAt(instant) => Some(*instant),
                _ => None,
            })
        }

        fn watch_starts(&mut self) -> Vec<(RepositoryKey, Vec<WatchDirectory>)> {
            self.drain(|effect| match effect {
                RepositoryEffect::StartWatch { key, directories } => {
                    Some((key.clone(), directories.clone()))
                }
                _ => None,
            })
        }

        fn watch_stops(&mut self) -> Vec<RepositoryKey> {
            self.drain(|effect| match effect {
                RepositoryEffect::StopWatch { key } => Some(key.clone()),
                _ => None,
            })
        }

        /// Answers the one pending probe with `outcome` and the count that follows with `total`.
        fn read(&mut self, outcome: ProbeOutcome, total: u32) {
            let (probe, _) = self.only_probe();
            self.finish_probe(probe, Ok(outcome));
            let count = self.only_count();
            self.finish_count(count, Ok(changes(total)));
            // The read is complete, so its counting indicator wake no longer matters.
            self.wakes();
        }

        fn status(&self, id: InterestId) -> Arc<RepositoryStatus> {
            match self.scheduler.view(id) {
                RepositoryView::Repository(status) => status,
                RepositoryView::Hidden => panic!("the Interest presents nothing"),
            }
        }

        fn hidden(&self, id: InterestId) -> bool {
            self.scheduler.view(id) == RepositoryView::Hidden
        }
    }

    #[test]
    fn a_local_interest_probes_then_counts() {
        let key = local_key("/src/app");
        let mut harness = Harness::new();
        harness.register(PANE, local_interest("/src/app/sub"));
        let (probe, directory) = harness.only_probe();
        assert_eq!(directory, local("/src/app/sub"));
        assert!(harness.hidden(PANE));

        harness.finish_probe(probe, Ok(on_branch(&key, "main")));
        assert_eq!(harness.changed, vec![PANE]);
        let status = harness.status(PANE);
        assert_eq!(status.key, key);
        assert_eq!(status.head, RepositoryHead::Branch("main".into()));
        assert_eq!(status.commit.as_deref(), Some("a1b2c3d"));
        assert_eq!(status.changes, ChangeState::NotCounted);
        assert_eq!(status.freshness, Freshness::Current);

        let count = harness.only_count();
        harness.finish_count(count, Ok(changes(4)));
        assert_eq!(harness.changed, vec![PANE]);
        assert_eq!(harness.status(PANE).changes, ChangeState::Known(changes(4)));
        assert_eq!(harness.status(PANE).read_failure, None);
    }

    #[test]
    fn interests_in_one_repository_share_one_status() {
        let key = local_key("/src/app");
        let mut harness = Harness::new();
        harness.register(PANE, local_interest("/src/app/sub"));
        harness.read(on_branch(&key, "main"), 4);
        harness.register(OTHER, local_interest("/src/app"));
        harness.read(on_branch(&key, "main"), 4);

        let (RepositoryView::Repository(pane), RepositoryView::Repository(other)) =
            (harness.scheduler.view(PANE), harness.scheduler.view(OTHER))
        else {
            panic!("both Interests present the repository");
        };
        assert!(Arc::ptr_eq(&pane, &other));
    }

    #[test]
    fn triggers_during_a_count_coalesce_into_one_follow_up() {
        let key = local_key("/src/app");
        let mut harness = Harness::new();
        harness.register(PANE, local_interest("/src/app"));
        let (probe, _) = harness.only_probe();
        harness.finish_probe(probe, Ok(on_branch(&key, "main")));
        let first = harness.only_count();

        for finished in 1..=3 {
            harness.advance(100);
            harness.command_finished(PANE, finished);
            for (probe, _) in harness.probes() {
                harness.finish_probe(probe, Ok(on_branch(&key, "main")));
            }
        }
        assert!(harness.counts().is_empty(), "one count runs at a time");

        harness.advance(100);
        harness.finish_count(first, Ok(changes(1)));
        let follow_up = harness.only_count();
        harness.advance(100);
        harness.finish_count(follow_up, Ok(changes(2)));
        assert!(harness.counts().is_empty(), "the follow-up absorbs every trigger");
        assert_eq!(harness.status(PANE).changes, ChangeState::Known(changes(2)));
    }

    #[test]
    fn a_trigger_during_a_probe_probes_again_after_it() {
        let key = local_key("/src/app");
        let mut harness = Harness::new();
        harness.register(PANE, local_interest("/src/app"));
        let (probe, _) = harness.only_probe();
        harness.advance(50);
        harness.command_finished(PANE, 1);
        assert!(harness.probes().is_empty(), "one probe per directory runs at a time");

        harness.finish_probe(probe, Ok(on_branch(&key, "main")));
        let (again, directory) = harness.only_probe();
        assert_eq!(directory, local("/src/app"));
        harness.finish_probe(again, Ok(on_branch(&key, "main")));
        assert!(harness.probes().is_empty());
    }

    #[test]
    fn results_from_superseded_reads_are_rejected() {
        let key = local_key("/src/app");
        let mut harness = Harness::new();
        harness.register(PANE, local_interest("/src/app"));
        let (old_probe, _) = harness.only_probe();
        harness.finish_probe(old_probe.clone(), Ok(on_branch(&key, "main")));
        let old_count = harness.only_count();

        harness.unregister(PANE);
        harness.register(PANE, local_interest("/src/app"));
        let (probe, _) = harness.only_probe();
        harness.finish_probe(probe, Ok(on_branch(&key, "main")));
        let count = harness.only_count();

        harness.finish_count(old_count, Ok(changes(9)));
        assert!(harness.changed.is_empty());
        assert_eq!(harness.status(PANE).changes, ChangeState::NotCounted);
        harness.finish_probe(old_probe, Ok(on_branch(&key, "stale")));
        assert!(harness.changed.is_empty());
        assert!(harness.counts().is_empty());

        harness.finish_count(count, Ok(changes(1)));
        assert_eq!(harness.status(PANE).changes, ChangeState::Known(changes(1)));
    }

    #[test]
    fn an_older_probe_never_replaces_newer_facts() {
        let key = local_key("/src/app");
        let mut harness = Harness::new();
        harness.register(PANE, local_interest("/src/app"));
        harness.register(OTHER, local_interest("/src/app/sub"));
        let mut probes = harness.probes();
        assert_eq!(probes.len(), 2);
        let (newer, _) = probes.remove(1);
        let (older, _) = probes.remove(0);
        harness.finish_probe(newer, Ok(on_branch(&key, "dev")));
        harness.finish_probe(older, Ok(on_branch(&key, "main")));
        assert_eq!(harness.status(PANE).head, RepositoryHead::Branch("dev".into()));
    }

    #[test]
    fn a_slow_first_count_shows_counting_after_one_second() {
        let key = local_key("/src/app");
        let mut harness = Harness::new();
        harness.register(PANE, local_interest("/src/app"));
        let (probe, _) = harness.only_probe();
        harness.finish_probe(probe, Ok(on_branch(&key, "main")));
        let count = harness.only_count();
        assert_eq!(harness.wakes(), vec![harness.at(1_000)]);

        harness.advance(999);
        harness.tick();
        assert!(harness.changed.is_empty());
        assert_eq!(harness.status(PANE).changes, ChangeState::NotCounted);

        harness.advance(1);
        harness.tick();
        assert_eq!(harness.changed, vec![PANE]);
        assert_eq!(harness.status(PANE).changes, ChangeState::Counting);

        harness.finish_count(count, Ok(changes(3)));
        assert_eq!(harness.status(PANE).changes, ChangeState::Known(changes(3)));
    }

    #[test]
    fn rereading_a_repository_keeps_its_counts_until_replaced() {
        let key = local_key("/src/app");
        let mut harness = Harness::new();
        harness.register(PANE, local_interest("/src/app"));
        harness.read(on_branch(&key, "main"), 4);

        harness.advance(100);
        harness.command_finished(PANE, 1);
        let (probe, _) = harness.only_probe();
        harness.finish_probe(probe, Ok(on_branch(&key, "main")));
        let count = harness.only_count();
        assert!(harness.wakes().is_empty());
        harness.advance(5_000);
        harness.tick();
        assert_eq!(harness.status(PANE).changes, ChangeState::Known(changes(4)));

        harness.finish_count(count, Ok(changes(5)));
        assert_eq!(harness.status(PANE).changes, ChangeState::Known(changes(5)));
    }

    #[test]
    fn leaving_a_repository_clears_the_view_at_once() {
        let key = local_key("/src/app");
        let mut harness = Harness::new();
        harness.register(PANE, local_interest("/src/app"));
        harness.read(on_branch(&key, "main"), 4);
        harness.watch_starts();

        harness.advance(100);
        harness.set_source(PANE, local("/tmp"));
        assert_eq!(harness.changed, vec![PANE]);
        assert!(harness.hidden(PANE));
        assert_eq!(harness.watch_stops(), vec![key]);

        let (probe, directory) = harness.only_probe();
        assert_eq!(directory, local("/tmp"));
        harness.finish_probe(probe, Ok(ProbeOutcome::NotRepository));
        assert!(harness.hidden(PANE));
        assert!(harness.counts().is_empty());

        let other = local_key("/src/other");
        harness.set_source(PANE, local("/src/other"));
        harness.read(on_branch(&other, "dev"), 0);
        assert_eq!(harness.status(PANE).key, other);
    }

    #[test]
    fn moving_within_a_repository_keeps_the_view_until_the_probe_answers() {
        let key = local_key("/src/app");
        let nested = local_key("/src/app/vendor/lib");
        let mut harness = Harness::new();
        harness.register(PANE, local_interest("/src/app"));
        harness.read(on_branch(&key, "main"), 4);

        harness.advance(100);
        harness.set_source(PANE, local("/src/app/vendor/lib"));
        assert!(harness.changed.is_empty());
        assert_eq!(harness.status(PANE).key, key);

        let (probe, _) = harness.only_probe();
        harness.finish_probe(probe, Ok(on_branch(&nested, "main")));
        assert_eq!(harness.changed, vec![PANE]);
        assert_eq!(harness.status(PANE).key, nested);
        assert_eq!(harness.status(PANE).changes, ChangeState::NotCounted);
    }

    #[test]
    fn hidden_and_non_repository_directories_present_nothing() {
        let mut harness = Harness::new();
        harness.register(PANE, local_interest("/src/app/.git"));
        let (probe, _) = harness.only_probe();
        harness.finish_probe(probe, Ok(ProbeOutcome::Hidden));
        assert!(harness.hidden(PANE));
        assert!(harness.counts().is_empty());
        assert!(harness.watch_starts().is_empty());
    }

    #[test]
    fn a_failed_probe_keeps_the_last_value_as_last_known_without_retrying() {
        let key = local_key("/src/app");
        let mut harness = Harness::new();
        harness.register(PANE, local_interest("/src/app"));
        harness.read(on_branch(&key, "main"), 4);
        harness.watch_starts();
        let read_at = harness.now;

        harness.advance(300_000);
        harness.command_finished(PANE, 1);
        let (probe, _) = harness.only_probe();
        harness.finish_probe(probe, Err(RepositoryReadError::Unavailable));
        assert_eq!(harness.changed, vec![PANE]);
        let status = harness.status(PANE);
        assert_eq!(status.freshness, Freshness::LastKnown { as_of: read_at });
        assert_eq!(status.read_failure, Some(RepositoryReadError::Unavailable));
        assert_eq!(status.changes, ChangeState::Known(changes(4)));
        assert!(harness.effects.is_empty(), "no retry until the next trigger");

        harness.advance(100);
        harness.command_finished(PANE, 2);
        let (probe, _) = harness.only_probe();
        harness.finish_probe(probe, Ok(on_branch(&key, "main")));
        let status = harness.status(PANE);
        assert_eq!(status.freshness, Freshness::Current);
        assert_eq!(status.read_failure, None);
    }

    #[test]
    fn a_failed_first_count_drops_counting_and_keeps_last_known() {
        let key = local_key("/src/app");
        let mut harness = Harness::new();
        harness.register(PANE, local_interest("/src/app"));
        let (probe, _) = harness.only_probe();
        harness.finish_probe(probe, Ok(on_branch(&key, "main")));
        let read_at = harness.now;
        let count = harness.only_count();
        harness.advance(1_500);
        harness.tick();
        assert_eq!(harness.status(PANE).changes, ChangeState::Counting);

        harness.finish_count(count, Err(RepositoryReadError::TimedOut));
        let status = harness.status(PANE);
        assert_eq!(status.changes, ChangeState::NotCounted);
        assert_eq!(status.freshness, Freshness::LastKnown { as_of: read_at });
        assert_eq!(status.read_failure, Some(RepositoryReadError::TimedOut));
        assert!(harness.counts().is_empty());
    }

    #[test]
    fn remote_triggers_debounce_two_seconds_per_repository() {
        let key = remote_key("/srv/api");
        let mut harness = Harness::new();
        harness.register(PANE, remote_interest("/srv/api"));
        harness.register(OTHER, remote_interest("/srv/api/web"));
        for (probe, _) in harness.probes() {
            harness.finish_probe(probe, Ok(on_branch(&key, "main")));
        }
        let count = harness.only_count();
        harness.finish_count(count, Ok(changes(0)));
        harness.wakes();

        harness.command_finished(PANE, 1);
        harness.advance(500);
        harness.command_finished(OTHER, 1);
        harness.window_activated();
        assert!(harness.probes().is_empty());
        assert_eq!(harness.wakes(), vec![harness.at(2_000)]);

        harness.advance(1_499);
        harness.tick();
        assert!(harness.probes().is_empty());
        harness.advance(1);
        harness.tick();
        let (_, directory) = harness.only_probe();
        assert_eq!(directory, remote("/srv/api"));
    }

    #[test]
    fn remote_source_changes_debounce_and_leaving_clears_at_once() {
        let key = remote_key("/srv/api");
        let mut harness = Harness::new();
        harness.register(PANE, remote_interest("/srv/api"));
        harness.read(on_branch(&key, "main"), 0);
        harness.wakes();

        harness.set_source(PANE, remote("/srv/other"));
        assert!(harness.hidden(PANE));
        assert!(harness.probes().is_empty());
        assert_eq!(harness.wakes(), vec![harness.at(2_000)]);

        harness.advance(2_000);
        harness.tick();
        let (_, directory) = harness.only_probe();
        assert_eq!(directory, remote("/srv/other"));
    }

    #[test]
    fn a_hidden_remote_interest_reads_once_when_shown() {
        let key = remote_key("/srv/api");
        let mut harness = Harness::new();
        harness.register(PANE, remote_interest("/srv/api"));
        harness.read(on_branch(&key, "main"), 0);

        harness.set_visible(PANE, false);
        harness.command_finished(PANE, 1);
        harness.window_activated();
        harness.advance(5_000);
        harness.tick();
        assert!(harness.probes().is_empty());

        harness.set_visible(PANE, true);
        let (_, directory) = harness.only_probe();
        assert_eq!(directory, remote("/srv/api"));

        harness.set_visible(PANE, false);
        harness.set_visible(PANE, true);
        assert!(harness.probes().is_empty(), "a current value is not read again");
    }

    #[test]
    fn a_hidden_remote_interest_registers_without_reading() {
        let mut harness = Harness::new();
        harness.register(
            PANE,
            Interest {
                visible: false,
                ..remote_interest("/srv/api")
            },
        );
        assert!(harness.probes().is_empty());
        harness.set_visible(PANE, true);
        assert_eq!(harness.only_probe().1, remote("/srv/api"));
    }

    #[test]
    fn an_unavailable_remote_machine_is_not_read_and_presents_last_known() {
        let key = remote_key("/srv/api");
        let mut harness = Harness::new();
        harness.register(PANE, remote_interest("/srv/api"));
        harness.read(on_branch(&key, "main"), 2);
        let read_at = harness.now;

        harness.advance(180_000);
        harness.set_available(PANE, false);
        assert_eq!(harness.changed, vec![PANE]);
        let status = harness.status(PANE);
        assert_eq!(status.freshness, Freshness::LastKnown { as_of: read_at });
        assert_eq!(status.read_failure, None);
        assert_eq!(status.changes, ChangeState::Known(changes(2)));

        harness.command_finished(PANE, 1);
        harness.repository_changed(&key);
        harness.window_activated();
        harness.set_source(PANE, remote("/srv/api/web"));
        harness.advance(5_000);
        harness.tick();
        assert!(harness.probes().is_empty());

        harness.set_available(PANE, true);
        assert_eq!(harness.probes().len(), 1);
    }

    #[test]
    fn an_unavailable_remote_interest_registers_without_reading() {
        let mut harness = Harness::new();
        harness.register(
            PANE,
            Interest {
                available: false,
                ..remote_interest("/srv/api")
            },
        );
        assert!(harness.probes().is_empty());
        harness.set_available(PANE, true);
        assert_eq!(harness.only_probe().1, remote("/srv/api"));
    }

    #[test]
    fn window_activation_reads_visible_interests_and_defers_hidden_ones() {
        let app = local_key("/src/app");
        let other = local_key("/src/other");
        let mut harness = Harness::new();
        harness.register(PANE, local_interest("/src/app"));
        harness.read(on_branch(&app, "main"), 0);
        harness.register(OTHER, local_interest("/src/other"));
        harness.read(on_branch(&other, "main"), 0);
        harness.set_visible(OTHER, false);

        harness.advance(100);
        harness.window_activated();
        assert_eq!(harness.only_probe().1, local("/src/app"));

        harness.set_visible(OTHER, true);
        assert_eq!(harness.only_probe().1, local("/src/other"));
    }

    #[test]
    fn local_triggers_read_hidden_interests_immediately() {
        let key = local_key("/src/app");
        let mut harness = Harness::new();
        harness.register(PANE, local_interest("/src/app"));
        harness.read(on_branch(&key, "main"), 0);
        harness.set_visible(PANE, false);

        harness.advance(100);
        harness.command_finished(PANE, 1);
        assert_eq!(harness.probes().len(), 1);
        harness.repository_changed(&key);
        assert!(harness.wakes().is_empty());
    }

    #[test]
    fn the_watch_follows_the_first_and_last_interest_in_a_local_repository() {
        let key = local_key("/src/worktree");
        let mut repository = probed(&key, RepositoryHead::Branch("main".into()));
        repository.git_directory =
            RepositoryRoot::Local(PathBuf::from("/src/app/.git/worktrees/worktree"));
        repository.common_directory = RepositoryRoot::Local(PathBuf::from("/src/app/.git"));
        let outcome = ProbeOutcome::Repository(Box::new(repository));

        let mut harness = Harness::new();
        harness.register(PANE, local_interest("/src/worktree"));
        harness.read(outcome.clone(), 0);
        assert_eq!(
            harness.watch_starts(),
            vec![(
                key.clone(),
                vec![
                    WatchDirectory {
                        path: PathBuf::from("/src/app/.git/worktrees/worktree"),
                        recursive: false,
                    },
                    WatchDirectory {
                        path: PathBuf::from("/src/app/.git"),
                        recursive: false,
                    },
                    WatchDirectory {
                        path: PathBuf::from("/src/app/.git/refs"),
                        recursive: true,
                    },
                ],
            )]
        );

        harness.advance(5_000);
        harness.register(OTHER, local_interest("/src/worktree/src"));
        harness.read(outcome, 0);
        assert!(harness.watch_starts().is_empty());

        harness.unregister(PANE);
        assert!(harness.watch_stops().is_empty());
        harness.unregister(OTHER);
        assert_eq!(harness.watch_stops(), vec![key]);
    }

    #[test]
    fn a_watch_change_probes_the_repository_root() {
        let key = local_key("/src/app");
        let mut harness = Harness::new();
        harness.register(PANE, local_interest("/src/app/sub"));
        harness.read(on_branch(&key, "main"), 0);

        harness.advance(100);
        harness.repository_changed(&key);
        assert_eq!(harness.only_probe().1, local("/src/app"));
        harness.repository_changed(&local_key("/src/unknown"));
        assert!(harness.probes().is_empty());
    }

    #[test]
    fn a_root_probe_that_finds_another_answer_reprobes_each_interest() {
        let key = local_key("/src/app");
        let mut harness = Harness::new();
        harness.register(PANE, local_interest("/src/app/sub"));
        harness.read(on_branch(&key, "main"), 0);

        harness.advance(100);
        harness.repository_changed(&key);
        let (probe, _) = harness.only_probe();
        harness.finish_probe(probe, Ok(ProbeOutcome::NotRepository));
        let (probe, directory) = harness.only_probe();
        assert_eq!(directory, local("/src/app/sub"));
        harness.finish_probe(probe, Ok(ProbeOutcome::NotRepository));
        assert!(harness.hidden(PANE));
    }

    #[test]
    fn remote_repositories_are_not_watched() {
        let key = remote_key("/srv/api");
        let mut harness = Harness::new();
        harness.register(PANE, remote_interest("/srv/api"));
        harness.read(on_branch(&key, "main"), 0);
        assert!(harness.watch_starts().is_empty());
        harness.unregister(PANE);
        assert!(harness.watch_stops().is_empty());
    }

    #[test]
    fn finished_command_counts_trigger_only_when_they_grow() {
        let key = local_key("/src/app");
        let mut harness = Harness::new();
        harness.register(
            PANE,
            Interest {
                finished_commands: 4,
                ..local_interest("/src/app")
            },
        );
        harness.read(on_branch(&key, "main"), 0);

        harness.advance(100);
        harness.command_finished(PANE, 4);
        assert!(harness.probes().is_empty());
        harness.command_finished(PANE, 5);
        assert_eq!(harness.probes().len(), 1);
    }

    #[test]
    fn interests_arriving_together_share_one_probe_and_later_ones_use_its_answer() {
        let key = local_key("/src/app");
        let mut harness = Harness::new();
        harness.register(PANE, local_interest("/src/app"));
        harness.register(OTHER, local_interest("/src/app"));
        harness.read(on_branch(&key, "main"), 1);
        harness.watch_starts();
        assert_eq!(harness.status(OTHER).key, key);

        harness.advance(1_000);
        harness.register(ROW, local_interest("/src/app"));
        assert!(harness.effects.is_empty());
        assert_eq!(harness.changed, vec![ROW]);
        assert_eq!(harness.status(ROW).changes, ChangeState::Known(changes(1)));

        harness.advance(PROBE_CACHE_DURATION.as_millis() as u64);
        harness.unregister(ROW);
        harness.register(ROW, local_interest("/src/app"));
        assert_eq!(harness.probes().len(), 1);
    }

    #[test]
    fn a_stopped_interest_keeps_its_last_value_and_stops_reading() {
        let key = local_key("/src/app");
        let mut harness = Harness::new();
        harness.register(PANE, local_interest("/src/app"));
        harness.read(on_branch(&key, "main"), 3);
        let read_at = harness.now;
        harness.watch_starts();

        harness.advance(60_000);
        harness.set_stopped(PANE, true);
        assert_eq!(harness.changed, vec![PANE]);
        let status = harness.status(PANE);
        assert_eq!(status.freshness, Freshness::LastKnown { as_of: read_at });
        assert_eq!(status.changes, ChangeState::Known(changes(3)));
        assert_eq!(harness.watch_stops(), vec![key]);

        harness.command_finished(PANE, 1);
        harness.window_activated();
        harness.set_source(PANE, local("/src/other"));
        assert!(harness.probes().is_empty());
        assert_eq!(harness.status(PANE).freshness, Freshness::LastKnown { as_of: read_at });
    }

    #[test]
    fn unregistering_presents_nothing() {
        let key = local_key("/src/app");
        let mut harness = Harness::new();
        harness.register(PANE, local_interest("/src/app"));
        harness.read(on_branch(&key, "main"), 0);
        harness.unregister(PANE);
        assert!(harness.hidden(PANE));
    }

    #[test]
    fn turning_off_repository_status_hides_views_and_stops_work() {
        let key = local_key("/src/app");
        let mut harness = Harness::signed_in();
        harness.register(PANE, local_interest("/src/app"));
        harness.read(on_branch(&key, "main"), 0);
        harness.watch_starts();
        harness.lookups();

        harness.advance(100);
        harness.command_finished(PANE, 1);
        let (probe, _) = harness.only_probe();
        harness.set_toggles(false, true);
        assert_eq!(harness.changed, vec![PANE]);
        assert!(harness.hidden(PANE));
        assert_eq!(harness.watch_stops(), vec![key.clone()]);

        harness.finish_probe(probe, Ok(on_branch(&key, "main")));
        harness.command_finished(PANE, 2);
        harness.window_activated();
        assert!(harness.hidden(PANE));
        assert!(harness.effects.is_empty());

        harness.set_toggles(true, true);
        assert_eq!(harness.probes().len(), 1);
    }

    #[test]
    fn a_failed_pane_registered_while_off_reads_when_turned_on() {
        let mut harness = Harness::with(
            RepositoryStatusPreferences {
                show_repository_status: false,
                show_pull_requests: true,
            },
            GitHubCliStatus::NotFound,
        );
        harness.register(PANE, local_interest("/src/app"));
        assert!(harness.effects.is_empty());
        harness.set_toggles(true, true);
        assert_eq!(harness.probes().len(), 1);
    }

    #[test]
    fn pull_requests_are_looked_up_on_branch_and_upstream_changes_and_activation() {
        let key = local_key("/src/app");
        let mut harness = Harness::signed_in();
        harness.register(PANE, local_interest("/src/app"));
        harness.read(on_branch(&key, "main"), 0);
        let (lookup, branch) = harness.lookups().remove(0);
        assert_eq!(&*branch, "main");
        harness.finish_lookup(lookup, Ok(Some(pull_request(478))));
        assert_eq!(harness.changed, vec![PANE]);
        assert_eq!(harness.status(PANE).pull_request, Some(pull_request(478)));

        let mut finished = 0;
        let mut trigger = |harness: &mut Harness, outcome: ProbeOutcome| {
            finished += 1;
            harness.advance(100);
            harness.command_finished(PANE, finished);
            harness.read(outcome, 0);
            harness.lookups()
        };
        assert!(trigger(&mut harness, on_branch(&key, "main")).is_empty());
        assert_eq!(trigger(&mut harness, with_upstream(&key, "main", 1)).len(), 1);
        let lookups = trigger(&mut harness, on_branch(&key, "dev"));
        assert_eq!(lookups.len(), 1);
        assert_eq!(&*lookups[0].1, "dev");
        assert_eq!(harness.status(PANE).pull_request, None);

        harness.advance(100);
        harness.window_activated();
        assert!(harness.lookups().is_empty(), "one lookup per branch runs at a time");
        let (lookup, _) = lookups.into_iter().next().unwrap();
        harness.finish_lookup(lookup, Ok(None));
        assert_eq!(harness.lookups().len(), 1, "activation looks up again afterwards");
    }

    #[test]
    fn a_cached_pull_request_returns_with_its_branch() {
        let key = local_key("/src/app");
        let mut harness = Harness::signed_in();
        harness.register(PANE, local_interest("/src/app"));
        harness.read(on_branch(&key, "main"), 0);
        let (lookup, _) = harness.lookups().remove(0);
        harness.finish_lookup(lookup, Ok(Some(pull_request(478))));

        harness.advance(100);
        harness.command_finished(PANE, 1);
        harness.read(on_branch(&key, "dev"), 0);
        harness.advance(100);
        harness.command_finished(PANE, 2);
        harness.read(on_branch(&key, "main"), 0);
        assert_eq!(harness.status(PANE).pull_request, Some(pull_request(478)));
    }

    #[test]
    fn a_failed_pull_request_lookup_keeps_the_last_pull_request() {
        let key = local_key("/src/app");
        let mut harness = Harness::signed_in();
        harness.register(PANE, local_interest("/src/app"));
        harness.read(on_branch(&key, "main"), 0);
        let (lookup, _) = harness.lookups().remove(0);
        harness.finish_lookup(lookup, Ok(Some(pull_request(478))));

        harness.advance(100);
        harness.window_activated();
        let (lookup, _) = harness.lookups().remove(0);
        harness.finish_lookup(lookup, Err(PullRequestError::Unavailable));
        assert_eq!(harness.status(PANE).pull_request, Some(pull_request(478)));
        assert!(harness.lookups().is_empty());
    }

    #[test]
    fn a_signed_out_github_cli_clears_pull_requests_and_stops_lookups() {
        let key = local_key("/src/app");
        let mut harness = Harness::signed_in();
        harness.register(PANE, local_interest("/src/app"));
        harness.read(on_branch(&key, "main"), 0);
        let (lookup, _) = harness.lookups().remove(0);
        harness.finish_lookup(lookup, Ok(Some(pull_request(478))));

        harness.advance(100);
        harness.window_activated();
        let (lookup, _) = harness.lookups().remove(0);
        harness.finish_lookup(lookup, Err(PullRequestError::NotLoggedIn));
        assert_eq!(harness.status(PANE).pull_request, None);
        harness.advance(100);
        harness.window_activated();
        assert!(harness.lookups().is_empty());

        let update = harness
            .scheduler
            .set_github_cli(GitHubCliStatus::SignedIn, harness.now);
        harness.record(update);
        assert_eq!(harness.lookups().len(), 1);
    }

    #[test]
    fn turning_off_pull_requests_clears_them_and_stops_lookups() {
        let key = local_key("/src/app");
        let mut harness = Harness::signed_in();
        harness.register(PANE, local_interest("/src/app"));
        harness.read(on_branch(&key, "main"), 0);
        let (lookup, _) = harness.lookups().remove(0);
        harness.finish_lookup(lookup, Ok(Some(pull_request(478))));

        harness.set_toggles(true, false);
        assert_eq!(harness.changed, vec![PANE]);
        assert_eq!(harness.status(PANE).pull_request, None);
        harness.advance(100);
        harness.window_activated();
        assert!(harness.lookups().is_empty());

        harness.set_toggles(true, true);
        assert_eq!(harness.lookups().len(), 1);
    }

    #[test]
    fn detached_and_unborn_heads_are_not_looked_up() {
        let key = local_key("/src/app");
        let mut harness = Harness::signed_in();
        harness.register(PANE, local_interest("/src/app"));
        harness.read(
            ProbeOutcome::Repository(Box::new(probed(
                &key,
                RepositoryHead::Detached("a1b2c3d".into()),
            ))),
            0,
        );
        assert!(harness.lookups().is_empty());
        harness.advance(100);
        harness.command_finished(PANE, 1);
        let mut unborn = probed(&key, RepositoryHead::Unborn("main".into()));
        unborn.operation = Some(RepositoryOperation::Merging);
        harness.read(ProbeOutcome::Repository(Box::new(unborn)), 0);
        assert!(harness.lookups().is_empty());
    }
}
