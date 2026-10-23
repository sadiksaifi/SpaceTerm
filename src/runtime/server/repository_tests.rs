use super::tests::{await_text, command, create_shared, host, shared_inventory, size};
use super::*;
use std::time::Instant;
pub(super) struct GitFixture(pub(super) std::path::PathBuf);
impl GitFixture {
    pub(super) fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "spaceterm-git-{}-{:x}",
            std::process::id(),
            u64::from_ne_bytes(nonce().unwrap()[..8].try_into().unwrap())
        ));
        std::fs::create_dir(&root).unwrap();
        let fixture = Self(std::fs::canonicalize(root).unwrap());
        fixture.git(&["init", "-b", "main"]);
        fixture.git(&[
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "commit",
            "--allow-empty",
            "-m",
            "initial",
        ]);
        fixture
    }
    pub(super) fn git(&self, arguments: &[&str]) {
        let output = std::process::Command::new("git")
            .arg("-C")
            .arg(&self.0)
            .args(arguments)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .output()
            .unwrap();
        assert!(output.status.success(), "fixture git failed");
    }
}
impl Drop for GitFixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
pub(super) fn git_facts(
    host: &mut SharedWorkspaceHost,
    directory: &std::path::Path,
    predicate: impl Fn(&spaceterm_server::repository::DirectoryFacts) -> bool,
) -> spaceterm_server::hierarchy::Inventory {
    let until = Instant::now() + Duration::from_secs(12);
    loop {
        let inventory = shared_inventory(host);
        if inventory.repositories.as_ref().is_some_and(|snapshot| {
            snapshot
                .directories
                .iter()
                .any(|facts| facts.directory == directory && predicate(facts))
        }) {
            return inventory;
        }
        assert!(
            Instant::now() < until,
            "owning server did not publish Git facts"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn repository_sources_follow_owning_prompts_while_osc7_keeps_live_directory_display() {
    use spaceterm_server::hierarchy::State;
    let git = GitFixture::new();
    let tree = git.0.join("live-worktree");
    git.git(&["worktree", "add", "-b", "feature", tree.to_str().unwrap()]);
    git.git(&[
        "remote",
        "add",
        "origin",
        "https://github.com/owner/example.git",
    ]);
    let mut host = SharedWorkspaceHost::new(host());
    host.prepare_initial_organization().unwrap();
    let before = shared_inventory(&mut host);
    let pane = before.workspaces[0].tabs[0].root_pane;
    let Reply::Attached { lease } = host.request(
        [1; 32],
        [1; 32],
        Request::SelectPane {
            pane,
            explicit: true,
            size: size(),
            style: None,
        },
    ) else {
        panic!("Root Pane did not start")
    };
    // The fixture disables automatic shell integration. No prompt marker accompanies OSC 7.
    command(
        &mut host,
        1,
        lease,
        &format!(
            "stty -echo; cd '{}'; printf '\\033]7;file://localhost%s\\007' \"$PWD\"; printf '\\033[2J\\033[HOSC7-LIVE\\n'",
            tree.display(),
        ),
    );
    await_text(&mut host, 1, lease, "OSC7-LIVE");
    let current = git_facts(&mut host, &tree, |facts| {
        facts.repository.is_some() && facts.worktrees.is_some()
    });
    let workspace = &current.workspaces[0];
    assert_eq!(workspace.name, "live-worktree");
    assert_eq!(
        super::repositories::workspace_directory(workspace),
        Some(tree.as_path())
    );
    assert_eq!(workspace.tabs[0].worktree.as_ref(), Some(&tree));
    assert_eq!(workspace.tabs[0].panes[0].confirmed_directory, None);
    assert_eq!(
        current
            .repositories
            .as_ref()
            .unwrap()
            .directories
            .iter()
            .find(|facts| facts.directory == tree)
            .unwrap()
            .repository
            .as_ref()
            .unwrap()
            .pull_request
            .as_ref()
            .unwrap()
            .head_branch
            .as_ref(),
        "feature"
    );
    let mut state = State::empty();
    state.inventory = current;
    let retained = state.envelope(&git.0).unwrap();
    assert!(
        retained.shared.as_ref().unwrap().inventory.workspaces[0].tabs[0].panes[0]
            .current_directory
            .is_none()
    );
    assert!(
        retained.shared.as_ref().unwrap().inventory.workspaces[0].tabs[0].panes[0]
            .presentation
            .is_none()
    );
    let retained_json = String::from_utf8(serde_json::to_vec(&retained).unwrap()).unwrap();
    assert!(!retained_json.contains("current_directory"));
    assert!(!retained_json.contains("repository_directory"));
    let restored = State::from_legacy(
        &serde_json::from_slice(&serde_json::to_vec(&retained).unwrap()).unwrap(),
    );
    assert_eq!(
        restored.inventory.workspaces[0].tabs[0].panes[0].confirmed_directory,
        None
    );
    assert_eq!(
        super::repositories::workspace_directory(&restored.inventory.workspaces[0]),
        Some(
            before.workspaces[0].tabs[0].panes[0]
                .starting_directory
                .as_path()
        )
    );
    // An owning prompt establishes the protected Repository Source Directory.
    command(
        &mut host,
        1,
        lease,
        "printf '\\033]133;A;spaceterm=%s\\007' \"$SPACETERM_PROMPT_OWNER\"; printf '\\033[2J\\033[HOWNED-DIRECTORY\\n'",
    );
    await_text(&mut host, 1, lease, "OWNED-DIRECTORY");
    assert_eq!(
        shared_inventory(&mut host).workspaces[0].tabs[0].panes[0]
            .confirmed_directory
            .as_ref(),
        Some(&tree)
    );
    for (marker, report) in [
        ("COMMAND-OSC7", "\\033]7;file://localhost%s\\007"),
        (
            "NESTED-SSH",
            "\\033]7;file://localhost%s\\007\\033]133;A;spaceterm=nested-ssh\\007",
        ),
        (
            "NESTED-CONTAINER",
            "\\033]133;C\\007\\033]7;file://localhost%s\\007\\033]133;D;0\\007\\033]133;A;spaceterm=container\\007",
        ),
    ] {
        command(
            &mut host,
            1,
            lease,
            &format!(
                "cd '{}'; printf '{report}' \"$PWD\"; printf '\\033[2J\\033[H{marker}\\n'",
                git.0.display()
            ),
        );
        await_text(&mut host, 1, lease, marker);
        let current = shared_inventory(&mut host);
        let workspace = &current.workspaces[0];
        assert_eq!(
            workspace.tabs[0].panes[0].presented_directory(),
            git.0.as_path(),
            "ordinary directory display follows {marker}"
        );
        assert_eq!(
            super::repositories::workspace_directory(workspace),
            Some(tree.as_path()),
            "{marker} must not move the Repository Source Directory"
        );
        assert_eq!(workspace.tabs[0].worktree.as_ref(), Some(&tree));
        assert_eq!(
            workspace.tabs[0].panes[0].confirmed_directory.as_ref(),
            Some(&tree)
        );
    }
    // Only the genuine owning-shell prompt moves repository discovery and Root Tab grouping.
    command(
        &mut host,
        1,
        lease,
        "printf '\\033]7;file://localhost%s\\007\\033]133;A;spaceterm=%s\\007' \"$PWD\" \"$SPACETERM_PROMPT_OWNER\"; printf '\\033[2J\\033[HOWNER-MOVED\\n'",
    );
    await_text(&mut host, 1, lease, "OWNER-MOVED");
    let moved = git_facts(&mut host, &git.0, |facts| facts.worktrees.is_some());
    let workspace = &moved.workspaces[0];
    assert_eq!(
        super::repositories::workspace_directory(workspace),
        Some(git.0.as_path())
    );
    assert_eq!(workspace.tabs[0].worktree.as_ref(), Some(&git.0));
    assert_eq!(
        workspace.tabs[0].panes[0].confirmed_directory.as_ref(),
        Some(&git.0)
    );
    host.shutdown().unwrap();
}

#[test]
fn count_failure_keeps_current_branch_upstream_pull_request_and_worktree_facts() {
    assert_count_failure_keeps_facts(false);
}

#[test]
fn count_timeout_keeps_current_branch_upstream_pull_request_and_worktree_facts() {
    assert_count_failure_keeps_facts(true);
}

fn assert_count_failure_keeps_facts(timeout: bool) {
    use spaceterm_server::repository::{RepositoryHead, RepositoryReadError};
    let git = GitFixture::new();
    std::fs::write(git.0.join(".gitattributes"), "tracked.txt filter=broken\n").unwrap();
    std::fs::write(git.0.join("tracked.txt"), "before\n").unwrap();
    git.git(&["add", "."]);
    git.git(&[
        "-c",
        "user.name=Fixture",
        "-c",
        "user.email=fixture@example.invalid",
        "commit",
        "-m",
        "tracked",
    ]);
    git.git(&[
        "remote",
        "add",
        "origin",
        "https://github.com/owner/example.git",
    ]);
    git.git(&["update-ref", "refs/remotes/origin/main", "HEAD"]);
    git.git(&["config", "branch.main.remote", "origin"]);
    git.git(&["config", "branch.main.merge", "refs/heads/main"]);
    let mut prototype = host();
    prototype.directory = git.0.clone();
    let mut host = SharedWorkspaceHost::new(prototype);
    create_shared(&mut host, 1);
    git_facts(&mut host, &git.0, |facts| {
        facts
            .repository
            .as_ref()
            .is_some_and(|repository| repository.counting.is_none() && repository.changes.is_ok())
    });
    // Fail only the expensive change read after a successful earlier publication.
    let filter = if timeout {
        std::fs::write(
            git.0.join(".git/timeout-filter"),
            "sleep 30\necho finished > .git/filter-finished\ncat\n",
        )
        .unwrap();
        "/bin/sh .git/timeout-filter"
    } else {
        "false"
    };
    git.git(&["config", "filter.broken.clean", filter]);
    git.git(&["config", "filter.broken.required", "true"]);
    git.git(&["branch", "-m", "feature"]);
    std::fs::write(git.0.join("tracked.txt"), "after!\n").unwrap();
    let current = git_facts(&mut host, &git.0, |facts| {
        facts.repository.as_ref().is_some_and(|repository| {
            repository.head == RepositoryHead::Branch("feature".into())
                && repository.counting.is_none()
        }) && facts.worktrees.as_ref().is_some_and(|list| {
            list.worktrees[0].head
                == spaceterm_server::repository::WorktreeHead::Branch("feature".into())
        })
    });
    let facts = current
        .repositories
        .as_ref()
        .unwrap()
        .directories
        .iter()
        .find(|facts| facts.directory == git.0)
        .unwrap();
    assert_eq!(facts.failure, None);
    let repository = facts.repository.as_ref().unwrap();
    assert_eq!(
        repository.changes,
        Err(if timeout {
            RepositoryReadError::TimedOut
        } else {
            RepositoryReadError::Unavailable
        })
    );
    assert_eq!(
        repository.upstream.as_ref().unwrap().name.as_ref(),
        "origin/main"
    );
    assert_eq!(
        repository
            .pull_request
            .as_ref()
            .unwrap()
            .head_branch
            .as_ref(),
        "feature"
    );
    assert_eq!(
        facts.worktrees.as_ref().unwrap().worktrees[0].head,
        spaceterm_server::repository::WorktreeHead::Branch("feature".into())
    );
    host.shutdown().unwrap();
    assert!(!git.0.join(".git/filter-finished").exists());
}
#[test]
fn shared_git_reads_publish_external_changes_to_two_clients_and_retain_worktree_membership() {
    use spaceterm_server::hierarchy::Mutation;
    use spaceterm_server::repository::{ChangeTotal, RepositoryHead};
    let git = GitFixture::new();
    let tree = git.0.join("linked");
    git.git(&["worktree", "add", "-b", "feature", tree.to_str().unwrap()]);
    let mut prototype = host();
    prototype.directory = tree.clone();
    let mut host = SharedWorkspaceHost::new(prototype);
    let workspace = create_shared(&mut host, 1);
    let first = git_facts(&mut host, &tree, |facts| {
        facts.repository.as_ref().is_some_and(|repo| {
            repo.head == RepositoryHead::Branch("feature".into()) && repo.counting.is_none()
        }) && facts.worktrees.is_some()
    });
    let tab = &first.workspaces[0].tabs[0];
    assert_eq!(tab.worktree.as_ref(), Some(&tree));
    let Reply::Inventory {
        inventory: second, ..
    } = host.request([3; 32], [3; 32], Request::Inventory)
    else {
        panic!("second client inventory")
    };
    assert_eq!(
        first
            .repositories
            .as_ref()
            .unwrap()
            .directories
            .iter()
            .find(|facts| facts.directory == tree),
        second
            .repositories
            .as_ref()
            .unwrap()
            .directories
            .iter()
            .find(|facts| facts.directory == tree)
    );
    // Creating another Workspace does not move or stop the first Workspace's Git reads.
    let other = create_shared(&mut host, 2);
    assert!(matches!(
        host.request(
            [1; 32],
            [1; 32],
            Request::Mutate {
                sequence: 3,
                mutation: Mutation::PinWorkspace {
                    workspace: other,
                    version: 0,
                    directory: Some(git.0.clone())
                }
            }
        ),
        Reply::Outcome(_)
    ));
    std::fs::write(tree.join("external"), "changed externally").unwrap();
    let changed = git_facts(&mut host, &tree, |facts| {
        facts.repository.as_ref().is_some_and(|repo| {
            repo.changes
                .as_ref()
                .is_ok_and(|changes| changes.total == ChangeTotal::Exact(1))
        })
    });
    let tab_id = changed
        .workspaces
        .iter()
        .find(|w| w.id == workspace)
        .unwrap()
        .tabs[0]
        .id;
    // Missing directories remain saved membership and are never recreated by reconciliation.
    std::fs::remove_dir_all(&tree).unwrap();
    let missing = git_facts(&mut host, &git.0, |facts| {
        facts.worktrees.as_ref().is_some_and(|list| {
            list.worktrees
                .iter()
                .any(|record| record.root == tree && record.missing)
        })
    });
    let mut retained = spaceterm_server::hierarchy::State::empty();
    retained.inventory = missing.clone();
    let envelope = retained.envelope(&git.0).unwrap();
    let restored = spaceterm_server::hierarchy::State::from_legacy(&envelope);
    assert!(restored.inventory.repositories.is_none());
    assert_eq!(
        restored
            .inventory
            .workspaces
            .iter()
            .find(|w| w.id == workspace)
            .unwrap()
            .tabs
            .iter()
            .find(|t| t.id == tab_id)
            .unwrap()
            .worktree
            .as_ref(),
        Some(&tree)
    );
    assert!(!tree.exists());
    assert!(missing.workspaces.iter().any(|w| w.id == workspace));
    host.shutdown().unwrap();
}

#[test]
fn a_deleted_sole_worktree_rediscovers_its_repository_after_restart() {
    use spaceterm_server::{hierarchy::State, repository::WorktreeInventory};
    let git = GitFixture::new();
    let tree = git.0.join("linked");
    git.git(&["worktree", "add", "-b", "feature", tree.to_str().unwrap()]);
    let mut prototype = host();
    prototype.directory = tree.clone();
    let mut host = SharedWorkspaceHost::new(prototype);
    create_shared(&mut host, 1);
    let inventory = git_facts(&mut host, &tree, |facts| facts.worktrees.is_some());
    let saved_tab = inventory.workspaces[0].tabs[0].clone();
    let mut state = State::empty();
    state.inventory = inventory;
    // Exercise the actual retained serialization, with no live repository facts at restore.
    let bytes = serde_json::to_vec(&state.envelope(&git.0).unwrap()).unwrap();
    host.shutdown().unwrap();
    std::fs::remove_dir_all(&tree).unwrap();
    let mut restored = State::from_legacy(&serde_json::from_slice(&bytes).unwrap());
    restored.begin_boot([9; 32]);
    assert!(restored.inventory.repositories.is_none());
    let mut repositories = super::repositories::ServerRepositories::new([9; 32]);
    repositories.subscribe([1; 32]);
    let mut wait_for = |predicate: &dyn Fn(&WorktreeInventory) -> bool| {
        let until = Instant::now() + Duration::from_secs(12);
        loop {
            repositories.update(&mut restored);
            if repositories.snapshot().directories.iter().any(|facts| {
                facts.directory == git.0
                    && facts.failure.is_none()
                    && facts.worktrees.as_ref().is_some_and(predicate)
            }) {
                break;
            }
            assert!(
                Instant::now() < until,
                "restored repository was not rediscovered"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    };
    wait_for(&|list| {
        list.worktrees
            .iter()
            .any(|record| record.root == tree && record.missing)
    });
    let extra = git.0.join("external");
    git.git(&["worktree", "add", "-b", "external", extra.to_str().unwrap()]);
    wait_for(&|list| list.worktrees.iter().any(|record| record.root == extra));
    git.git(&["worktree", "prune"]);
    wait_for(&|list| {
        !list.worktrees.iter().any(|record| record.root == tree)
            && list.worktrees.iter().any(|record| record.root == extra)
    });
    assert_eq!(restored.inventory.workspaces.len(), 1);
    let tabs = &restored.inventory.workspaces[0].tabs;
    assert_eq!(tabs.len(), 1);
    assert_eq!(tabs[0].id, saved_tab.id);
    assert_eq!(tabs[0].root_pane, saved_tab.root_pane);
    assert_eq!(tabs[0].worktree.as_ref(), Some(&tree));
    assert_eq!(tabs[0].worktree_repository.as_ref(), Some(&git.0));
    assert!(tabs[0].panes.iter().all(|pane| pane.creator.is_none()));
    assert!(!tree.exists());
    repositories.shutdown();
}

#[test]
fn abandoned_server_git_reads_cancel_on_disconnect_and_stop_without_losing_tabs() {
    for stop in [false, true] {
        let git = GitFixture::new();
        std::fs::write(git.0.join(".gitattributes"), "*.txt filter=slow\n").unwrap();
        std::fs::write(git.0.join("tracked.txt"), "before\n").unwrap();
        git.git(&["add", "."]);
        git.git(&[
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "commit",
            "-m",
            "filter fixture",
        ]);
        let script = git.0.join(".git/slow-filter");
        std::fs::write(
            &script,
            "echo $$ > .git/started\nsleep 30\necho finished > .git/finished\ncat\n",
        )
        .unwrap();
        git.git(&[
            "config",
            "filter.slow.clean",
            &format!("/bin/sh {}", script.display()),
        ]);
        std::fs::write(git.0.join("tracked.txt"), "after!\n").unwrap();
        let mut prototype = host();
        prototype.directory = git.0.clone();
        let mut host = SharedWorkspaceHost::new(prototype);
        let workspace = create_shared(&mut host, 1);
        let until = Instant::now() + Duration::from_secs(8);
        while !git.0.join(".git/started").exists() {
            shared_inventory(&mut host);
            assert!(Instant::now() < until, "filter never started");
            std::thread::sleep(Duration::from_millis(20));
        }
        let started = Instant::now();
        if stop {
            host.shutdown().unwrap();
        } else {
            host.disconnected([2; 32]);
        }
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "abandoned read reached its deadline instead of cancelling"
        );
        let current = shared_inventory(&mut host);
        assert_eq!(current.workspaces[0].id, workspace);
        assert_eq!(current.workspaces[0].tabs.len(), 1);
        assert!(!git.0.join(".git/finished").exists());
        host.shutdown().unwrap();
    }
}

#[test]
fn an_unpinned_root_tab_follows_entering_and_leaving_git_without_moving_other_tabs() {
    use spaceterm_server::hierarchy::Mutation;
    let git = GitFixture::new();
    let outside = GitFixture::new();
    std::fs::remove_dir_all(outside.0.join(".git")).unwrap();
    let mut prototype = host();
    prototype.directory = git.0.clone();
    let mut host = SharedWorkspaceHost::new(prototype);
    let workspace = create_shared(&mut host, 1);
    git_facts(&mut host, &git.0, |facts| facts.worktrees.is_some());
    assert!(matches!(
        host.request(
            [1; 32],
            [1; 32],
            Request::Mutate {
                sequence: 2,
                mutation: Mutation::CreateTab {
                    workspace,
                    source: None
                }
            }
        ),
        Reply::Outcome(_)
    ));
    let initial = shared_inventory(&mut host);
    let root = &initial.workspaces[0].tabs[0];
    let root_id = root.id;
    let other_id = initial.workspaces[0].tabs[1].id;
    assert!(
        initial.workspaces[0]
            .tabs
            .iter()
            .all(|tab| tab.worktree.as_ref() == Some(&git.0))
    );
    let Reply::Attached { lease } = host.request(
        [1; 32],
        [1; 32],
        Request::SelectPane {
            pane: root.root_pane,
            explicit: true,
            size: size(),
            style: None,
        },
    ) else {
        panic!("Root Pane did not start")
    };
    command(
        &mut host,
        1,
        lease,
        &format!(
            "stty -echo; cd '{}'; printf '\\033]7;file://localhost%s\\007\\033]133;A;spaceterm=%s\\007' \"$PWD\" \"$SPACETERM_PROMPT_OWNER\"; printf '\\033[2J\\033[HLEFT-REPOSITORY\\n'",
            outside.0.display()
        ),
    );
    await_text(&mut host, 1, lease, "LEFT-REPOSITORY");
    let left = git_facts(&mut host, &outside.0, |facts| {
        facts.failure.is_none() && facts.repository.is_none()
    });
    let tabs = &left.workspaces[0].tabs;
    assert_eq!(
        tabs.iter().find(|tab| tab.id == root_id).unwrap().worktree,
        None
    );
    assert_eq!(
        tabs.iter()
            .find(|tab| tab.id == root_id)
            .unwrap()
            .worktree_repository,
        None
    );
    assert_eq!(
        tabs.iter()
            .find(|tab| tab.id == other_id)
            .unwrap()
            .worktree
            .as_ref(),
        Some(&git.0)
    );
    assert_eq!(
        tabs.iter()
            .find(|tab| tab.id == other_id)
            .unwrap()
            .worktree_repository
            .as_ref(),
        Some(&git.0)
    );
    command(
        &mut host,
        1,
        lease,
        &format!(
            "cd '{}'; printf '\\033]7;file://localhost%s\\007\\033]133;A;spaceterm=%s\\007' \"$PWD\" \"$SPACETERM_PROMPT_OWNER\"; printf '\\033[2J\\033[HENTERED-REPOSITORY\\n'",
            git.0.display()
        ),
    );
    await_text(&mut host, 1, lease, "ENTERED-REPOSITORY");
    let entered = shared_inventory(&mut host);
    assert!(
        entered.workspaces[0]
            .tabs
            .iter()
            .all(|tab| tab.worktree.as_ref() == Some(&git.0))
    );
    host.shutdown().unwrap();
}

#[test]
fn repairing_an_unpinned_root_preserves_missing_worktree_membership_after_reads_and_restore() {
    use spaceterm_server::hierarchy::State;
    let git = GitFixture::new();
    let tree = git.0.join("repair-linked");
    git.git(&["worktree", "add", "-b", "repair", tree.to_str().unwrap()]);
    let mut prototype = host();
    prototype.directory = tree.clone();
    let mut host = SharedWorkspaceHost::new(prototype);
    create_shared(&mut host, 1);
    let before = git_facts(&mut host, &tree, |facts| facts.worktrees.is_some());
    let original = before.workspaces[0].clone();
    let id = original.tabs[0].root_pane;
    std::fs::remove_dir_all(&tree).unwrap();
    assert!(matches!(
        host.request(
            [1; 32],
            [1; 32],
            Request::SelectPane {
                pane: id,
                explicit: true,
                size: size(),
                style: None
            }
        ),
        Reply::Refused(Refusal::DirectoryUnavailable)
    ));
    let Reply::Restored {
        recovery: Some(target),
        ..
    } = host.request(
        [1; 32],
        [1; 32],
        Request::SelectPane {
            pane: id,
            explicit: false,
            size: size(),
            style: None,
        },
    )
    else {
        panic!("dormant recovery target")
    };
    assert!(matches!(
        host.request(
            [1; 32],
            [1; 32],
            Request::RepairDirectory {
                target,
                replacement: Some(git.0.clone()),
                size: size(),
                style: None
            }
        ),
        Reply::Attached { .. }
    ));
    let after = git_facts(&mut host, &git.0, |facts| facts.worktrees.is_some());
    let mut expected = original.clone();
    expected.tabs[0].panes[0].confirmed_directory = Some(git.0.clone());
    expected.tabs[0].worktree_membership_fixed = true;
    let mut organization = after.workspaces[0].clone();
    // Live terminal presentation is independent of the repaired saved organization.
    organization.tabs[0].panes[0].presentation = None;
    assert_eq!(organization, expected);
    assert!(
        !tree.exists(),
        "repair must never recreate a missing Worktree"
    );
    let mut state = State::empty();
    state.inventory = after;
    let bytes = serde_json::to_vec(&state.envelope(&git.0).unwrap()).unwrap();
    host.shutdown().unwrap();
    let mut restored = State::from_legacy(&serde_json::from_slice(&bytes).unwrap());
    restored.begin_boot([9; 32]);
    let mut reads = super::repositories::ServerRepositories::new([9; 32]);
    reads.subscribe([1; 32]);
    let until = Instant::now() + Duration::from_secs(10);
    while reads.snapshot().directories.is_empty() && Instant::now() < until {
        reads.update(&mut restored);
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(
        restored.inventory.workspaces[0].tabs[0].worktree,
        original.tabs[0].worktree
    );
    assert_eq!(restored.inventory.workspaces[0].root_tab, original.root_tab);
    reads.shutdown();
}

#[test]
fn worktree_write_retries_resolve_the_original_and_never_recreate_removed_state() {
    use spaceterm_server::{
        hierarchy::{Mutation, Outcome},
        worktree::{Branch, BranchName, Operation},
    };
    let git = GitFixture::new();
    let mut prototype = host();
    prototype.directory = git.0.clone();
    let mut host = SharedWorkspaceHost::new(prototype);
    let workspace = create_shared(&mut host, 1);
    let tree = git.0.join("created");
    let mutation = Mutation::Worktree(Operation::Create {
        workspace,
        root: git.0.clone(),
        path: tree.clone(),
        branch: Branch::New {
            name: "created".into(),
            base: BranchName::Local("main".into()),
        },
    });
    let until = Instant::now() + Duration::from_secs(12);
    let result = loop {
        match host.request(
            [1; 32],
            [1; 32],
            Request::Mutate {
                sequence: 2,
                mutation: mutation.clone(),
            },
        ) {
            Reply::MutationPending => {}
            Reply::Outcome(outcome) => break outcome,
            reply => panic!("unexpected reply: {reply:?}"),
        }
        assert!(Instant::now() < until);
        std::thread::sleep(Duration::from_millis(20));
    };
    assert!(
        matches!(result.outcome, Outcome::Accepted { .. }),
        "{result:?}"
    );
    git.git(&["worktree", "remove", tree.to_str().unwrap()]);
    assert!(
        matches!(host.request([1;32], [2;32], Request::Mutate {sequence: 2, mutation}), Reply::Outcome(outcome) if outcome == result)
    );
    assert!(!tree.exists());
    host.shutdown().unwrap();
}

pub(super) fn finish_write(
    host: &mut SharedWorkspaceHost,
    sequence: u64,
    operation: spaceterm_server::worktree::Operation,
) -> spaceterm_server::hierarchy::Outcome {
    let until = Instant::now() + Duration::from_secs(12);
    loop {
        match host.request(
            [1; 32],
            [1; 32],
            Request::Mutate {
                sequence,
                mutation: spaceterm_server::hierarchy::Mutation::Worktree(operation.clone()),
            },
        ) {
            Reply::MutationPending => {}
            Reply::Outcome(result) => return result.outcome,
            other => panic!("unexpected write reply: {other:?}"),
        }
        assert!(Instant::now() < until);
        std::thread::sleep(Duration::from_millis(20));
    }
}
pub(super) fn review_removal(
    host: &mut SharedWorkspaceHost,
    root: &std::path::Path,
    path: &std::path::Path,
) -> spaceterm_server::worktree::Removal {
    match read_worktree(
        host,
        spaceterm_server::worktree::Read::CheckRemoval {
            root: root.to_owned(),
            path: path.to_owned(),
        },
    ) {
        Reply::WorktreeRead(Ok(spaceterm_server::worktree::ReadReply::Removal(review))) => review,
        other => panic!("unexpected removal review: {other:?}"),
    }
}
pub(super) fn read_worktree(
    host: &mut SharedWorkspaceHost,
    mut read: spaceterm_server::worktree::Read,
) -> Reply {
    let until = Instant::now() + Duration::from_secs(30);
    loop {
        match host.request([1; 32], [1; 32], Request::WorktreeRead(read)) {
            Reply::WorktreeRead(Ok(spaceterm_server::worktree::ReadReply::Pending { id })) => {
                assert!(Instant::now() < until, "server Git read did not finish");
                read = spaceterm_server::worktree::Read::Poll { id };
                std::thread::sleep(Duration::from_millis(10));
            }
            reply => return reply,
        }
    }
}
#[test]
fn removal_revalidates_other_workspace_sessions_and_closes_unattached_work_before_git() {
    use spaceterm_server::{
        hierarchy::Outcome,
        worktree::{Error, Operation},
    };
    let git = GitFixture::new();
    let tree = git.0.join("linked");
    git.git(&["worktree", "add", "-b", "linked", tree.to_str().unwrap()]);
    let mut prototype = host();
    prototype.directory = tree.clone();
    let mut host = SharedWorkspaceHost::new(prototype);
    create_shared(&mut host, 1);
    create_shared(&mut host, 2);
    git_facts(&mut host, &tree, |facts| facts.worktrees.is_some());
    let review = review_removal(&mut host, &git.0, &tree);
    assert_eq!(review.tabs.len(), 2);
    let pane = review.tabs[1].panes[0].id;
    let Reply::Attached { lease } = host.request(
        [1; 32],
        [3; 32],
        Request::SelectPane {
            pane,
            explicit: true,
            size: size(),
            style: None,
        },
    ) else {
        panic!("start other client's Pane")
    };
    command(
        &mut host,
        3,
        lease,
        "printf '\\nunattached-work\\n'; sleep 30\n",
    );
    await_text(&mut host, 3, lease, "unattached-work");
    assert!(matches!(
        host.request(
            [3; 32],
            [4; 32],
            Request::TakeOver {
                target: lease.into(),
                size: size(),
                style: None
            }
        ),
        Reply::Attached { .. }
    ));
    let operation = Operation::Remove {
        root: git.0.clone(),
        path: tree.clone(),
        boot: review.boot,
        confirmation: review.confirmation,
    };
    assert_eq!(
        finish_write(&mut host, 3, operation),
        Outcome::WorktreeFailed(Error::Changed)
    );
    assert!(tree.exists());
    let review = review_removal(&mut host, &git.0, &tree);
    assert_eq!(
        review
            .tabs
            .iter()
            .flat_map(|t| &t.panes)
            .filter(|p| p.session.is_some())
            .count(),
        1
    );
    let operation = Operation::Remove {
        root: git.0.clone(),
        path: tree.clone(),
        boot: review.boot,
        confirmation: review.confirmation,
    };
    assert!(matches!(
        finish_write(&mut host, 4, operation),
        Outcome::Accepted { .. }
    ));
    assert!(!tree.exists());
    assert!(
        shared_inventory(&mut host)
            .close
            .unwrap()
            .sessions
            .iter()
            .all(|p| p.session.is_none())
    );
    let inventory = shared_inventory(&mut host);
    assert_eq!(inventory.workspaces.len(), 2);
    assert!(
        inventory
            .workspaces
            .iter()
            .all(|w| w.tabs.len() == 1 && w.tabs[0].worktree.as_ref() == Some(&git.0))
    );
    host.shutdown().unwrap();
}
#[test]
fn replaced_worktree_and_main_worktree_are_preserved_after_confirmation() {
    use spaceterm_server::{
        hierarchy::Outcome,
        worktree::{Error, Operation, Read},
    };
    let git = GitFixture::new();
    let tree = git.0.join("linked");
    git.git(&["worktree", "add", "-b", "linked", tree.to_str().unwrap()]);
    let mut prototype = host();
    prototype.directory = git.0.clone();
    let mut host = SharedWorkspaceHost::new(prototype);
    create_shared(&mut host, 1);
    assert!(matches!(
        read_worktree(
            &mut host,
            Read::CheckRemoval {
                root: git.0.clone(),
                path: git.0.clone()
            }
        ),
        Reply::WorktreeRead(Err(Error::Main))
    ));
    let review = review_removal(&mut host, &git.0, &tree);
    git.git(&["worktree", "remove", tree.to_str().unwrap()]);
    git.git(&["worktree", "add", tree.to_str().unwrap(), "linked"]);
    assert_eq!(
        finish_write(
            &mut host,
            2,
            Operation::Remove {
                root: git.0.clone(),
                path: tree.clone(),
                boot: review.boot,
                confirmation: review.confirmation
            }
        ),
        Outcome::WorktreeFailed(Error::Changed)
    );
    assert!(tree.exists());
    host.shutdown().unwrap();
}

#[test]
fn worktree_location_refuses_an_unwritable_folder_without_changing_settings() {
    use spaceterm_server::{
        hierarchy::Outcome,
        worktree::{Operation, Read, ReadReply, Settings},
    };
    use std::os::unix::fs::{PermissionsExt, symlink};

    let fixture = GitFixture::new();
    let folder = fixture.0.join("read-only");
    std::fs::create_dir(&folder).unwrap();
    let alias = fixture.0.join("alias");
    symlink(&folder, &alias).unwrap();
    std::fs::set_permissions(&folder, std::fs::Permissions::from_mode(0o500)).unwrap();
    let mut host = SharedWorkspaceHost::new(host());
    let result = finish_write(
        &mut host,
        1,
        Operation::SetLocation {
            version: 0,
            template: format!("{}/missing/{{repository}}/{{branch}}", alias.display()),
        },
    );
    std::fs::set_permissions(&folder, std::fs::Permissions::from_mode(0o700)).unwrap();
    assert!(
        matches!(result, Outcome::WorktreeFailed(_)),
        "a read-only ancestor on the owning server must reject the edit: {result:?}"
    );
    assert!(matches!(
        host.request([1; 32], [1; 32], Request::WorktreeRead(Read::Settings)),
        Reply::WorktreeRead(Ok(ReadReply::Settings(saved))) if saved == Settings::default()
    ));
    assert!(!folder.join("missing").exists());
    host.shutdown().unwrap();
}

#[test]
fn worktree_location_refuses_a_file_in_the_folder_prefix() {
    use spaceterm_server::{hierarchy::Outcome, worktree::Operation};
    let fixture = GitFixture::new();
    let file = fixture.0.join("file");
    std::fs::write(&file, b"keep").unwrap();
    let mut host = SharedWorkspaceHost::new(host());
    let result = finish_write(
        &mut host,
        1,
        Operation::SetLocation {
            version: 0,
            template: format!("{}/{{branch}}", file.display()),
        },
    );
    assert!(
        matches!(result, Outcome::WorktreeFailed(_)),
        "a file cannot contain new Worktree folders: {result:?}"
    );
    assert_eq!(std::fs::read(file).unwrap(), b"keep");
    host.shutdown().unwrap();
}

#[test]
fn worktree_location_is_seeded_once_and_validated_by_the_owning_server() {
    use spaceterm_server::{
        hierarchy::Outcome,
        worktree::{Error, Operation, Read, ReadReply},
    };
    let mut host = SharedWorkspaceHost::new(host());
    let read = |host: &mut SharedWorkspaceHost, template: &str| {
        host.request(
            [1; 32],
            [1; 32],
            Request::WorktreeRead(Read::SeedLocation {
                template: template.into(),
            }),
        )
    };
    assert!(
        matches!(read(&mut host,"~/first/{branch}"),Reply::WorktreeRead(Ok(ReadReply::Settings(s))) if s.template=="~/first/{branch}")
    );
    assert!(
        matches!(read(&mut host,"~/later/{branch}"),Reply::WorktreeRead(Ok(ReadReply::Settings(s))) if s.template=="~/first/{branch}")
    );
    assert_eq!(
        finish_write(
            &mut host,
            1,
            Operation::SetLocation {
                version: 0,
                template: "~/missing-placeholder".into()
            }
        ),
        Outcome::WorktreeFailed(Error::Template(
            spaceterm_server::worktree::path_template::WorktreePathTemplateError::MissingBranch
        ))
    );
    assert!(matches!(
        finish_write(
            &mut host,
            2,
            Operation::SetLocation {
                version: 0,
                template: "~/second/{branch}".into()
            }
        ),
        Outcome::Accepted { .. }
    ));
    assert_eq!(
        finish_write(
            &mut host,
            3,
            Operation::SetLocation {
                version: 0,
                template: "~/stale/{branch}".into()
            }
        ),
        Outcome::WorktreeFailed(Error::Changed)
    );
    assert!(
        matches!(host.request([1;32],[1;32],Request::WorktreeRead(Read::Settings)),Reply::WorktreeRead(Ok(ReadReply::Settings(s))) if s.template=="~/second/{branch}" && s.version==1)
    );
    host.shutdown().unwrap();
}

#[test]
fn server_removal_protects_pins_in_unattached_workspaces_and_rechecks_missing_locations() {
    use spaceterm_server::{
        hierarchy::{Mutation, Outcome},
        worktree::{Error, Operation, Read},
    };
    let git = GitFixture::new();
    let tree = git.0.join("linked");
    git.git(&["worktree", "add", "-b", "linked", tree.to_str().unwrap()]);
    let mut prototype = host();
    prototype.directory = git.0.clone();
    let mut host = SharedWorkspaceHost::new(prototype);
    let workspace = create_shared(&mut host, 1);
    let other = create_shared(&mut host, 2);
    let alias = git.0.join("pin-alias");
    std::os::unix::fs::symlink(&tree, &alias).unwrap();
    host.request(
        [1; 32],
        [1; 32],
        Request::Mutate {
            sequence: 3,
            mutation: Mutation::PinWorkspace {
                workspace: other,
                version: 0,
                directory: Some(alias),
            },
        },
    );
    let mut inventory = git_facts(&mut host, &git.0, |facts| facts.worktrees.is_some());
    let until = Instant::now() + Duration::from_secs(3);
    while inventory.repositories.as_ref().unwrap().pin_check
        == spaceterm_server::repository::PinCheckState::Checking
    {
        assert!(
            Instant::now() < until,
            "the owning server must finish physical pin discovery"
        );
        std::thread::sleep(Duration::from_millis(5));
        inventory = shared_inventory(&mut host);
    }
    assert!(
        inventory
            .repositories
            .unwrap()
            .pinned_worktrees
            .contains(&tree),
        "the server publishes physical containment for a pin in another Workspace"
    );
    assert!(matches!(
        read_worktree(
            &mut host,
            Read::CheckRemoval {
                root: git.0.clone(),
                path: tree.clone()
            }
        ),
        Reply::WorktreeRead(Err(Error::Pinned))
    ));
    host.request(
        [1; 32],
        [1; 32],
        Request::Mutate {
            sequence: 4,
            mutation: Mutation::PinWorkspace {
                workspace: other,
                version: 1,
                directory: None,
            },
        },
    );
    assert!(
        !shared_inventory(&mut host)
            .repositories
            .unwrap()
            .pinned_worktrees
            .contains(&tree),
        "unpinning removes the server's removal restriction"
    );
    let review = review_removal(&mut host, &git.0, &tree);
    std::fs::remove_dir_all(&tree).unwrap();
    assert!(matches!(
        finish_write(
            &mut host,
            5,
            Operation::Remove {
                root: git.0.clone(),
                path: tree.clone(),
                boot: review.boot,
                confirmation: review.confirmation
            }
        ),
        Outcome::WorktreeFailed(_)
    ));
    let review = review_removal(&mut host, &git.0, &tree);
    assert!(!review.changes);
    // A directory appearing after a missing-target confirmation receives no destructive authority.
    std::fs::create_dir(&tree).unwrap();
    std::fs::write(tree.join("keep"), "replacement").unwrap();
    assert_eq!(
        finish_write(
            &mut host,
            6,
            Operation::Remove {
                root: git.0.clone(),
                path: tree.clone(),
                boot: review.boot,
                confirmation: review.confirmation
            }
        ),
        Outcome::WorktreeFailed(Error::Changed)
    );
    assert_eq!(std::fs::read(tree.join("keep")).unwrap(), b"replacement");
    std::fs::remove_dir_all(&tree).unwrap();
    let review = review_removal(&mut host, &git.0, &tree);
    std::os::unix::fs::symlink(git.0.join("absent-target"), &tree).unwrap();
    assert_eq!(
        finish_write(
            &mut host,
            7,
            Operation::Remove {
                root: git.0.clone(),
                path: tree.clone(),
                boot: review.boot,
                confirmation: review.confirmation
            }
        ),
        Outcome::WorktreeFailed(Error::Changed)
    );
    assert_eq!(
        std::fs::read_link(&tree).unwrap(),
        git.0.join("absent-target")
    );

    assert!(
        shared_inventory(&mut host)
            .workspaces
            .iter()
            .any(|w| w.id == workspace)
    );
    host.shutdown().unwrap();
}

#[test]
fn created_worktrees_use_gits_physical_directory_in_saved_membership() {
    use spaceterm_server::{
        hierarchy::Outcome,
        worktree::{Branch, BranchName, Operation},
    };
    let git = GitFixture::new();
    let alias = git.0.join("alias");
    let actual = git.0.join("actual");
    std::fs::create_dir(&actual).unwrap();
    std::os::unix::fs::symlink(&actual, &alias).unwrap();
    let mut prototype = host();
    prototype.directory = git.0.clone();
    let mut host = SharedWorkspaceHost::new(prototype);
    let workspace = create_shared(&mut host, 1);
    let result = finish_write(
        &mut host,
        2,
        Operation::Create {
            workspace,
            root: git.0.clone(),
            path: alias.join("created"),
            branch: Branch::New {
                name: "physical".into(),
                base: BranchName::Local("main".into()),
            },
        },
    );
    let Outcome::Accepted { target } = result else {
        panic!("creation through alias: {result:?}")
    };
    let inventory = shared_inventory(&mut host);
    let tab = inventory
        .workspaces
        .iter()
        .flat_map(|w| &w.tabs)
        .find(|t| t.id == target)
        .unwrap();
    assert_eq!(tab.worktree.as_ref(), Some(&actual.join("created")));
    host.shutdown().unwrap();
}
