//! Git facts a SpaceTerm Server read for the directories its Workspaces show.
//!
//! The server reads Repository Status and Worktree inventory on its own machine and publishes them
//! with its live inventory. It retains none of them. This client presents the facts in the views it
//! already has and adds only what stays local: its Git Settings and Pull Requests from its own
//! GitHub CLI.
//!
//! Every path here names the server's files. A path is displayed, or compared with other server
//! values to find the facts for a view; it is never read, opened, or acted on here, even when the
//! server runs on this machine.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use spaceterm_server::hierarchy::{Pane, Workspace};
use spaceterm_server::repository::{ChangeCounting, DirectoryFacts, Snapshot, WorktreeInventory};

use crate::repository_status::{
    ChangeState, Freshness, PullRequest, RemoteMachineKey, RepositoryKey, RepositoryMachine,
    RepositoryRoot, RepositoryStatus, RepositoryStatusPreferences, RepositoryView,
};
use crate::ui::repository_status_store::ServerPullRequestKey;

/// The machine every server repository is keyed by. Its root is an opaque spelling rather than a
/// local path, so no local read or file action can start from it.
const SERVER_MACHINE: &str = "spaceterm-server";

/// The server's latest Git facts, and when this client first saw each directory's current facts.
#[derive(Debug, Default)]
pub(crate) struct SharedRepositories {
    snapshot: Option<Snapshot>,
    /// A status reads this as its update time. It changes only when the facts do, so an unchanged
    /// reading presents an unchanged view.
    seen: HashMap<PathBuf, Instant>,
}

/// What this client adds to the server's facts when it presents them.
pub(crate) struct LocalPresentation<'a> {
    pub(crate) preferences: RepositoryStatusPreferences,
    /// Whether this client is connected to the server. Facts kept from before a disconnection are
    /// presented as last known, because the server may have read newer ones since.
    pub(crate) connected: bool,
    /// The cached Pull Request this client's GitHub CLI found for a server branch.
    pub(crate) pull_request: &'a dyn Fn(&ServerPullRequestKey) -> Option<PullRequest>,
}

impl SharedRepositories {
    /// Accepts the server's latest reading, and reports whether it differs from the one
    /// presented. A new revision can arrive while the organization revision stays the same, and a
    /// restarted server starts a new revision sequence, so the server boot identifies it too.
    pub(crate) fn accept(&mut self, snapshot: Option<&Snapshot>, now: Instant) -> bool {
        let revision = |snapshot: Option<&Snapshot>| {
            snapshot.map(|snapshot| (snapshot.boot, snapshot.revision))
        };
        if revision(self.snapshot.as_ref()) == revision(snapshot) {
            return false;
        }
        let previous = self.snapshot.take();
        let mut seen = HashMap::new();
        for facts in snapshot.iter().flat_map(|snapshot| &snapshot.directories) {
            let Some(repository) = &facts.repository else {
                continue;
            };
            let unchanged = previous
                .as_ref()
                .and_then(|previous| find(previous, &facts.directory))
                .and_then(|previous| previous.repository.as_ref())
                .is_some_and(|previous| previous == repository);
            let at = self
                .seen
                .get(&facts.directory)
                .copied()
                .filter(|_| unchanged)
                .unwrap_or(now);
            seen.insert(facts.directory.clone(), at);
        }
        self.seen = seen;
        self.snapshot = snapshot.cloned();
        true
    }

    /// The facts the server read for one directory it reported.
    pub(crate) fn facts(&self, directory: &Path) -> Option<&DirectoryFacts> {
        find(self.snapshot.as_ref()?, directory)
    }

    /// The owning server resolves pins across all its Workspaces on its own filesystem.
    pub(crate) fn holds_pinned_directory(&self, root: &Path) -> bool {
        self.snapshot
            .as_ref()
            .is_some_and(|snapshot| snapshot.pinned_worktrees.iter().any(|path| path == root))
    }

    pub(crate) fn pin_check_state(&self) -> spaceterm_server::repository::PinCheckState {
        self.snapshot
            .as_ref()
            .map(|snapshot| snapshot.pin_check)
            .unwrap_or_default()
    }

    /// The Worktrees of the repository containing one directory, as git on the server lists them.
    ///
    /// A failed read keeps the directory's last listing, which a removed Worktree would keep
    /// forever. Another directory in the same repository that the server still reads lists its
    /// current Worktrees instead; the first by path is chosen so every reading picks the same one.
    pub(crate) fn worktrees(&self, directory: &Path) -> Option<&WorktreeInventory> {
        let facts = self.facts(directory)?;
        let last = facts.worktrees.as_ref()?;
        if facts.failure.is_none() {
            return Some(last);
        }
        self.snapshot
            .iter()
            .flat_map(|snapshot| &snapshot.directories)
            .filter(|sibling| sibling.failure.is_none())
            .filter_map(|sibling| Some((&sibling.directory, sibling.worktrees.as_ref()?)))
            .filter(|(_, inventory)| inventory.main_root == last.main_root)
            .min_by_key(|(directory, _)| *directory)
            .map_or(Some(last), |(_, current)| Some(current))
    }

    /// The Worktrees of the repository a Workspace's row reads, as git on the server lists them.
    ///
    /// A restarted server keeps no facts for a directory it cannot read, so a row in a removed
    /// Worktree has no listing of its own. The Main Worktree saved with the Tab membership that
    /// contains the row's directory then locates the repository, when the server reads it. The
    /// saved Main Worktree is only where to look: what the server reads there decides the listing,
    /// and a row the server reads outside any repository lists nothing.
    pub(crate) fn workspace_worktrees(&self, workspace: &Workspace) -> Option<&WorktreeInventory> {
        let directory = workspace_directory(workspace)?;
        if let Some(inventory) = self.worktrees(directory) {
            return Some(inventory);
        }
        if self
            .facts(directory)
            .is_some_and(|facts| facts.failure.is_none())
        {
            return None;
        }
        workspace
            .tabs
            .iter()
            .filter_map(|tab| {
                Some((
                    tab.worktree.as_deref()?,
                    tab.worktree_repository.as_deref()?,
                ))
            })
            .filter(|(worktree, _)| directory.starts_with(worktree))
            .max_by_key(|(worktree, _)| worktree.components().count())
            .and_then(|(_, repository)| self.worktrees(repository))
    }

    /// The Pull Request lookups the presented repositories need, one per branch and upstream.
    pub(crate) fn pull_request_keys(&self) -> Vec<ServerPullRequestKey> {
        self.snapshot
            .iter()
            .flat_map(|snapshot| &snapshot.directories)
            .filter_map(|facts| pull_request_key(facts.repository.as_ref()?))
            .collect()
    }

    /// What a Pane, sidebar row, or chrome presents for one server directory.
    ///
    /// A failed read keeps the last facts, dimmed with the failure; facts without a connection are
    /// dimmed as last known. A directory outside any repository, and every directory while
    /// Repository Status is off, presents nothing.
    pub(crate) fn view(&self, directory: &Path, local: &LocalPresentation<'_>) -> RepositoryView {
        if !local.preferences.show_repository_status {
            return RepositoryView::Hidden;
        }
        let Some(facts) = self.facts(directory) else {
            return RepositoryView::Hidden;
        };
        let Some(repository) = &facts.repository else {
            return RepositoryView::Hidden;
        };
        let Some(&read_at) = self.seen.get(directory) else {
            return RepositoryView::Hidden;
        };
        let freshness = if facts.failure.is_some() || !local.connected {
            Freshness::LastKnown { as_of: read_at }
        } else {
            Freshness::Current
        };
        let pull_request = pull_request_key(repository)
            .filter(|_| local.preferences.pull_requests_enabled())
            .and_then(|key| (local.pull_request)(&key));
        RepositoryView::Repository(Arc::new(RepositoryStatus {
            key: RepositoryKey {
                machine: RepositoryMachine::Remote(RemoteMachineKey::new(SERVER_MACHINE)),
                root: RepositoryRoot::Remote(repository.root.to_string_lossy().into()),
            },
            head: repository.head.clone(),
            commit: repository.commit.clone(),
            upstream: repository.upstream.clone(),
            operation: repository.operation,
            changes: match &repository.changes {
                Ok(changes) => ChangeState::Known(changes.clone()),
                Err(_) if repository.counting == Some(ChangeCounting::Pending) => {
                    ChangeState::NotCounted
                }
                Err(_) if repository.counting == Some(ChangeCounting::Counting) => {
                    ChangeState::Counting
                }
                Err(error) => ChangeState::Unavailable(*error),
            },
            freshness,
            read_at,
            pull_request,
            read_failure: facts.failure,
        }))
    }
}

fn find<'a>(snapshot: &'a Snapshot, directory: &Path) -> Option<&'a DirectoryFacts> {
    snapshot
        .directories
        .iter()
        .find(|facts| facts.directory == directory)
}

/// The lookup one repository's branch needs. The server sends only the parsed query; raw remote
/// URLs and Git configuration stay on the server.
fn pull_request_key(
    repository: &spaceterm_server::repository::Repository,
) -> Option<ServerPullRequestKey> {
    Some(ServerPullRequestKey {
        query: repository.pull_request.clone()?,
        upstream: repository
            .upstream
            .as_ref()
            .map(|upstream| Arc::clone(&upstream.name)),
    })
}

/// The protected Repository Source Directory a server Pane reports.
pub(crate) fn pane_directory(pane: &Pane) -> &Path {
    pane.repository_directory()
}

/// The directory a server Workspace's row reads: its Pinned Directory, else its Root Pane's.
pub(crate) fn workspace_directory(workspace: &Workspace) -> Option<&Path> {
    if let Some(pinned) = &workspace.pinned_directory {
        return Some(pinned);
    }
    workspace
        .tabs
        .iter()
        .find(|tab| tab.id == workspace.root_tab)
        .and_then(|tab| tab.panes.iter().find(|pane| pane.id == tab.root_pane))
        .map(pane_directory)
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use spaceterm_server::hierarchy::{Axis, Tab};
    use spaceterm_server::repository::{
        ChangeSummary, ChangeTotal, GitHubRepository, GitToolStatus, PullRequestQuery, Repository,
        RepositoryHead, RepositoryReadError, WorktreeHead, WorktreeRecord,
    };

    use super::*;

    fn repository(branch: &str) -> Repository {
        Repository {
            root: "/srv/app".into(),
            head: RepositoryHead::Branch(branch.into()),
            commit: Some("abc1234".into()),
            upstream: None,
            operation: None,
            counting: None,
            changes: Ok(ChangeSummary {
                total: ChangeTotal::Exact(2),
                modified: 2,
                ..ChangeSummary::default()
            }),
            pull_request: Some(PullRequestQuery {
                repository: GitHubRepository {
                    host: "github.com".into(),
                    owner: "owner".into(),
                    name: "app".into(),
                },
                head_branch: branch.into(),
                head_owner: None,
            }),
        }
    }

    fn snapshot(revision: u64, directories: Vec<DirectoryFacts>) -> Snapshot {
        Snapshot {
            boot: [1; 32],
            revision,
            git: GitToolStatus::Unknown,
            directories,
            pinned_worktrees: Vec::new(),
            pin_check: spaceterm_server::repository::PinCheckState::Ready,
        }
    }

    fn facts(
        repository: Option<Repository>,
        failure: Option<RepositoryReadError>,
    ) -> DirectoryFacts {
        DirectoryFacts {
            directory: "/srv/app/src".into(),
            repository,
            worktrees: None,
            failure,
        }
    }

    fn pull_request(number: u32) -> PullRequest {
        PullRequest {
            number,
            title: "Title".into(),
            draft: false,
            url: "https://github.com/owner/app/pull/1".into(),
            head: "feature".into(),
            base: "main".into(),
        }
    }

    fn present(
        repositories: &SharedRepositories,
        preferences: RepositoryStatusPreferences,
        connected: bool,
    ) -> RepositoryView {
        let lookup = |_: &ServerPullRequestKey| Some(pull_request(7));
        repositories.view(
            Path::new("/srv/app/src"),
            &LocalPresentation {
                preferences,
                connected,
                pull_request: &lookup,
            },
        )
    }

    fn status(view: &RepositoryView) -> &RepositoryStatus {
        match view {
            RepositoryView::Repository(status) => status,
            RepositoryView::Hidden => panic!("the view should present a repository"),
        }
    }

    #[test]
    fn a_new_revision_or_server_boot_should_present_new_facts_and_a_repeat_should_not() {
        let mut repositories = SharedRepositories::default();
        let now = Instant::now();
        let restarted = Snapshot {
            boot: [2; 32],
            ..snapshot(1, vec![facts(Some(repository("feature")), None)])
        };

        assert!(repositories.accept(
            Some(&snapshot(1, vec![facts(Some(repository("main")), None)])),
            now
        ));
        assert!(!repositories.accept(
            Some(&snapshot(1, vec![facts(Some(repository("main")), None)])),
            now
        ));
        // A restarted server's first revision can repeat the former boot's latest one.
        assert!(repositories.accept(Some(&restarted), now));
        let view = present(&repositories, RepositoryStatusPreferences::default(), true);
        assert!(repositories.accept(None, now));

        assert_eq!(
            status(&view).head,
            spaceterm_server::repository::RepositoryHead::Branch("feature".into())
        );
        assert_eq!(
            present(&repositories, RepositoryStatusPreferences::default(), true),
            RepositoryView::Hidden
        );
    }

    #[test]
    fn unchanged_facts_should_keep_their_update_time_and_changed_facts_should_take_a_new_one() {
        let mut repositories = SharedRepositories::default();
        let first = Instant::now();
        let later = first + Duration::from_secs(30);
        repositories.accept(
            Some(&snapshot(1, vec![facts(Some(repository("main")), None)])),
            first,
        );
        repositories.accept(
            Some(&snapshot(2, vec![facts(Some(repository("main")), None)])),
            later,
        );
        let kept = present(&repositories, RepositoryStatusPreferences::default(), true);

        repositories.accept(
            Some(&snapshot(3, vec![facts(Some(repository("feature")), None)])),
            later,
        );
        let changed = present(&repositories, RepositoryStatusPreferences::default(), true);

        assert_eq!(status(&kept).read_at, first);
        assert_eq!(status(&changed).read_at, later);
    }

    #[test]
    fn a_failed_read_or_a_lost_connection_should_present_the_last_facts_as_last_known() {
        let mut repositories = SharedRepositories::default();
        let now = Instant::now();
        repositories.accept(
            Some(&snapshot(
                1,
                vec![facts(
                    Some(repository("main")),
                    Some(RepositoryReadError::TimedOut),
                )],
            )),
            now,
        );
        let failed = present(&repositories, RepositoryStatusPreferences::default(), true);
        repositories.accept(
            Some(&snapshot(2, vec![facts(Some(repository("main")), None)])),
            now,
        );
        let offline = present(&repositories, RepositoryStatusPreferences::default(), false);
        let current = present(&repositories, RepositoryStatusPreferences::default(), true);

        assert_eq!(
            status(&failed).freshness,
            Freshness::LastKnown { as_of: now }
        );
        assert_eq!(
            status(&failed).read_failure,
            Some(RepositoryReadError::TimedOut)
        );
        assert_eq!(
            status(&offline).freshness,
            Freshness::LastKnown { as_of: now }
        );
        assert_eq!(status(&offline).read_failure, None);
        assert_eq!(status(&current).freshness, Freshness::Current);
    }

    #[test]
    fn decoded_count_failure_keeps_fresh_facts_and_client_pull_request_lookup() {
        let now = Instant::now();
        let mut repositories = SharedRepositories::default();
        repositories.accept(
            Some(&snapshot(1, vec![facts(Some(repository("main")), None)])),
            now,
        );
        for error in [
            RepositoryReadError::Unavailable,
            RepositoryReadError::TimedOut,
        ] {
            let mut current = repository("feature");
            current.changes = Err(error);
            current.upstream = Some(spaceterm_server::repository::Upstream {
                name: "origin/main".into(),
                divergence: Some(spaceterm_server::repository::Divergence {
                    ahead: 2,
                    behind: 1,
                }),
            });
            let mut directory = facts(Some(current), None);
            directory.worktrees = Some(WorktreeInventory {
                main_root: "/srv/app".into(),
                current: Some(0),
                worktrees: vec![WorktreeRecord {
                    root: "/srv/app".into(),
                    head: WorktreeHead::Branch("feature".into()),
                    locked: false,
                    missing: false,
                }],
            });
            let revision = if error == RepositoryReadError::Unavailable {
                2
            } else {
                3
            };
            let decoded: Snapshot = serde_json::from_slice(
                &serde_json::to_vec(&snapshot(revision, vec![directory])).unwrap(),
            )
            .unwrap();
            assert!(repositories.accept(Some(&decoded), now));
            let keys = repositories.pull_request_keys();
            assert_eq!(keys[0].query.head_branch.as_ref(), "feature");
            assert_eq!(keys[0].upstream.as_deref(), Some("origin/main"));
            let presented = present(&repositories, RepositoryStatusPreferences::default(), true);
            let status = status(&presented);
            assert_eq!(status.changes, ChangeState::Unavailable(error));
            assert_eq!(status.head, RepositoryHead::Branch("feature".into()));
            assert_eq!(status.freshness, Freshness::Current);
            assert_eq!(status.read_failure, None);
            assert_eq!(status.pull_request.as_ref().unwrap().number, 7);
            assert_eq!(
                repositories
                    .worktrees(Path::new("/srv/app/src"))
                    .unwrap()
                    .worktrees[0]
                    .head,
                WorktreeHead::Branch("feature".into())
            );
        }
    }

    #[test]
    fn decoded_count_progress_keeps_branch_and_last_known_changes_visible() {
        let mut repositories = SharedRepositories::default();
        let now = Instant::now();
        for (revision, counting, changes, expected) in [
            (
                1,
                Some(ChangeCounting::Pending),
                Err(RepositoryReadError::Unavailable),
                ChangeState::NotCounted,
            ),
            (
                2,
                Some(ChangeCounting::Counting),
                Err(RepositoryReadError::Unavailable),
                ChangeState::Counting,
            ),
            (
                3,
                None,
                Ok(ChangeSummary::default()),
                ChangeState::Known(ChangeSummary::default()),
            ),
            (
                4,
                Some(ChangeCounting::Pending),
                Ok(ChangeSummary::default()),
                ChangeState::Known(ChangeSummary::default()),
            ),
            (
                5,
                Some(ChangeCounting::Counting),
                Ok(ChangeSummary::default()),
                ChangeState::Known(ChangeSummary::default()),
            ),
        ] {
            let mut current = repository("feature");
            current.counting = counting;
            current.changes = changes;
            let decoded: Snapshot = serde_json::from_slice(
                &serde_json::to_vec(&snapshot(revision, vec![facts(Some(current), None)])).unwrap(),
            )
            .unwrap();
            repositories.accept(Some(&decoded), now);
            let view = present(&repositories, RepositoryStatusPreferences::default(), true);
            assert_eq!(status(&view).head, RepositoryHead::Branch("feature".into()));
            assert_eq!(status(&view).changes, expected);
            assert_eq!(status(&view).read_failure, None);
            assert_eq!(status(&view).freshness, Freshness::Current);
        }
    }

    #[test]
    fn client_settings_should_decide_what_server_facts_present() {
        let mut repositories = SharedRepositories::default();
        repositories.accept(
            Some(&snapshot(1, vec![facts(Some(repository("main")), None)])),
            Instant::now(),
        );

        let hidden = present(
            &repositories,
            RepositoryStatusPreferences {
                show_repository_status: false,
                show_pull_requests: true,
            },
            true,
        );
        let without_pull_requests = present(
            &repositories,
            RepositoryStatusPreferences {
                show_repository_status: true,
                show_pull_requests: false,
            },
            true,
        );
        let with_pull_requests =
            present(&repositories, RepositoryStatusPreferences::default(), true);

        assert_eq!(hidden, RepositoryView::Hidden);
        assert_eq!(status(&without_pull_requests).pull_request, None);
        assert_eq!(
            status(&with_pull_requests)
                .pull_request
                .as_ref()
                .map(|pull_request| pull_request.number),
            Some(7)
        );
    }

    #[test]
    fn a_server_root_should_never_present_as_a_local_path() {
        let mut repositories = SharedRepositories::default();
        repositories.accept(
            Some(&snapshot(1, vec![facts(Some(repository("main")), None)])),
            Instant::now(),
        );

        let view = present(&repositories, RepositoryStatusPreferences::default(), true);

        assert!(matches!(status(&view).key.root, RepositoryRoot::Remote(_)));
        assert_eq!(status(&view).key.root.display(), "/srv/app");
    }

    fn listing(directory: &str, main_root: &str, listed: &[&str]) -> DirectoryFacts {
        DirectoryFacts {
            directory: directory.into(),
            worktrees: Some(WorktreeInventory {
                main_root: main_root.into(),
                current: None,
                worktrees: listed
                    .iter()
                    .map(|root| WorktreeRecord {
                        root: root.into(),
                        head: WorktreeHead::Branch("main".into()),
                        locked: false,
                        missing: false,
                    })
                    .collect(),
            }),
            ..facts(Some(repository("main")), None)
        }
    }

    fn listed(repositories: &SharedRepositories, directory: &str) -> Vec<PathBuf> {
        repositories
            .worktrees(Path::new(directory))
            .expect("the directory has a listing")
            .worktrees
            .iter()
            .map(|record| record.root.clone())
            .collect()
    }

    #[test]
    fn a_failed_directory_should_list_the_worktrees_a_surviving_sibling_reads() {
        let failed = DirectoryFacts {
            failure: Some(RepositoryReadError::Unavailable),
            ..listing("/srv/app-gone", "/srv/app", &["/srv/app", "/srv/app-gone"])
        };
        let siblings = [
            listing("/srv/app-b", "/srv/app", &["/srv/app", "/srv/app-b"]),
            listing("/srv/app-a", "/srv/app", &["/srv/app", "/srv/app-a"]),
            listing("/srv/other", "/srv/other", &["/srv/other"]),
        ];
        let mut forward = SharedRepositories::default();
        let mut reversed = SharedRepositories::default();
        let mut alone = SharedRepositories::default();
        let mut directories = vec![failed.clone()];
        directories.extend(siblings.iter().cloned());
        forward.accept(Some(&snapshot(1, directories.clone())), Instant::now());
        directories.reverse();
        reversed.accept(Some(&snapshot(1, directories)), Instant::now());
        alone.accept(
            Some(&snapshot(1, vec![failed, siblings[2].clone()])),
            Instant::now(),
        );

        let chosen = [PathBuf::from("/srv/app"), PathBuf::from("/srv/app-a")];
        assert_eq!(listed(&forward, "/srv/app-gone"), chosen);
        assert_eq!(listed(&reversed, "/srv/app-gone"), chosen);
        assert_eq!(
            listed(&alone, "/srv/app-gone"),
            [PathBuf::from("/srv/app"), PathBuf::from("/srv/app-gone")],
            "with no surviving sibling, the last listing stays"
        );
    }

    /// A Workspace whose Root Tab is in `/srv/app-gone`, saved with the Main Worktree `/srv/app`,
    /// beside a Tab in another repository.
    fn workspace_in_removed_worktree() -> Workspace {
        let tab = |id: u8, worktree: &str, repository: &str| Tab {
            worktree: Some(worktree.into()),
            worktree_membership: spaceterm_server::hierarchy::WorktreeMembership::Following,
            worktree_repository: Some(repository.into()),
            id: [id; 32],
            root_pane: [id; 32],
            panes: vec![Pane {
                id: [id; 32],
                source: None,
                axis: Axis::Right,
                starting_directory: worktree.into(),
                current_directory: None,
                repository_directory: None,
                presentation: None,
                confirmed_directory: None,
                creator: None,
            }],
        };
        Workspace {
            id: [9; 32],
            name: "Build".to_owned(),
            name_version: 0,
            pin_version: 0,
            pinned_directory: None,
            root_tab: [1; 32],
            tabs: vec![
                tab(1, "/srv/app-gone", "/srv/app"),
                tab(2, "/srv/other-feature", "/srv/other"),
            ],
        }
    }

    #[test]
    fn server_repository_views_follow_the_protected_source_independently_of_live_display() {
        let mut workspace = workspace_in_removed_worktree();
        let pane = &mut workspace.tabs[0].panes[0];
        pane.current_directory = Some("/srv/nested".into());
        pane.repository_directory = Some("/srv/app/src".into());
        pane.confirmed_directory = Some("/srv/restore".into());
        // Consume the hierarchy as an observer receives it over SpaceTerm Protocol.
        let mut workspace: Workspace =
            serde_json::from_slice(&serde_json::to_vec(&workspace).unwrap()).unwrap();
        let mut repositories = SharedRepositories::default();
        let mut source = listing("/srv/app/src", "/srv/app", &["/srv/app", "/srv/feature"]);
        source.repository = Some(repository("owner"));
        let mut nested = listing("/srv/nested", "/srv/nested", &["/srv/nested"]);
        nested.repository = Some(repository("nested"));
        repositories.accept(Some(&snapshot(1, vec![source, nested])), Instant::now());
        let presentation = LocalPresentation {
            preferences: RepositoryStatusPreferences::default(),
            connected: true,
            pull_request: &|_| None,
        };
        let view = repositories.view(pane_directory(&workspace.tabs[0].panes[0]), &presentation);
        assert_eq!(status(&view).head, RepositoryHead::Branch("owner".into()));
        assert_eq!(
            repositories
                .workspace_worktrees(&workspace)
                .unwrap()
                .main_root,
            Path::new("/srv/app")
        );
        assert_eq!(
            workspace.tabs[0].panes[0].presented_directory(),
            Path::new("/srv/nested")
        );
        assert_eq!(
            workspace.tabs[0].panes[0].confirmed_directory.as_deref(),
            Some(Path::new("/srv/restore"))
        );
        // The owning server's next protected source update moves both repository views.
        workspace.tabs[0].panes[0].repository_directory = Some("/srv/nested".into());
        let view = repositories.view(pane_directory(&workspace.tabs[0].panes[0]), &presentation);
        assert_eq!(status(&view).head, RepositoryHead::Branch("nested".into()));
        assert_eq!(
            repositories
                .workspace_worktrees(&workspace)
                .unwrap()
                .main_root,
            Path::new("/srv/nested")
        );
        workspace.pinned_directory = Some("/srv/app/src".into());
        assert_eq!(
            repositories
                .workspace_worktrees(&workspace)
                .unwrap()
                .main_root,
            Path::new("/srv/app")
        );
    }

    fn workspace_listing(repositories: &SharedRepositories) -> Option<Vec<PathBuf>> {
        let inventory = repositories.workspace_worktrees(&workspace_in_removed_worktree())?;
        Some(
            inventory
                .worktrees
                .iter()
                .map(|record| record.root.clone())
                .collect(),
        )
    }

    #[test]
    fn a_workspace_in_a_removed_worktree_should_list_what_its_saved_main_worktree_reads() {
        let unreadable = DirectoryFacts {
            directory: "/srv/app-gone".into(),
            repository: None,
            worktrees: None,
            failure: Some(RepositoryReadError::Unavailable),
        };
        let outside = DirectoryFacts {
            failure: None,
            ..unreadable.clone()
        };
        let main = listing("/srv/app", "/srv/app", &["/srv/app", "/srv/app-extra"]);
        let other = listing("/srv/other", "/srv/other", &["/srv/other"]);
        let mut restarted = SharedRepositories::default();
        let mut outside_git = SharedRepositories::default();
        let mut main_unread = SharedRepositories::default();
        restarted.accept(
            Some(&snapshot(
                1,
                vec![unreadable.clone(), other.clone(), main.clone()],
            )),
            Instant::now(),
        );
        outside_git.accept(
            Some(&snapshot(1, vec![outside, other.clone(), main])),
            Instant::now(),
        );
        main_unread.accept(
            Some(&snapshot(
                1,
                vec![
                    unreadable.clone(),
                    other,
                    DirectoryFacts {
                        directory: "/srv/app".into(),
                        ..unreadable
                    },
                ],
            )),
            Instant::now(),
        );

        assert_eq!(
            workspace_listing(&restarted),
            Some(vec![
                PathBuf::from("/srv/app"),
                PathBuf::from("/srv/app-extra")
            ])
        );
        assert_eq!(
            workspace_listing(&outside_git),
            None,
            "a directory the server reads outside Git stays outside"
        );
        assert_eq!(
            workspace_listing(&main_unread),
            None,
            "a saved Main Worktree the server cannot read lists nothing"
        );
    }

    #[test]
    fn a_directory_outside_a_repository_or_unreported_should_present_nothing() {
        let mut repositories = SharedRepositories::default();
        repositories.accept(
            Some(&snapshot(
                1,
                vec![facts(None, Some(RepositoryReadError::Unavailable))],
            )),
            Instant::now(),
        );

        assert_eq!(
            present(&repositories, RepositoryStatusPreferences::default(), true),
            RepositoryView::Hidden
        );
        assert_eq!(
            repositories.view(
                Path::new("/elsewhere"),
                &LocalPresentation {
                    preferences: RepositoryStatusPreferences::default(),
                    connected: true,
                    pull_request: &|_| None,
                },
            ),
            RepositoryView::Hidden
        );
    }
}
