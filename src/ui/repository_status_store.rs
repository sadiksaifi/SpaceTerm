//! App-scoped Repository Status state shared by every window.
//!
//! The store owns the one [`RepositoryScheduler`], runs its effects on dedicated threads, and tells
//! Panes and sidebar rows when their view changed. Programs run only through the injected
//! adapters.

use std::collections::HashMap;
use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Instant;

use gpui::{App, AppContext as _, Context, Entity, EventEmitter, Global, Task};

use crate::domain::RemoteDirectory;
use crate::repository_status::local_read::LocalRepositoryReader;
use crate::repository_status::pull_request::pull_request_query;
use crate::repository_status::pull_request_lookup::GitHubCliLookup;
use crate::repository_status::remote_read::{remote_change_summary, remote_probe_outcome};
use crate::repository_status::remote_url::{GitHubHost, github_host};
use crate::repository_status::scheduler::{
    Interest, InterestId, RepositoryEffect, RepositoryScheduler, SchedulerUpdate, SourceDirectory,
};
use crate::repository_status::tool_check::check_tools;
use crate::repository_status::{
    ChangeSummary, GitHubCliStatus, GitToolStatus, ProbeOutcome, PullRequest, PullRequestError,
    RemoteMachineKey, RemoteRepositoryReader, RepositoryKey, RepositoryMachine,
    RepositoryMarkerReader, RepositoryProgramRunner, RepositoryReadError, RepositoryRoot,
    RepositoryStatusPreferences, RepositoryToolDiscovery, RepositoryView, RepositoryWatch,
    RepositoryWatcher,
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

fn thread_spawner() -> ReadSpawner {
    Arc::new(|work| {
        std::thread::Builder::new()
            .name("repository-status".into())
            .spawn(work)
            .is_ok()
    })
}

/// Remote repository readers by machine. Each Control Connection lends its reader for as long as
/// it holds the lease; any lent reader serves its machine.
#[derive(Clone, Default)]
pub(crate) struct RemoteRepositoryReaders(Arc<Mutex<ReaderTable>>);

#[derive(Default)]
struct ReaderTable {
    next_owner: u64,
    readers: HashMap<RemoteMachineKey, Vec<(u64, Arc<dyn RemoteRepositoryReader>)>>,
}

impl RemoteRepositoryReaders {
    pub(crate) fn lend(
        &self,
        machine: RemoteMachineKey,
        reader: Arc<dyn RemoteRepositoryReader>,
    ) -> RemoteReaderLease {
        let mut table = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        table.next_owner += 1;
        let owner = table.next_owner;
        table
            .readers
            .entry(machine.clone())
            .or_default()
            .push((owner, reader));
        RemoteReaderLease {
            readers: self.clone(),
            machine,
            owner,
        }
    }

    fn reader(&self, machine: &RemoteMachineKey) -> Option<Arc<dyn RemoteRepositoryReader>> {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .readers
            .get(machine)
            .and_then(|readers| readers.first())
            .map(|(_, reader)| Arc::clone(reader))
    }
}

/// Withdraws a lent reader when dropped.
pub(crate) struct RemoteReaderLease {
    readers: RemoteRepositoryReaders,
    machine: RemoteMachineKey,
    owner: u64,
}

impl Drop for RemoteReaderLease {
    fn drop(&mut self) {
        let mut table = self
            .readers
            .0
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if let Some(readers) = table.readers.get_mut(&self.machine) {
            readers.retain(|(owner, _)| *owner != self.owner);
            if readers.is_empty() {
                table.readers.remove(&self.machine);
            }
        }
    }
}

/// The Panes and sidebar rows whose [`RepositoryStatusStore::view`] changed.
#[derive(Clone, Debug)]
pub(crate) struct RepositoryViewsChanged(pub(crate) Arc<[InterestId]>);

impl RepositoryViewsChanged {
    pub(crate) fn contains(&self, id: InterestId) -> bool {
        self.0.binary_search(&id).is_ok()
    }
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
            settings.snapshot().candidate.git,
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
            let preferences = followed.snapshot().candidate.git;
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
    remote_readers: RemoteRepositoryReaders,
    watches: HashMap<RepositoryKey, Box<dyn RepositoryWatch>>,
    watch_events: async_channel::Sender<RepositoryKey>,
    /// Whether each unverified host is a GitHub host, asked once per app session.
    github_hosts: Arc<Mutex<HashMap<Arc<str>, bool>>>,
    wake: Option<(Instant, Task<()>)>,
    next_interest: u64,
    cancellation: SshCancellationToken,
    _watch_task: Task<()>,
}

impl EventEmitter<RepositoryViewsChanged> for RepositoryStatusStore {}

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
            remote_readers: RemoteRepositoryReaders::default(),
            watches: HashMap::new(),
            watch_events,
            github_hosts: Arc::default(),
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
    }

    /// The readers Control Connections lend to Repository Status.
    pub(crate) fn remote_readers(&self) -> RemoteRepositoryReaders {
        self.remote_readers.clone()
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
                    let cancellation = self.cancellation.clone();
                    self.read(
                        move || reader.probe(&directory, &cancellation),
                        move |store, result, cx| {
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
                (RepositoryMachine::Remote(machine), SourceDirectory::Remote(directory)) => {
                    let reader = self.remote_reader(&machine);
                    let cancellation = self.cancellation.clone();
                    self.read(
                        move || remote_probe(reader, &directory, &cancellation),
                        move |store, result, cx| {
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
                            let cancellation = self.cancellation.clone();
                            Box::new(move || {
                                reader
                                    .count(&root, fsmonitor, &cancellation)
                                    .map(|summary| summary.changes)
                            })
                        }
                        (RepositoryMachine::Remote(machine), RepositoryRoot::Remote(root)) => {
                            let reader = self.remote_reader(machine);
                            let root = Arc::clone(root);
                            let cancellation = self.cancellation.clone();
                            Box::new(move || {
                                let reader =
                                    reader.ok_or(RepositoryReadError::ConnectionUnavailable)?;
                                remote_change_summary(&reader.count(
                                    &root,
                                    fsmonitor,
                                    &cancellation,
                                )?)
                            })
                        }
                        _ => Box::new(|| Err(RepositoryReadError::InvalidResponse)),
                    };
                self.read(
                    work,
                    move |store, result, cx| {
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
                        if !is_github_host(&lookup, &hosts, &query.repository.host, &cancellation)?
                        {
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
            RepositoryEffect::WakeAt(due) => self.wake_at(due, cx),
        }
    }

    fn pull_request_finished(
        &mut self,
        ticket: crate::repository_status::scheduler::PullRequestTicket,
        result: Result<Option<PullRequest>, PullRequestError>,
        cx: &mut Context<Self>,
    ) {
        let github_cli = match result {
            Err(PullRequestError::NotLoggedIn) => Some(GitHubCliStatus::SignedOut),
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

    fn remote_reader(&self, machine: &RemoteMachineKey) -> Option<Arc<dyn RemoteRepositoryReader>> {
        self.remote_readers.reader(machine)
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

fn remote_probe(
    reader: Option<Arc<dyn RemoteRepositoryReader>>,
    directory: &str,
    cancellation: &SshCancellationToken,
) -> Result<ProbeOutcome, RepositoryReadError> {
    let reader = reader.ok_or(RepositoryReadError::ConnectionUnavailable)?;
    let remote = RemoteDirectory::new(directory.to_owned())
        .map_err(|_| RepositoryReadError::InvalidResponse)?;
    remote_probe_outcome(reader.probe(&remote, cancellation)?, directory)
}

fn is_github_host(
    lookup: &GitHubCliLookup,
    hosts: &Mutex<HashMap<Arc<str>, bool>>,
    host: &Arc<str>,
    cancellation: &SshCancellationToken,
) -> Result<bool, PullRequestError> {
    if github_host(host) == GitHubHost::Known {
        return Ok(true);
    }
    if let Some(known) = hosts
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .get(host)
    {
        return Ok(*known);
    }
    let known = lookup.is_github_host(host, Instant::now(), cancellation)?;
    hosts
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .insert(Arc::clone(host), known);
    Ok(known)
}
