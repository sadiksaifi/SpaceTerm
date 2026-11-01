//! App-scoped Repository Status state shared by every window.
//!
//! The store checks the programs Repository Status runs, and looks up the Pull Requests for the
//! branches SpaceTerm Servers read with this client's GitHub CLI. Programs run only through the
//! injected adapters, on dedicated threads.

use std::collections::HashMap;
use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use gpui::{App, AppContext as _, Context, Entity, EventEmitter, Global};

use crate::cancellation::CancellationToken;
use crate::repository_status::pull_request::PullRequestQuery;
use crate::repository_status::pull_request_lookup::{GitHubCliLookup, GitHubHosts};
use crate::repository_status::tool_check::check_github_cli;
use crate::repository_status::{
    GitHubCliStatus, PullRequest, PullRequestError, RepositoryProgramRunner,
    RepositoryStatusPreferences, RepositoryToolDiscovery,
};
use crate::settings::Settings;

/// What SpaceTerm last learned about the programs Repository Status runs.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct RepositoryTools {
    pub(crate) github_cli: GitHubCliStatus,
}

impl Global for RepositoryTools {}

/// The native capabilities Repository Status checks its programs and looks up Pull Requests with,
/// chosen by host composition.
#[derive(Clone)]
pub(crate) struct RepositoryStatusAdapters {
    pub(crate) runner: Arc<dyn RepositoryProgramRunner>,
    pub(crate) discovery: Arc<dyn RepositoryToolDiscovery>,
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
    github_cli: Option<Arc<GitHubCliLookup>>,
}

pub(crate) struct RepositoryStatusStore {
    preferences: RepositoryStatusPreferences,
    adapters: RepositoryStatusAdapters,
    spawn: ReadSpawner,
    tools: ToolState,
    github_hosts: Arc<GitHubHosts>,
    /// Pull Requests for branches SpaceTerm Servers read. Only the parsed query comes from the
    /// server; the GitHub CLI, its login, and these answers stay on this client.
    server_pull_requests: HashMap<ServerPullRequestKey, ServerPullRequest>,
    /// The GitHub CLI status the last tool check found, which server lookups follow.
    server_github_cli: GitHubCliStatus,
    cancellation: CancellationToken,
}

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
        let mut store = Self {
            preferences,
            adapters,
            spawn,
            tools: ToolState::default(),
            github_hosts: Arc::default(),
            server_pull_requests: HashMap::new(),
            server_github_cli: GitHubCliStatus::Unknown,
            cancellation: CancellationToken::default(),
        };
        if preferences.show_repository_status {
            store.check_tools(cx);
        }
        store
    }

    /// A window became active: look up server Pull Requests again, and look for tools that were
    /// missing or unanswered.
    pub(crate) fn window_activated(&mut self, cx: &mut Context<Self>) {
        if !self.preferences.show_repository_status {
            return;
        }
        let tools = cx
            .try_global::<RepositoryTools>()
            .copied()
            .unwrap_or_default();
        let github_cli_missing = self.preferences.pull_requests_enabled()
            && tools.github_cli != GitHubCliStatus::SignedIn;
        if self.tools.checked && github_cli_missing {
            self.check_tools(cx);
        }
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
                let github_cli = check_github_cli(
                    adapters.runner.as_ref(),
                    inventory.github_cli.as_deref(),
                    &adapters.physical_home,
                    &adapters.github_cli_environment,
                    Instant::now() + TOOL_CHECK_TIMEOUT,
                    &cancellation,
                );
                (inventory, github_cli)
            },
            |store, result, cx| {
                let (inventory, github_cli) = result.unwrap_or_default();
                store.tools_checked(inventory, github_cli, cx);
            },
            cx,
        );
    }

    fn tools_checked(
        &mut self,
        inventory: crate::repository_status::ToolInventory,
        github_cli: GitHubCliStatus,
        cx: &mut Context<Self>,
    ) {
        self.tools.checking = false;
        self.tools.checked = true;
        self.tools.github_cli = inventory.github_cli.map(|executable| {
            Arc::new(GitHubCliLookup::new(
                Arc::clone(&self.adapters.runner),
                executable,
                self.adapters.physical_home.clone(),
                self.adapters.github_cli_environment.clone(),
            ))
        });
        cx.set_global(RepositoryTools { github_cli });
        // Server branches waiting for the first check, or for a login, are looked up now. A
        // check that changes nothing looks nothing up again.
        let was_enabled = self.server_pull_requests_enabled(self.server_github_cli);
        self.server_github_cli = github_cli;
        match (was_enabled, self.server_pull_requests_enabled(github_cli)) {
            (false, true) => self.lookup_server_pull_requests(cx),
            (true, false) => cx.emit(ServerPullRequestsChanged),
            _ => {}
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

const TOOL_CHECK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

#[cfg(test)]
mod server_pull_request_tests {
    use std::sync::Mutex;

    use gpui::TestAppContext;

    use super::*;
    use crate::repository_status::{
        GitHubRepository, ProgramError, ProgramExit, ProgramRequest, ToolInventory,
    };

    /// A local GitHub CLI that is signed in and lists one Pull Request for any branch.
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
            _: &CancellationToken,
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
                git: Some("/usr/local/bin/git".into()),
                github_cli: Some("/usr/local/bin/gh".into()),
            }
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
