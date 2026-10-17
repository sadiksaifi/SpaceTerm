use super::tests::{await_text, command, create_shared, host, shared_inventory, size};
use super::*;
use std::time::Instant;
struct GitFixture(std::path::PathBuf);
impl GitFixture {
    fn new() -> Self {
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
    fn git(&self, arguments: &[&str]) {
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
fn git_facts(
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
        facts
            .repository
            .as_ref()
            .is_some_and(|repo| repo.head == RepositoryHead::Branch("feature".into()))
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
        facts
            .repository
            .as_ref()
            .is_some_and(|repo| repo.changes.total == ChangeTotal::Exact(1))
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
    git_facts(&mut host, &git.0, |facts| facts.repository.is_some());
    assert!(matches!(
        host.request(
            [1; 32],
            [1; 32],
            Request::Mutate {
                sequence: 2,
                mutation: Mutation::CreateTab { workspace }
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
    assert_eq!(after.workspaces[0], expected);
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
