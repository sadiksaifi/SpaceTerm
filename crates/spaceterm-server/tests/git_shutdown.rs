use spaceterm_core::{
    authority::*,
    identity::ServerId,
    native_management::*,
    operation_ledger::canonical::CanonicalCodec,
    operation_ledger_contracts::*,
    operations::*,
    organization::{EndReason, SessionPhase},
    storage::*,
    worktrees::*,
};
use spaceterm_server::{
    contracts::ServerFailure,
    git::writes::GitSubmission,
    shutdown::{StoppedAuthorizationPort, TrustedShutdownRequest},
};
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

struct Retirement(NativeRetirementAction);
impl StoppedAuthorizationPort for Retirement {
    fn verify_stopped(
        &self,
        server: ServerId,
        _: ServerEpoch,
    ) -> Result<(NativeManagementRequestRef, NativeRetirementAction), ServerFailure> {
        Ok((
            NativeManagementRequestRef::new(server, [3; 16], RequestDigest::from_bytes([4; 32]))
                .unwrap(),
            self.0,
        ))
    }
}

fn process_exists(pid: i32) -> bool {
    assert!(pid > 1);
    // SAFETY: signal zero observes only the fixture's recorded child. It sends no signal.
    unsafe { libc::kill(pid, 0) == 0 }
}

#[tokio::test]
async fn shutdown_waits_for_long_started_write() {
    long_write_retirement(NativeRetirementAction::Stop, false).await;
}

#[tokio::test]
async fn restart_waits_for_git_settlement_before_retiring_the_native_session() {
    long_write_retirement(NativeRetirementAction::RestartApproved, false).await;
}

#[tokio::test]
async fn activation_waits_for_git_settlement_before_retiring_the_native_session() {
    long_write_retirement(NativeRetirementAction::ActivateUpdate, false).await;
}

#[tokio::test]
async fn replacing_a_client_preserves_the_native_session_and_started_write() {
    long_write_retirement(NativeRetirementAction::Stop, true).await;
}

async fn long_write_retirement(action: NativeRetirementAction, replace_client: bool) {
    let mut fixture = Fixture::new_native_runtime().await;
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
    let pane = fixture.handle.snapshot().state().panes[0].id;
    let session = fixture.start_pane(pane, 2).await;
    let pid = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let Ok(text) = std::fs::read_to_string(root.join("runtime-pid"))
                && let Ok(pid) = text.parse::<i32>()
            {
                break pid;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("native shell must publish its PID");
    assert!(process_exists(pid));
    let before = fixture.handle.snapshot();
    let retained_panes = before.state().panes.clone();
    let runtime = *before
        .runtime()
        .iter()
        .find(|value| value.pane == pane)
        .unwrap();
    let repository = fixture.inventory().await;
    let command = fixture.preview(repository, "blocked").await;
    let binding = CanonicalOperationRequest::new(
        fixture.operation(4),
        Command::Worktree(command.clone()),
        &CanonicalCodec,
    )
    .unwrap()
    .binding();
    assert!(matches!(
        fixture
            .git
            .submit(fixture.git_principal, fixture.operation(4), command)
            .await
            .unwrap(),
        GitSubmission::Pending(_)
    ));
    write_filter.entered("write").await;
    assert_eq!(fixture.runner.writes.load(Ordering::SeqCst), 1);
    if replace_client {
        let original = fixture.principal;
        fixture.replace_client(1).await;
        assert_ne!(fixture.principal.binding(), original.binding());
        let snapshot = fixture.handle.snapshot();
        let after = snapshot
            .runtime()
            .iter()
            .find(|value| value.pane == pane)
            .unwrap();
        assert_eq!(after.phase, runtime.phase);
        assert_eq!(after.session_generation, session.generation);
        assert_eq!(after.control, runtime.control);
        assert_eq!(snapshot.state().panes, retained_panes);
        assert!(
            process_exists(pid),
            "client removal cannot retire the shell"
        );
        assert_eq!(fixture.runner.writes.load(Ordering::SeqCst), 1);
        assert_eq!(
            fixture
                .handle
                .lookup_state(fixture.principal, binding)
                .await
                .unwrap(),
            OperationLookup::Pending
        );
    }
    // This callback observes the real Git process's successful return, before
    // the actor can settle its durable outcome or begin Session retirement.
    *fixture.runner.after_write.lock().unwrap() = Some(Box::new(move || {
        assert!(
            process_exists(pid),
            "the shell was retired before Git returned"
        );
    }));
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
    let authority = std::sync::Arc::new(Retirement(action));
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
    assert!(
        process_exists(pid),
        "blocked Git must keep the native Session alive"
    );
    assert_eq!(
        fixture
            .handle
            .snapshot()
            .runtime()
            .iter()
            .find(|value| value.pane == pane)
            .unwrap()
            .phase,
        runtime.phase,
    );
    write_filter.release();
    let report = tokio::time::timeout(Duration::from_secs(10), stopping)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(report.started_writes_finished && report.cleanup_confirmed);
    fixture.server.wait().await.unwrap();
    assert!(
        !process_exists(pid),
        "retirement must reap the native shell"
    );
    assert_eq!(fixture.runner.writes.load(Ordering::SeqCst), 1);
    let expected_end = match action {
        NativeRetirementAction::Stop | NativeRetirementAction::RestartApproved => EndReason::Stop,
        NativeRetirementAction::ActivateUpdate => EndReason::ServerUpdate,
    };
    assert_eq!(
        fixture
            .handle
            .snapshot()
            .runtime()
            .iter()
            .find(|value| value.pane == pane)
            .unwrap()
            .phase,
        SessionPhase::Ended {
            reason: expected_end
        },
    );
    assert_eq!(fixture.handle.snapshot().state().panes, retained_panes);
    assert!(fixture.directory.0.join("blocked").is_dir());
    let mut store = physical::reopen(&fixture.directory.0).unwrap();
    let recovered = store.recover().unwrap();
    assert!(recovered.git_intents.is_empty() && recovered.git_allocations.is_empty());
    assert!(recovered.native_retirements.is_empty());
    assert_eq!(recovered.organization.server, before.state().server);
    assert_eq!(recovered.organization.panes, retained_panes);
    assert_eq!(
        recovered
            .organization
            .retained
            .as_ref()
            .unwrap()
            .remote_access(),
        before.state().retained.as_ref().unwrap().remote_access(),
    );
    assert_eq!(
        recovered.pairing.as_ref().unwrap().parts().authorizations[0]
            .parts()
            .clients[0]
            .parts()
            .ordinary,
        fixture.stream,
    );
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
    drop(store);
    fixture.authenticator.close().await.unwrap();
    let reopened = Fixture::reopen(fixture.directory).await;
    assert!(
        reopened
            .handle
            .snapshot()
            .runtime()
            .iter()
            .all(|value| { !matches!(value.phase, SessionPhase::Running { .. }) })
    );
    assert_eq!(reopened.handle.snapshot().state().panes, retained_panes);
    assert_eq!(reopened.runner.writes.load(Ordering::SeqCst), 0);
    assert!(matches!(
        reopened.handle.lookup_state(reopened.principal, binding).await.unwrap(),
        OperationLookup::Completed { result: recovered, .. } if recovered == result
    ));
    reopened.stop().await;
}
