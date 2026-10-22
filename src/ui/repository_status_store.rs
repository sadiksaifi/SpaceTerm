//! App-scoped Repository Status state shared by every window.
//!
//! The store owns the one [`RepositoryScheduler`], runs its effects on dedicated threads, and tells
//! Panes and sidebar rows when their view changed. Programs run only through the injected
//! adapters.

use std::collections::HashMap;
use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use gpui::{App, AppContext as _, Context, Entity, EventEmitter, Global, Task};

use crate::repository_status::local_read::LocalRepositoryReader;
use crate::repository_status::pull_request::{PullRequestQuery, pull_request_query};
use crate::repository_status::pull_request_lookup::{GitHubCliLookup, GitHubHosts};
use crate::repository_status::scheduler::{
    CountTicket, Interest, InterestId, ProbeTicket, RepositoryEffect, RepositoryScheduler,
    SchedulerUpdate, SourceDirectory,
};
use crate::repository_status::tool_check::check_tools;
use crate::repository_status::{
    ChangeSummary, GitHubCliStatus, GitToolStatus, PullRequest, PullRequestError, RepositoryKey,
    RepositoryMachine, RepositoryMarkerReader, RepositoryProgramRunner, RepositoryReadError,
    RepositoryRoot, RepositoryStatusPreferences, RepositoryToolDiscovery, RepositoryView,
    RepositoryWatch, RepositoryWatcher,
};
use crate::settings::Settings;
use crate::ssh::cancellation::SshCancellationToken;

/// What SpaceTerm last learned about the programs Repository Status runs.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct RepositoryTools {
    pub(crate) git: GitToolStatus,
    pub(crate) github_cli: GitHubCliStatus,
}

impl Global for RepositoryTools {}

/// The native capabilities Repository Status reads local repositories with, chosen by host
/// composition.
#[derive(Clone)]
pub(crate) struct RepositoryStatusAdapters {
    pub(crate) runner: Arc<dyn RepositoryProgramRunner>,
    pub(crate) discovery: Arc<dyn RepositoryToolDiscovery>,
    pub(crate) markers: Arc<dyn RepositoryMarkerReader>,
    pub(crate) watcher: Arc<dyn RepositoryWatcher>,
    /// The account's home with symbolic links resolved.
    pub(crate) physical_home: PathBuf,
    /// Launch environment entries the GitHub CLI may need, such as proxy settings.
    pub(crate) github_cli_environment: Vec<(OsString, OsString)>,
}

/// Runs one blocking read away from the UI thread.
pub(crate) type ReadSpawner = Arc<dyn Fn(Box<dyn FnOnce() + Send>) -> bool + Send + Sync>;

pub(crate) fn thread_spawner() -> ReadSpawner {
    Arc::new(|work| {
        std::thread::Builder::new()
            .name("repository-status".into())
            .spawn(work)
            .is_ok()
    })
}

/// The Panes and sidebar rows whose [`RepositoryStatusStore::view`] changed.
#[derive(Clone, Debug)]
pub(crate) struct RepositoryViewsChanged(pub(crate) Arc<[InterestId]>);

impl RepositoryViewsChanged {
    pub(crate) fn contains(&self, id: InterestId) -> bool {
        self.0.binary_search(&id).is_ok()
    }
}

/// The Pull Requests a SpaceTerm Server's repositories have whose value changed.
#[derive(Clone, Copy, Debug)]
pub(crate) struct ServerPullRequestsChanged;

/// One Pull Request lookup for a branch a SpaceTerm Server reads: the parsed query the server
/// sent, and the upstream it tracks. A new branch or upstream is a new lookup.
#[derive(Clone, Eq, Hash, PartialEq)]
pub(crate) struct ServerPullRequestKey {
    pub(crate) query: PullRequestQuery,
    pub(crate) upstream: Option<Arc<str>>,
}

impl std::fmt::Debug for ServerPullRequestKey {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("ServerPullRequestKey(<redacted>)")
    }
}

/// The last answer for one server branch, and whether a lookup for it runs.
#[derive(Default)]
struct ServerPullRequest {
    value: Option<PullRequest>,
    in_flight: bool,
    /// The window activated while the lookup ran, so it looks up once more when it finishes.
    again: bool,
}

/// The installed store, absent when the host composes no Repository Status adapters.
#[derive(Clone)]
pub(crate) struct InstalledRepositoryStatus(pub(crate) Entity<RepositoryStatusStore>);

impl Global for InstalledRepositoryStatus {}

impl InstalledRepositoryStatus {
    pub(crate) fn store(cx: &App) -> Option<Entity<RepositoryStatusStore>> {
        cx.try_global::<Self>().map(|installed| installed.0.clone())
    }
}

/// Installs the store and follows the Git Settings.
pub(crate) fn install(settings: &Settings, adapters: RepositoryStatusAdapters, cx: &mut App) {
    let store = cx.new(|cx| {
        RepositoryStatusStore::new(
            settings.snapshot().candidate.git.repository_status(),
            adapters,
            thread_spawner(),
            cx,
        )
    });
    let changed = settings.subscribe();
    let followed = settings.clone();
    let weak = store.downgrade();
    cx.spawn(async move |cx| {
        while changed.recv().await.is_ok() {
            while changed.try_recv().is_ok() {}
            let preferences = followed.snapshot().candidate.git.repository_status();
            if weak
                .update(cx, |store, cx| store.set_preferences(preferences, cx))
                .is_err()
            {
                break;
            }
        }
    })
    .detach();
    cx.set_global(InstalledRepositoryStatus(store));
}

#[derive(Default)]
struct ToolState {
    checking: bool,
    checked: bool,
    local: Option<Arc<LocalRepositoryReader>>,
    github_cli: Option<Arc<GitHubCliLookup>>,
    /// Local reads that arrived before the first tool check finished.
    waiting: Vec<RepositoryEffect>,
}

pub(crate) struct RepositoryStatusStore {
    scheduler: RepositoryScheduler,
    preferences: RepositoryStatusPreferences,
    adapters: RepositoryStatusAdapters,
    spawn: ReadSpawner,
    tools: ToolState,
    watches: HashMap<RepositoryKey, Box<dyn RepositoryWatch>>,
    watch_events: async_channel::Sender<RepositoryKey>,
    /// Each count in flight and the token that stops it once nothing presents its repository.
    counts: HashMap<RepositoryKey, (CountTicket, SshCancellationToken)>,
    probes: HashMap<ProbeTicket, SshCancellationToken>,
    github_hosts: Arc<GitHubHosts>,
    /// Pull Requests for branches SpaceTerm Servers read. Only the parsed query comes from the
    /// server; the GitHub CLI, its login, and these answers stay on this client.
    server_pull_requests: HashMap<ServerPullRequestKey, ServerPullRequest>,
    /// The GitHub CLI status the last tool check found, which server lookups follow.
    server_github_cli: GitHubCliStatus,
    wake: Option<(Instant, Task<()>)>,
    next_interest: u64,
    cancellation: SshCancellationToken,
    _watch_task: Task<()>,
}

impl EventEmitter<RepositoryViewsChanged> for RepositoryStatusStore {}
impl EventEmitter<ServerPullRequestsChanged> for RepositoryStatusStore {}

impl Drop for RepositoryStatusStore {
    fn drop(&mut self) {
        self.cancellation.cancel();
    }
}

impl RepositoryStatusStore {
    pub(crate) fn new(
        preferences: RepositoryStatusPreferences,
        adapters: RepositoryStatusAdapters,
        spawn: ReadSpawner,
        cx: &mut Context<Self>,
    ) -> Self {
        let (watch_events, watch_receiver) = async_channel::unbounded::<RepositoryKey>();
        let watch_task = cx.spawn(async move |store, cx| {
            while let Ok(key) = watch_receiver.recv().await {
                if store
                    .update(cx, |store, cx| {
                        let update = store.scheduler.repository_changed(&key, Instant::now());
                        store.apply(update, cx);
                    })
                    .is_err()
                {
                    break;
                }
            }
        });
        let mut store = Self {
            scheduler: RepositoryScheduler::new(preferences, GitHubCliStatus::Unknown),
            preferences,
            adapters,
            spawn,
            tools: ToolState::default(),
            watches: HashMap::new(),
            watch_events,
            counts: HashMap::new(),
            probes: HashMap::new(),
            github_hosts: Arc::default(),
            server_pull_requests: HashMap::new(),
            server_github_cli: GitHubCliStatus::Unknown,
            wake: None,
            next_interest: 0,
            cancellation: SshCancellationToken::default(),
            _watch_task: watch_task,
        };
        if preferences.show_repository_status {
            store.check_tools(cx);
        }
        store
    }

    /// What `id` presents now.
    pub(crate) fn view(&self, id: InterestId) -> RepositoryView {
        self.scheduler.view(id)
    }

    pub(crate) fn register(&mut self, interest: Interest, cx: &mut Context<Self>) -> InterestId {
        self.next_interest += 1;
        let id = InterestId::new(self.next_interest);
        let update = self.scheduler.register(id, interest, Instant::now());
        self.apply(update, cx);
        id
    }

    pub(crate) fn unregister(&mut self, id: InterestId, cx: &mut Context<Self>) {
        let update = self.scheduler.unregister(id);
        self.apply(update, cx);
    }

    pub(crate) fn set_source(
        &mut self,
        id: InterestId,
        directory: SourceDirectory,
        cx: &mut Context<Self>,
    ) {
        let update = self.scheduler.set_source(id, directory, Instant::now());
        self.apply(update, cx);
    }

    pub(crate) fn set_visible(&mut self, id: InterestId, visible: bool, cx: &mut Context<Self>) {
        let update = self.scheduler.set_visible(id, visible, Instant::now());
        self.apply(update, cx);
    }

    pub(crate) fn set_available(
        &mut self,
        id: InterestId,
        available: bool,
        cx: &mut Context<Self>,
    ) {
        let update = self.scheduler.set_available(id, available, Instant::now());
        self.apply(update, cx);
    }

    pub(crate) fn set_stopped(&mut self, id: InterestId, stopped: bool, cx: &mut Context<Self>) {
        let update = self.scheduler.set_stopped(id, stopped, Instant::now());
        self.apply(update, cx);
    }

    pub(crate) fn command_finished(
        &mut self,
        id: InterestId,
        finished_commands: u64,
        cx: &mut Context<Self>,
    ) {
        let update = self
            .scheduler
            .command_finished(id, finished_commands, Instant::now());
        self.apply(update, cx);
    }

    /// A window became active: read visible repositories again, and look for tools that were
    /// missing or unanswered.
    pub(crate) fn window_activated(&mut self, cx: &mut Context<Self>) {
        if !self.preferences.show_repository_status {
            return;
        }
        let tools = cx
            .try_global::<RepositoryTools>()
            .copied()
            .unwrap_or_default();
        let git_missing = !matches!(tools.git, GitToolStatus::Ready(_));
        let github_cli_missing = self.preferences.pull_requests_enabled()
            && tools.github_cli != GitHubCliStatus::SignedIn;
        if self.tools.checked && (git_missing || github_cli_missing) {
            self.check_tools(cx);
        }
        let update = self.scheduler.window_activated(Instant::now());
        self.apply(update, cx);
        self.lookup_server_pull_requests(cx);
    }

    /// The cached Pull Request for a branch a SpaceTerm Server reads. Reading never looks up.
    pub(crate) fn server_pull_request(&self, key: &ServerPullRequestKey) -> Option<PullRequest> {
        if !self.server_pull_requests_enabled(self.server_github_cli) {
            return None;
        }
        self.server_pull_requests
            .get(key)
            .and_then(|entry| entry.value.clone())
    }

    /// Follows the branches SpaceTerm Servers' views present now. A branch seen for the first time
    /// is looked up once with this client's GitHub CLI; one no view presents is forgotten. Lookups
    /// run again only when the window activates, as they do for local repositories.
    pub(crate) fn follow_server_pull_requests(
        &mut self,
        wanted: impl IntoIterator<Item = ServerPullRequestKey>,
        cx: &mut Context<Self>,
    ) {
        let wanted: std::collections::HashSet<ServerPullRequestKey> = wanted.into_iter().collect();
        let before = self.server_pull_requests.len();
        self.server_pull_requests
            .retain(|key, _| wanted.contains(key));
        let forgot = self.server_pull_requests.len() != before;
        for key in wanted {
            if !self.server_pull_requests.contains_key(&key) {
                self.server_pull_requests
                    .insert(key.clone(), ServerPullRequest::default());
                self.lookup_server_pull_request(key, cx);
            }
        }
        if forgot {
            cx.emit(ServerPullRequestsChanged);
        }
    }

    fn server_pull_requests_enabled(&self, github_cli: GitHubCliStatus) -> bool {
        self.preferences.pull_requests_enabled() && github_cli == GitHubCliStatus::SignedIn
    }

    /// Looks up one server branch's Pull Request, or once more after the running lookup.
    fn lookup_server_pull_request(&mut self, key: ServerPullRequestKey, cx: &mut Context<Self>) {
        if !self.preferences.pull_requests_enabled() {
            return;
        }
        // The first lookup waits for the GitHub CLI check, which looks the branch up afterwards.
        if !self.tools.checked {
            self.check_tools(cx);
            return;
        }
        if !self.server_pull_requests_enabled(self.server_github_cli) {
            return;
        }
        let Some(entry) = self.server_pull_requests.get_mut(&key) else {
            return;
        };
        if entry.in_flight {
            entry.again = true;
            return;
        }
        entry.in_flight = true;
        let lookup = self.tools.github_cli.clone();
        let hosts = Arc::clone(&self.github_hosts);
        let cancellation = self.cancellation.clone();
        let query = key.query.clone();
        self.read(
            move || {
                let lookup = lookup.ok_or(PullRequestError::ToolMissing)?;
                if !hosts.is_github_host(&lookup, &query.repository.host, &cancellation)? {
                    return Ok(None);
                }
                lookup.find_pull_request(&query, Instant::now(), &cancellation)
            },
            move |store, result, cx| {
                let result = result.unwrap_or(Err(PullRequestError::Unavailable));
                store.server_pull_request_finished(key, result, cx);
            },
            cx,
        );
    }

    /// A failed lookup keeps the last known Pull Request; a signed-out or missing GitHub CLI
    /// clears it, as it does for local repositories.
    fn server_pull_request_finished(
        &mut self,
        key: ServerPullRequestKey,
        result: Result<Option<PullRequest>, PullRequestError>,
        cx: &mut Context<Self>,
    ) {
        if matches!(result, Err(PullRequestError::NotLoggedIn)) {
            self.check_tools(cx);
        }
        let Some(entry) = self.server_pull_requests.get_mut(&key) else {
            return;
        };
        entry.in_flight = false;
        let again = std::mem::take(&mut entry.again);
        let value = match result {
            Ok(value) => value,
            Err(PullRequestError::NotLoggedIn | PullRequestError::ToolMissing) => None,
            Err(
                PullRequestError::Unavailable
                | PullRequestError::InvalidResponse
                | PullRequestError::Cancelled,
            ) => entry.value.clone(),
        };
        if entry.value != value {
            entry.value = value;
            cx.emit(ServerPullRequestsChanged);
        }
        if again {
            self.lookup_server_pull_request(key, cx);
        }
    }

    /// Looks up every followed server branch again, as when Pull Requests turn on.
    fn lookup_server_pull_requests(&mut self, cx: &mut Context<Self>) {
        let keys: Vec<ServerPullRequestKey> = self.server_pull_requests.keys().cloned().collect();
        for key in keys {
            self.lookup_server_pull_request(key, cx);
        }
    }

    /// The Git Settings this client presents repository facts with, including facts a SpaceTerm
    /// Server read.
    pub(crate) const fn preferences(&self) -> RepositoryStatusPreferences {
        self.preferences
    }

    fn set_preferences(
        &mut self,
        preferences: RepositoryStatusPreferences,
        cx: &mut Context<Self>,
    ) {
        if self.preferences == preferences {
            return;
        }
        let pull_requests_turned_on =
            preferences.pull_requests_enabled() && !self.preferences.pull_requests_enabled();
        self.preferences = preferences;
        if preferences.show_repository_status && (!self.tools.checked || pull_requests_turned_on) {
            self.check_tools(cx);
        }
        let update = self.scheduler.set_toggles(preferences, Instant::now());
        self.apply(update, cx);
        if pull_requests_turned_on {
            self.lookup_server_pull_requests(cx);
        } else if !preferences.pull_requests_enabled() {
            for entry in self.server_pull_requests.values_mut() {
                entry.value = None;
            }
        }
        // Server Workspaces present the toggles too, so their views follow every change.
        cx.emit(ServerPullRequestsChanged);
    }

    fn apply(&mut self, update: SchedulerUpdate, cx: &mut Context<Self>) {
        for effect in update.effects {
            self.run(effect, cx);
        }
        if !update.changed.is_empty() {
            cx.emit(RepositoryViewsChanged(update.changed.into()));
        }
    }

    fn run(&mut self, effect: RepositoryEffect, cx: &mut Context<Self>) {
        match effect {
            RepositoryEffect::Probe {
                ticket,
                machine,
                directory,
            } => match (machine, directory) {
                (RepositoryMachine::Local, SourceDirectory::Local(directory)) => {
                    let Some(reader) = self.local_reader(
                        RepositoryEffect::Probe {
                            ticket: ticket.clone(),
                            machine: RepositoryMachine::Local,
                            directory: SourceDirectory::Local(directory.clone()),
                        },
                        cx,
                    ) else {
                        return;
                    };
                    let cancellation = self.probe_cancellation(&ticket);
                    self.read(
                        move || reader.probe(&directory, &cancellation),
                        move |store, result, cx| {
                            store.probes.remove(&ticket);
                            let result = result.unwrap_or(Err(RepositoryReadError::Unavailable));
                            let update =
                                store
                                    .scheduler
                                    .probe_finished(ticket, result, Instant::now());
                            store.apply(update, cx);
                        },
                        cx,
                    );
                }
                _ => {
                    let update = self.scheduler.probe_finished(
                        ticket,
                        Err(RepositoryReadError::InvalidResponse),
                        Instant::now(),
                    );
                    self.apply(update, cx);
                }
            },
            RepositoryEffect::Count {
                ticket,
                key,
                fsmonitor,
            } => {
                let cancellation = SshCancellationToken::linked(
                    &self.cancellation,
                    &SshCancellationToken::default(),
                );
                let work: Box<dyn FnOnce() -> Result<ChangeSummary, RepositoryReadError> + Send> =
                    match (&key.machine, &key.root) {
                        (RepositoryMachine::Local, RepositoryRoot::Local(root)) => {
                            let Some(reader) = self.local_reader(
                                RepositoryEffect::Count {
                                    ticket: ticket.clone(),
                                    key: key.clone(),
                                    fsmonitor,
                                },
                                cx,
                            ) else {
                                return;
                            };
                            let root = root.clone();
                            let cancellation = cancellation.clone();
                            Box::new(move || {
                                reader
                                    .count(&root, fsmonitor, &cancellation)
                                    .map(|summary| summary.changes)
                            })
                        }
                        _ => Box::new(|| Err(RepositoryReadError::InvalidResponse)),
                    };
                self.counts
                    .insert(key.clone(), (ticket.clone(), cancellation));
                self.read(
                    work,
                    move |store, result, cx| {
                        if store
                            .counts
                            .get(&key)
                            .is_some_and(|(current, _)| *current == ticket)
                        {
                            store.counts.remove(&key);
                        }
                        let result = result.unwrap_or(Err(RepositoryReadError::Unavailable));
                        let update = store
                            .scheduler
                            .count_finished(ticket, result, Instant::now());
                        store.apply(update, cx);
                    },
                    cx,
                );
            }
            RepositoryEffect::LookupPullRequest {
                ticket,
                key: _,
                branch,
                config,
            } => {
                let lookup = self.tools.github_cli.clone();
                let hosts = Arc::clone(&self.github_hosts);
                let cancellation = self.cancellation.clone();
                self.read(
                    move || {
                        let lookup = lookup.ok_or(PullRequestError::ToolMissing)?;
                        let head = crate::repository_status::RepositoryHead::Branch(branch);
                        let Some(query) = pull_request_query(&config, &head) else {
                            return Ok(None);
                        };
                        if !hosts.is_github_host(&lookup, &query.repository.host, &cancellation)? {
                            return Ok(None);
                        }
                        lookup.find_pull_request(&query, Instant::now(), &cancellation)
                    },
                    move |store, result, cx| {
                        let result = result.unwrap_or(Err(PullRequestError::Unavailable));
                        store.pull_request_finished(ticket, result, cx);
                    },
                    cx,
                );
            }
            RepositoryEffect::StartWatch { key, directories } => {
                let events = self.watch_events.clone();
                let changed_key = key.clone();
                let watch = self.adapters.watcher.watch(
                    directories,
                    Box::new(move || {
                        let _ = events.try_send(changed_key.clone());
                    }),
                );
                // A repository that cannot be watched still refreshes on the other triggers.
                if let Ok(watch) = watch {
                    self.watches.insert(key, watch);
                }
            }
            RepositoryEffect::StopWatch { key } => {
                self.watches.remove(&key);
            }
            RepositoryEffect::CancelCount { key } => {
                if let Some((_, cancellation)) = self.counts.remove(&key) {
                    cancellation.cancel();
                }
            }
            RepositoryEffect::CancelProbe { ticket } => {
                if let Some(cancellation) = self.probes.remove(&ticket) {
                    cancellation.cancel();
                }
                self.tools.waiting.retain(|effect| {
                    !matches!(effect, RepositoryEffect::Probe { ticket: waiting, .. } if *waiting == ticket)
                });
            }
            RepositoryEffect::WakeAt(due) => self.wake_at(due, cx),
        }
    }

    fn pull_request_finished(
        &mut self,
        ticket: crate::repository_status::scheduler::PullRequestTicket,
        result: Result<Option<PullRequest>, PullRequestError>,
        cx: &mut Context<Self>,
    ) {
        // A lookup's host may be the only one signed out, so check every host again.
        if matches!(result, Err(PullRequestError::NotLoggedIn)) {
            self.check_tools(cx);
        }
        let github_cli = match result {
            Err(PullRequestError::ToolMissing) => Some(GitHubCliStatus::NotFound),
            _ => None,
        };
        if let Some(github_cli) = github_cli {
            let mut tools = cx
                .try_global::<RepositoryTools>()
                .copied()
                .unwrap_or_default();
            tools.github_cli = github_cli;
            cx.set_global(tools);
        }
        let update = self
            .scheduler
            .pull_request_finished(ticket, result, Instant::now());
        self.apply(update, cx);
    }

    /// A cancellation for one probe, also cancelled with the store.
    fn probe_cancellation(&mut self, ticket: &ProbeTicket) -> SshCancellationToken {
        let cancellation =
            SshCancellationToken::linked(&self.cancellation, &SshCancellationToken::default());
        self.probes.insert(ticket.clone(), cancellation.clone());
        cancellation
    }

    /// The local reader, or `None` after queueing or failing `effect` when git is not ready.
    fn local_reader(
        &mut self,
        effect: RepositoryEffect,
        cx: &mut Context<Self>,
    ) -> Option<Arc<LocalRepositoryReader>> {
        if let Some(reader) = &self.tools.local {
            return Some(Arc::clone(reader));
        }
        if !self.tools.checked {
            self.tools.waiting.push(effect);
            self.check_tools(cx);
            return None;
        }
        let now = Instant::now();
        let update = match effect {
            RepositoryEffect::Probe { ticket, .. } => {
                self.scheduler
                    .probe_finished(ticket, Err(RepositoryReadError::ToolMissing), now)
            }
            RepositoryEffect::Count { ticket, .. } => {
                self.scheduler
                    .count_finished(ticket, Err(RepositoryReadError::ToolMissing), now)
            }
            _ => SchedulerUpdate::default(),
        };
        self.apply(update, cx);
        None
    }

    fn check_tools(&mut self, cx: &mut Context<Self>) {
        if self.tools.checking {
            return;
        }
        self.tools.checking = true;
        let adapters = self.adapters.clone();
        let cancellation = self.cancellation.clone();
        self.read(
            move || {
                let inventory = adapters.discovery.discover();
                let (git, github_cli) = check_tools(
                    adapters.runner.as_ref(),
                    &inventory,
                    &adapters.physical_home,
                    &adapters.github_cli_environment,
                    Instant::now() + TOOL_CHECK_TIMEOUT,
                    &cancellation,
                );
                (inventory, git, github_cli)
            },
            |store, result, cx| {
                let (inventory, git, github_cli) = result.unwrap_or_default();
                store.tools_checked(inventory, git, github_cli, cx);
            },
            cx,
        );
    }

    fn tools_checked(
        &mut self,
        inventory: crate::repository_status::ToolInventory,
        git: GitToolStatus,
        github_cli: GitHubCliStatus,
        cx: &mut Context<Self>,
    ) {
        self.tools.checking = false;
        self.tools.checked = true;
        self.tools.local = match (git, inventory.git) {
            (GitToolStatus::Ready(version), Some(executable)) => {
                Some(Arc::new(LocalRepositoryReader::new(
                    Arc::clone(&self.adapters.runner),
                    Arc::clone(&self.adapters.markers),
                    executable,
                    self.adapters.physical_home.clone(),
                    version,
                )))
            }
            _ => None,
        };
        self.tools.github_cli = inventory.github_cli.map(|executable| {
            Arc::new(GitHubCliLookup::new(
                Arc::clone(&self.adapters.runner),
                executable,
                self.adapters.physical_home.clone(),
                self.adapters.github_cli_environment.clone(),
            ))
        });
        cx.set_global(RepositoryTools { git, github_cli });
        let update = self.scheduler.set_github_cli(github_cli, Instant::now());
        self.apply(update, cx);
        // Server branches waiting for the first check, or for a login, are looked up now. A
        // check that changes nothing looks nothing up again.
        let was_enabled = self.server_pull_requests_enabled(self.server_github_cli);
        self.server_github_cli = github_cli;
        match (was_enabled, self.server_pull_requests_enabled(github_cli)) {
            (false, true) => self.lookup_server_pull_requests(cx),
            (true, false) => cx.emit(ServerPullRequestsChanged),
            _ => {}
        }
        for effect in std::mem::take(&mut self.tools.waiting) {
            self.run(effect, cx);
        }
    }

    fn wake_at(&mut self, due: Instant, cx: &mut Context<Self>) {
        if self
            .wake
            .as_ref()
            .is_some_and(|(pending, _)| *pending <= due)
        {
            return;
        }
        let delay = due.saturating_duration_since(Instant::now());
        let task = cx.spawn(async move |store, cx| {
            cx.background_executor().timer(delay).await;
            let _ = store.update(cx, |store, cx| {
                store.wake = None;
                // The test clock advances timers without moving `Instant`, so never tick early.
                let update = store.scheduler.tick(Instant::now().max(due));
                store.apply(update, cx);
            });
        });
        self.wake = Some((due, task));
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

const TOOL_CHECK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

#[cfg(test)]
mod server_pull_request_tests {
    use std::sync::Mutex;

    use gpui::TestAppContext;

    use super::*;
    use crate::repository_status::{
        GitHubRepository, OperationMarkers, ProgramError, ProgramExit, ProgramRequest,
        RepositoryWatchError, ToolInventory, WatchDirectory,
    };

    /// A local GitHub CLI that is signed in and lists one Pull Request for any branch. Git is
    /// absent, so nothing reads a repository here.
    #[derive(Default)]
    struct LocalGitHubCli {
        requests: Mutex<Vec<ProgramRequest>>,
    }

    impl LocalGitHubCli {
        fn lookups(&self) -> Vec<Vec<String>> {
            self.requests
                .lock()
                .unwrap()
                .iter()
                .map(|request| {
                    request
                        .arguments
                        .iter()
                        .map(|argument| argument.to_string_lossy().into_owned())
                        .collect::<Vec<_>>()
                })
                .filter(|arguments| arguments.first().map(String::as_str) == Some("pr"))
                .collect()
        }
    }

    impl RepositoryProgramRunner for LocalGitHubCli {
        fn run(
            &self,
            request: &ProgramRequest,
            stdout: &mut dyn FnMut(&[u8]),
            _: &SshCancellationToken,
        ) -> Result<ProgramExit, ProgramError> {
            self.requests.lock().unwrap().push(request.clone());
            let first = request
                .arguments
                .first()
                .and_then(|argument| argument.to_str());
            match first {
                Some("auth") => {
                    stdout(br#"{"hosts":{"github.com":[{"state":"success","active":true}]}}"#);
                }
                Some("pr") => stdout(
                    br#"[{"number":7,"title":"Fix","isDraft":false,"url":"https://github.com/owner/app/pull/7","headRefName":"feature","baseRefName":"main","headRepositoryOwner":{"login":"owner"},"isCrossRepository":false}]"#,
                ),
                _ => return Err(ProgramError::NotFound),
            }
            Ok(ProgramExit { code: Some(0) })
        }
    }

    struct Tools;

    impl RepositoryToolDiscovery for Tools {
        fn discover(&self) -> ToolInventory {
            ToolInventory {
                git: None,
                github_cli: Some("/usr/local/bin/gh".into()),
            }
        }
    }

    impl RepositoryMarkerReader for Tools {
        fn read(&self, _: &std::path::Path) -> OperationMarkers {
            OperationMarkers::default()
        }
    }

    impl RepositoryWatcher for Tools {
        fn watch(
            &self,
            _: Vec<WatchDirectory>,
            _: Box<dyn Fn() + Send + Sync>,
        ) -> Result<Box<dyn RepositoryWatch>, RepositoryWatchError> {
            Err(RepositoryWatchError)
        }
    }

    fn key(branch: &str) -> ServerPullRequestKey {
        ServerPullRequestKey {
            query: PullRequestQuery {
                repository: GitHubRepository {
                    host: "github.com".into(),
                    owner: "owner".into(),
                    name: "app".into(),
                },
                head_branch: branch.into(),
                head_owner: None,
            },
            upstream: Some(format!("origin/{branch}").into()),
        }
    }

    fn store(cli: &Arc<LocalGitHubCli>, cx: &mut TestAppContext) -> Entity<RepositoryStatusStore> {
        let runner: Arc<dyn RepositoryProgramRunner> = cli.clone();
        let store = cx.new(|cx| {
            RepositoryStatusStore::new(
                RepositoryStatusPreferences::default(),
                RepositoryStatusAdapters {
                    runner,
                    discovery: Arc::new(Tools),
                    markers: Arc::new(Tools),
                    watcher: Arc::new(Tools),
                    physical_home: "/home/person".into(),
                    github_cli_environment: Vec::new(),
                },
                Arc::new(|work| {
                    work();
                    true
                }),
                cx,
            )
        });
        cx.run_until_parked();
        store
    }

    #[gpui::test]
    fn a_server_branch_should_be_looked_up_once_locally_until_the_window_activates(
        cx: &mut TestAppContext,
    ) {
        let cli = Arc::new(LocalGitHubCli::default());
        let store = store(&cli, cx);

        store.update(cx, |store, cx| {
            store.follow_server_pull_requests([key("feature")], cx);
        });
        cx.run_until_parked();
        store.update(cx, |store, cx| {
            store.follow_server_pull_requests([key("feature")], cx);
        });
        cx.run_until_parked();
        let found = store.read_with(cx, |store, _| store.server_pull_request(&key("feature")));
        let reused = cli.lookups().len();
        store.update(cx, |store, cx| store.window_activated(cx));
        cx.run_until_parked();

        assert_eq!(found.map(|pull_request| pull_request.number), Some(7));
        assert_eq!(
            reused, 1,
            "following the same branch again reuses its answer"
        );
        assert_eq!(
            cli.lookups().len(),
            2,
            "activating the window looks up again"
        );
        assert!(
            cli.lookups()[0].contains(&"--repo=github.com/owner/app".to_owned()),
            "the local GitHub CLI receives only the parsed query"
        );
        let executables: Vec<_> = cli
            .requests
            .lock()
            .unwrap()
            .iter()
            .map(|request| request.executable.clone())
            .collect();
        assert!(
            executables
                .iter()
                .all(|executable| executable == std::path::Path::new("/usr/local/bin/gh")),
            "only this client's GitHub CLI runs"
        );
    }

    #[gpui::test]
    fn a_new_branch_should_be_looked_up_and_one_no_view_presents_should_be_forgotten(
        cx: &mut TestAppContext,
    ) {
        let cli = Arc::new(LocalGitHubCli::default());
        let store = store(&cli, cx);

        store.update(cx, |store, cx| {
            store.follow_server_pull_requests([key("feature")], cx);
        });
        cx.run_until_parked();
        store.update(cx, |store, cx| {
            store.follow_server_pull_requests([key("other")], cx);
        });
        cx.run_until_parked();

        assert_eq!(cli.lookups().len(), 2);
        store.read_with(cx, |store, _| {
            assert_eq!(store.server_pull_request(&key("feature")), None);
        });
    }

    #[gpui::test]
    fn turning_pull_requests_off_should_hide_server_pull_requests_without_a_lookup(
        cx: &mut TestAppContext,
    ) {
        let cli = Arc::new(LocalGitHubCli::default());
        let store = store(&cli, cx);
        store.update(cx, |store, cx| {
            store.follow_server_pull_requests([key("feature")], cx);
        });
        cx.run_until_parked();

        store.update(cx, |store, cx| {
            store.set_preferences(
                RepositoryStatusPreferences {
                    show_repository_status: true,
                    show_pull_requests: false,
                },
                cx,
            );
            store.window_activated(cx);
        });
        cx.run_until_parked();

        assert_eq!(cli.lookups().len(), 1);
        store.read_with(cx, |store, _| {
            assert_eq!(store.server_pull_request(&key("feature")), None);
        });
    }
}
