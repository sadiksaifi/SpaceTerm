use super::tests::{create_shared, host, shared_inventory};
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
