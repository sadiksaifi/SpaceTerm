//! Native evidence that a Worktree removal never reaches a Worktree recreated at the confirmed
//! location, although git gives the replacement the same administrative directory.
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;

use super::{local_filesystem, short_temporary_root};
use crate::platform::unix_repository_program::UnixRepositoryProgramRunner;
use crate::repository_status::ToolVersion;
use crate::ssh::cancellation::SshCancellationToken;
use crate::worktrees::git::{LocalWorktreeGit, RemovalExpectation, WorktreeRemoveError};

/// Where the fixture looks for git, in the order a person's PATH usually lists them.
const GIT_DIRECTORIES: [&str; 4] = ["/opt/homebrew/bin", "/usr/local/bin", "/usr/bin", "/bin"];

struct Fixture {
    root: PathBuf,
    git: PathBuf,
}

impl Fixture {
    /// Returns `None` when the host has no runnable git.
    fn new() -> Option<Self> {
        let git = GIT_DIRECTORIES
            .into_iter()
            .map(|directory| Path::new(directory).join("git"))
            .find(|git| {
                git.is_file()
                    && Command::new(git)
                        .arg("--version")
                        .env_clear()
                        .stdin(Stdio::null())
                        .stdout(Stdio::null())
                        .stderr(Stdio::null())
                        .status()
                        .is_ok_and(|status| status.success())
            })?;
        let root = short_temporary_root().join(format!(
            "spaceterm-worktree-replacement-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("home")).unwrap();
        Some(Self { root, git })
    }

    /// Runs setup git with a private identity and no system or global configuration.
    fn git(&self, directory: &Path, arguments: &[&str]) {
        let output = Command::new(&self.git)
            .args(arguments)
            .current_dir(directory)
            .env_clear()
            .env("HOME", self.root.join("home"))
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", "SpaceTerm")
            .env("GIT_AUTHOR_EMAIL", "spaceterm@example.invalid")
            .env("GIT_COMMITTER_NAME", "SpaceTerm")
            .env("GIT_COMMITTER_EMAIL", "spaceterm@example.invalid")
            .stdin(Stdio::null())
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {arguments:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

#[test]
fn removal_should_refuse_a_worktree_recreated_at_the_confirmed_location() {
    let Some(fixture) = Fixture::new() else {
        eprintln!("skipping: the host has no runnable git");
        return;
    };
    let repository = fixture.root.join("app");
    let location = fixture.root.join("topic");
    fs::create_dir_all(&repository).unwrap();
    fixture.git(&repository, &["init", "-q", "-b", "main"]);
    fixture.git(&repository, &["config", "commit.gpgsign", "false"]);
    fs::write(repository.join("tracked"), b"base\n").unwrap();
    fixture.git(&repository, &["add", "tracked"]);
    fixture.git(&repository, &["commit", "-q", "-m", "base"]);
    let location_text = location.to_str().unwrap();
    fixture.git(
        &repository,
        &["worktree", "add", "-q", "-b", "topic", location_text],
    );
    let git = LocalWorktreeGit::new(
        Arc::new(UnixRepositoryProgramRunner::new()),
        fixture.git.clone(),
        fixture.root.join("home"),
        ToolVersion {
            major: 2,
            minor: 0,
            patch: 0,
        },
    );
    let cancellation = SshCancellationToken::default();
    let filesystem = local_filesystem();
    let confirmed = git
        .check_removal(&location, &filesystem, &cancellation)
        .unwrap();
    assert!(!confirmed.changes);

    // Another process replaces the Worktree while the confirmation is open.
    fixture.git(&repository, &["worktree", "remove", location_text]);
    fixture.git(
        &repository,
        &["worktree", "add", "-q", "-b", "other", location_text],
    );
    fs::write(location.join("notes"), b"unsaved\n").unwrap();

    assert_eq!(
        git.is_worktree(&location, &confirmed.identity, &cancellation),
        Err(WorktreeRemoveError::Replaced)
    );
    assert_eq!(
        git.remove(
            &repository,
            &location,
            true,
            &RemovalExpectation::Worktree(confirmed.identity.clone()),
            &cancellation,
        ),
        Err(WorktreeRemoveError::Replaced)
    );
    assert!(
        location.join("notes").is_file(),
        "the replacement keeps its changes"
    );

    // Confirming the replacement itself removes it.
    let replacement = git
        .check_removal(&location, &filesystem, &cancellation)
        .unwrap();
    assert!(replacement.changes);
    assert_eq!(
        git.remove(
            &repository,
            &location,
            true,
            &RemovalExpectation::Worktree(replacement.identity),
            &cancellation,
        ),
        Ok(())
    );
    assert!(!location.exists());
}
