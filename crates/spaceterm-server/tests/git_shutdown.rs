use spaceterm_core::{
    operation_ledger::canonical::CanonicalCodec, operation_ledger_contracts::*, operations::*,
    storage::*, worktrees::*,
};
use spaceterm_server::{git::writes::GitSubmission, shutdown::TrustedShutdownRequest};
use std::{
    fs::{File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::atomic::Ordering,
    time::Duration,
};

#[allow(dead_code)]
#[path = "git_support/mod.rs"]
mod git_support;
use git_support::{Fixture, physical, run_git};

struct Filter {
    entered: File,
    release: Option<File>,
    script: PathBuf,
}
impl Filter {
    fn new(directory: &Path, name: &str) -> Self {
        let entered_path = directory.join(format!("{name}-entered"));
        let release_path = directory.join(format!("{name}-release"));
        let open = |path: &Path| {
            assert!(
                std::process::Command::new("/usr/bin/mkfifo")
                    .args(["-m", "600"])
                    .arg(path)
                    .status()
                    .unwrap()
                    .success()
            );
            // std opens CLOEXEC. RDWR lets the fixture own release before Git
            // starts, without a startup race or a blocking FIFO open.
            OpenOptions::new()
                .read(true)
                .write(true)
                .open(path)
                .unwrap()
        };
        let entered = open(&entered_path);
        let release = Some(open(&release_path));
        let script = directory.join(format!("{name}-filter"));
        std::fs::write(
            &script,
            format!(
                "#!/bin/sh\nprintf x > '{}'\ncat < '{}' >/dev/null\ncat\n",
                entered_path.display(),
                release_path.display()
            ),
        )
        .unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
        Self {
            entered,
            release,
            script,
        }
    }
    async fn entered(&self, stage: &str) {
        let mut reader = self.entered.try_clone().unwrap();
        let notice = tokio::task::spawn_blocking(move || reader.read_exact(&mut [0]));
        tokio::time::timeout(Duration::from_secs(10), notice)
            .await
            .unwrap_or_else(|_| panic!("{stage} filter did not enter"))
            .unwrap()
            .unwrap();
    }
    fn release(&mut self) {
        if let Some(mut file) = self.release.take() {
            file.write_all(b"x").unwrap();
        }
    }
}
impl Drop for Filter {
    fn drop(&mut self) {
        self.release();
        // Release a notice reader if a failing test never reached the filter.
        let _ = self.entered.write_all(b"x");
    }
}

#[tokio::test]
async fn shutdown_waits_for_long_started_write() {
    let fixture = Fixture::new().await;
    let root = fixture.directory.0.join("repository");
    let mut write_filter = Filter::new(&fixture.directory.0, "write");
    let _read_filter = Filter::new(&fixture.directory.0, "read");
    std::fs::write(
        root.join(".gitattributes"),
        "write.data filter=hold-write\nread.data filter=hold-read\n",
    )
    .unwrap();
    std::fs::write(root.join("write.data"), "written by the checkout filter\n").unwrap();
    std::fs::write(root.join("read.data"), "base\n").unwrap();
    run_git(&root, &["add", ".gitattributes", "write.data", "read.data"]);
    run_git(
        &root,
        &[
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "commit",
            "--quiet",
            "-m",
            "Filter fixture",
        ],
    );
    run_git(
        &root,
        &[
            "config",
            "filter.hold-write.smudge",
            write_filter.script.to_str().unwrap(),
        ],
    );
    run_git(&root, &["config", "filter.hold-write.clean", "cat"]);
    run_git(
        &root,
        &[
            "config",
            "filter.hold-read.clean",
            _read_filter.script.to_str().unwrap(),
        ],
    );
    run_git(&root, &["config", "filter.hold-read.smudge", "cat"]);
    let repository = fixture.inventory().await;
    let command = fixture.preview(repository, "blocked").await;
    let binding = CanonicalOperationRequest::new(
        fixture.operation(2),
        Command::Worktree(command.clone()),
        &CanonicalCodec,
    )
    .unwrap()
    .binding();
    assert!(matches!(
        fixture
            .git
            .submit(fixture.git_principal, fixture.operation(2), command)
            .await
            .unwrap(),
        GitSubmission::Pending(_)
    ));
    write_filter.entered("write").await;
    assert_eq!(fixture.runner.writes.load(Ordering::SeqCst), 1);
    std::fs::write(root.join("read.data"), "diff\n").unwrap();
    let git = fixture.git.clone();
    let principal = fixture.git_principal;
    let workspace = fixture.workspace;
    let reading = tokio::spawn(async move {
        git.query(
            principal,
            GitQuery::Status {
                target: GitStatusTarget::Workspace(workspace),
            },
        )
        .await
    });
    _read_filter.entered("read").await;
    let handle = fixture.handle.clone();
    let authority = fixture.stop.clone();
    let mut stopping =
        tokio::spawn(async move { handle.shutdown(TrustedShutdownRequest { authority }).await });
    let read = tokio::time::timeout(Duration::from_secs(10), reading)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        read.unwrap_err(),
        GitFailure::CancelledRead,
        "ordinary shutdown cancels the live clean-filter read"
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(100), &mut stopping)
            .await
            .is_err(),
        "shutdown cannot finish or kill the pipe-blocked Started write"
    );
    assert_eq!(fixture.runner.writes.load(Ordering::SeqCst), 1);
    write_filter.release();
    let report = tokio::time::timeout(Duration::from_secs(10), stopping)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(report.started_writes_finished && report.cleanup_confirmed);
    fixture.server.wait().await.unwrap();
    assert!(fixture.directory.0.join("blocked").is_dir());
    let mut store = physical::reopen(&fixture.directory.0).unwrap();
    let recovered = store.recover().unwrap();
    assert!(recovered.git_intents.is_empty() && recovered.git_allocations.is_empty());
    let result = recovered
        .ledger
        .unwrap()
        .operations()
        .iter()
        .find_map(|entry| match entry {
            RecoveredOperationRecord::Completed(record) if record.binding == binding => {
                Some(record.outcome.outcome(&CanonicalCodec).unwrap())
            }
            _ => None,
        })
        .unwrap();
    assert!(
        matches!(result, Outcome::Git(ref outcome) if matches!(outcome.parts(), GitMutationOutcomeParts::Created { .. }))
    );
}
