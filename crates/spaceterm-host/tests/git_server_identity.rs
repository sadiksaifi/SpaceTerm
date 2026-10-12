#![cfg(any(target_os = "macos", target_os = "linux"))]
use spaceterm_core::identity::*;
use spaceterm_core::operations::*;
use spaceterm_core::worktrees::GitFailure;
use spaceterm_host::git::filesystem::NativeWorktreeFilesystem;
use spaceterm_host::git::server::FinalWriteValidation;
use spaceterm_host::git::*;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::{Arc, mpsc};
use std::time::Duration;
struct Valid;
impl FinalWriteValidation for Valid {
    fn validate(&self) -> Result<(), GitFailure> {
        Ok(())
    }
}
fn git(path: &Path, args: &[&str]) {
    assert!(
        Command::new("/usr/bin/git")
            .args(args)
            .current_dir(path)
            .env_clear()
            .env("HOME", path)
            .env("PATH", "/usr/bin:/bin")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .unwrap()
            .success()
    );
}
fn fixture() -> (tempfile::TempDir, GitCoordinator) {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    git(root, &["init", "-b", "main"]);
    git(
        root,
        &[
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@invalid",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "--allow-empty",
            "-m",
            "initial",
        ],
    );
    let runner: Arc<dyn RepositoryProgramRunner> = Arc::new(UnixRepositoryProgramRunner::new());
    let version = tools::parse_git_version(
        &Command::new("/usr/bin/git")
            .arg("--version")
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap();
    let coordinator = GitCoordinator::new(
        Arc::new(LocalWorktreeGit::new(
            Arc::clone(&runner),
            "/usr/bin/git".into(),
            root.to_path_buf(),
            version,
        )),
        Arc::new(LocalRepositoryReader::new(
            runner,
            Arc::new(UnixRepositoryMarkerReader::new()),
            "/usr/bin/git".into(),
            root.to_path_buf(),
            version,
        )),
        Arc::new(NativeWorktreeFilesystem),
    );
    (directory, coordinator)
}
fn binding(sequence: u64) -> OperationBinding {
    let server = ServerId::new([7; 16]);
    let stream = OperationStreamId::new(
        OperationStreamKey::new(server, 1).unwrap(),
        PairingId::new(server, 1).unwrap(),
        ClientId::new(server, 1).unwrap(),
    )
    .unwrap();
    OperationBinding {
        id: OperationId::new(stream, OperationSequence::new(sequence).unwrap()),
        digest: RequestDigest::from_bytes([1; 32]),
    }
}
fn create(root: &Path, name: &str) -> GitWrite {
    GitWrite::Create {
        path: root.join(name),
        branch: WorktreeBranch::New {
            name: name.into(),
            base: BranchName::Local("main".into()),
        },
    }
}

#[test]
fn alias_paths_share_one_physical_repository_write_lease() {
    let (root, coordinator) = fixture();
    let alias_parent = tempfile::tempdir().unwrap();
    let alias = alias_parent.path().join("alias");
    std::os::unix::fs::symlink(root.path(), &alias).unwrap();
    let cancellation = CancellationToken::default();
    let repository = coordinator.repository(root.path(), &cancellation).unwrap();
    let aliased = coordinator.repository(&alias, &cancellation).unwrap();
    let (_, first) = coordinator
        .prepare_server_write(
            &repository,
            create(root.path(), "one"),
            binding(1),
            &cancellation,
            Arc::new(Valid),
        )
        .unwrap();
    assert!(!root.path().join("one").exists());
    let (sender, arrival) = mpsc::channel();
    let second_coordinator = coordinator.clone();
    let second_path = root.path().to_path_buf();
    let second = std::thread::spawn(move || {
        let prepared = second_coordinator
            .prepare_server_write(
                &aliased,
                create(&second_path, "two"),
                binding(2),
                &CancellationToken::default(),
                Arc::new(Valid),
            )
            .unwrap();
        sender.send(()).unwrap();
        prepared.1
    });
    let executed = first.execute(binding(1)).unwrap();
    assert_eq!(executed.outcome(), GitWriteOutcome::Created);
    assert!(arrival.recv_timeout(Duration::from_millis(100)).is_err());
    executed.finish(Ok(())).unwrap();
    arrival.recv_timeout(Duration::from_secs(10)).unwrap();
    drop(second.join().unwrap());
    assert!(!root.path().join("two").exists());
    coordinator.shutdown();
}

#[test]
fn replaced_common_directory_invalidates_prepared_write() {
    let (root, coordinator) = fixture();
    let cancellation = CancellationToken::default();
    let repository = coordinator.repository(root.path(), &cancellation).unwrap();
    let (_, prepared) = coordinator
        .prepare_server_write(
            &repository,
            create(root.path(), "one"),
            binding(1),
            &cancellation,
            Arc::new(Valid),
        )
        .unwrap();
    std::fs::rename(root.path().join(".git"), root.path().join("old-git")).unwrap();
    std::fs::create_dir(root.path().join(".git")).unwrap();
    let executed = prepared.execute(binding(1)).unwrap();
    assert_eq!(executed.outcome(), GitWriteOutcome::RepositoryChanged);
    executed.finish(Ok(())).unwrap();
    assert!(!root.path().join("one").exists());
    coordinator.shutdown();
}

#[test]
fn failed_final_validation_runs_no_git_write() {
    struct Refuse(std::sync::atomic::AtomicUsize);
    impl FinalWriteValidation for Refuse {
        fn validate(&self) -> Result<(), GitFailure> {
            if self.0.fetch_add(1, std::sync::atomic::Ordering::AcqRel) == 0 {
                Ok(())
            } else {
                Err(GitFailure::GitStateChanged)
            }
        }
    }
    let (root, coordinator) = fixture();
    let cancellation = CancellationToken::default();
    let repository = coordinator.repository(root.path(), &cancellation).unwrap();
    let (_, prepared) = coordinator
        .prepare_server_write(
            &repository,
            create(root.path(), "one"),
            binding(1),
            &cancellation,
            Arc::new(Refuse(std::sync::atomic::AtomicUsize::new(0))),
        )
        .unwrap();
    assert!(matches!(
        prepared.execute(binding(1)),
        Err(GitFailure::GitStateChanged)
    ));
    assert!(!root.path().join("one").exists());
    coordinator.shutdown();
}
