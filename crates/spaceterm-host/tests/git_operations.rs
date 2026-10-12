#![cfg(any(target_os = "macos", target_os = "linux"))]

use std::collections::VecDeque;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

use spaceterm_host::git::filesystem::{NativeWorktreeFilesystem, WorktreeFilesystem};
use spaceterm_host::git::{
    BranchName, CancellationToken, GitCoordinator, GitWrite, GitWriteOutcome, IntentError,
    LocalRepositoryReader, LocalWorktreeGit, PendingWriteIntent, ProgramError, ProgramExit,
    ProgramRequest, RepositoryProgramRunner, UnixRepositoryMarkerReader,
    UnixRepositoryProgramRunner, WorktreeBranch, WriteIntentHook,
};

const WAIT: Duration = Duration::from_secs(10);

struct Fixture(PathBuf);

impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!(
            "spaceterm-host-git-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&root).unwrap();
        let fixture = Self(root);
        fixture.git(&["init", "-b", "main"]);
        fixture.git(&[
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
        ]);
        fixture
    }

    fn git(&self, arguments: &[&str]) {
        let status = Command::new("/usr/bin/git")
            .args(arguments)
            .current_dir(&self.0)
            .env_clear()
            .env("HOME", &self.0)
            .env("PATH", "/usr/bin:/bin")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .unwrap();
        assert!(status.success());
    }

    fn create(&self, name: &str) -> GitWrite {
        GitWrite::Create {
            path: self.0.join(name),
            branch: WorktreeBranch::New {
                name: name.into(),
                base: BranchName::Local("main".into()),
            },
        }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

struct Gate {
    entered: mpsc::Sender<()>,
    release: mpsc::Receiver<()>,
}

#[derive(Default)]
struct GatedRunner {
    gates: Mutex<VecDeque<Gate>>,
    writes: AtomicUsize,
    unavailable: AtomicBool,
}

impl GatedRunner {
    fn gate(&self) -> (mpsc::Receiver<()>, mpsc::Sender<()>) {
        let (entered, arrival) = mpsc::channel();
        let (release, resume) = mpsc::channel();
        self.gates.lock().unwrap().push_back(Gate {
            entered,
            release: resume,
        });
        (arrival, release)
    }
}

impl RepositoryProgramRunner for GatedRunner {
    fn run(
        &self,
        request: &ProgramRequest,
        stdout: &mut dyn FnMut(&[u8]),
        cancellation: &CancellationToken,
    ) -> Result<ProgramExit, ProgramError> {
        let is_write = request
            .arguments
            .windows(2)
            .any(|pair| pair[0] == "worktree" && (pair[1] == "add" || pair[1] == "remove"));
        if !is_write && self.unavailable.load(Ordering::Acquire) {
            return Err(ProgramError::Failed);
        }
        if is_write {
            self.writes.fetch_add(1, Ordering::Relaxed);
            let gate = self.gates.lock().unwrap().pop_front();
            if let Some(gate) = gate {
                gate.entered.send(()).unwrap();
                gate.release.recv_timeout(WAIT).unwrap();
            }
        }
        UnixRepositoryProgramRunner::new().run(request, stdout, cancellation)
    }
}

#[derive(Default)]
struct IntentLog {
    events: Mutex<Vec<&'static str>>,
    intents: Mutex<Vec<PendingWriteIntent>>,
    fail_record: bool,
    fail_finish: bool,
}

impl WriteIntentHook for IntentLog {
    fn record(&self, intent: &PendingWriteIntent) -> Result<(), IntentError> {
        self.events.lock().unwrap().push("intent");
        self.intents.lock().unwrap().push(intent.clone());
        if self.fail_record {
            Err(IntentError)
        } else {
            Ok(())
        }
    }

    fn finish(&self, _: &PendingWriteIntent, _: GitWriteOutcome) -> Result<(), IntentError> {
        self.events.lock().unwrap().push("outcome");
        if self.fail_finish {
            Err(IntentError)
        } else {
            Ok(())
        }
    }
}

fn coordinator(fixture: &Fixture, runner: Arc<dyn RepositoryProgramRunner>) -> GitCoordinator {
    coordinator_with_filesystem(fixture, runner, Arc::new(NativeWorktreeFilesystem))
}

fn coordinator_with_filesystem(
    fixture: &Fixture,
    runner: Arc<dyn RepositoryProgramRunner>,
    filesystem: Arc<dyn WorktreeFilesystem>,
) -> GitCoordinator {
    let version = spaceterm_host::git::tools::parse_git_version(
        &Command::new("/usr/bin/git")
            .arg("--version")
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap();
    let worktrees = Arc::new(LocalWorktreeGit::new(
        Arc::clone(&runner),
        "/usr/bin/git".into(),
        fixture.0.clone(),
        version,
    ));
    let reader = Arc::new(LocalRepositoryReader::new(
        runner,
        Arc::new(UnixRepositoryMarkerReader::new()),
        "/usr/bin/git".into(),
        fixture.0.clone(),
        version,
    ));
    GitCoordinator::new(worktrees, reader, filesystem)
}

#[test]
fn same_repository_writes_should_wait_for_the_previous_outcome() {
    let fixture = Fixture::new();
    let runner = Arc::new(GatedRunner::default());
    let (first_started, release_first) = runner.gate();
    let (second_started, release_second) = runner.gate();
    let coordinator = coordinator(&fixture, runner);
    let cancellation = CancellationToken::default();
    let repository = coordinator.repository(&fixture.0, &cancellation).unwrap();
    let first_intent = Arc::new(IntentLog::default());
    let second_intent = Arc::new(IntentLog::default());
    let first = coordinator
        .write(
            &repository,
            fixture.create("one"),
            &cancellation,
            first_intent.clone(),
        )
        .unwrap();
    first_started.recv_timeout(WAIT).unwrap();
    let second = coordinator
        .write(
            &repository,
            fixture.create("two"),
            &cancellation,
            second_intent.clone(),
        )
        .unwrap();

    assert!(
        second_started
            .recv_timeout(Duration::from_millis(100))
            .is_err()
    );
    assert_eq!(*first_intent.events.lock().unwrap(), ["intent"]);
    assert!(second_intent.events.lock().unwrap().is_empty());
    release_first.send(()).unwrap();
    second_started.recv_timeout(WAIT).unwrap();
    assert_eq!(first.join().unwrap().outcome, GitWriteOutcome::Created);
    assert_eq!(*first_intent.events.lock().unwrap(), ["intent", "outcome"]);
    release_second.send(()).unwrap();
    assert_eq!(second.join().unwrap().outcome, GitWriteOutcome::Created);
    coordinator.shutdown();
}

#[test]
fn started_writes_should_ignore_client_cancellation_and_block_orderly_shutdown() {
    use spaceterm_host::git::WriteError;

    let fixture = Fixture::new();
    let runner = Arc::new(GatedRunner::default());
    let (started, release) = runner.gate();
    let coordinator = coordinator(&fixture, runner.clone());
    let client = CancellationToken::default();
    let repository = coordinator.repository(&fixture.0, &client).unwrap();
    let intent = Arc::new(IntentLog::default());
    let ticket = coordinator
        .write(
            &repository,
            fixture.create("started"),
            &client,
            intent.clone(),
        )
        .unwrap();
    started.recv_timeout(WAIT).unwrap();
    let queued_intent = Arc::new(IntentLog::default());
    let queued = coordinator
        .write(
            &repository,
            fixture.create("queued"),
            &client,
            queued_intent.clone(),
        )
        .unwrap();
    client.cancel();
    assert_eq!(queued.join(), Err(WriteError::Cancelled));
    // A held write lane cannot stall a Repository Status read.
    assert!(
        coordinator
            .probe(&fixture.0, &CancellationToken::default())
            .is_ok()
    );
    assert!(queued_intent.events.lock().unwrap().is_empty());

    let (stopped, stop_result) = mpsc::channel();
    let shutdown = coordinator.clone();
    let stop_thread = std::thread::spawn(move || {
        shutdown.shutdown();
        stopped.send(()).unwrap();
    });
    assert!(
        stop_result
            .recv_timeout(Duration::from_millis(100))
            .is_err()
    );
    release.send(()).unwrap();
    assert_eq!(ticket.join().unwrap().outcome, GitWriteOutcome::Created);
    stop_result.recv_timeout(WAIT).unwrap();
    stop_thread.join().unwrap();
    assert_eq!(*intent.events.lock().unwrap(), ["intent", "outcome"]);
    assert_eq!(runner.writes.load(Ordering::Relaxed), 1);
    assert!(matches!(
        coordinator.write(
            &repository,
            fixture.create("later"),
            &CancellationToken::default(),
            Arc::new(IntentLog::default())
        ),
        Err(WriteError::ShuttingDown)
    ));
}

#[test]
fn linked_worktrees_and_symlink_aliases_should_share_one_physical_write_lane() {
    use std::os::unix::fs::symlink;

    let fixture = Fixture::new();
    let linked = fixture.0.join("linked");
    fixture.git(&["worktree", "add", "-b", "linked", linked.to_str().unwrap()]);
    let alias = fixture.0.join("alias");
    symlink(&fixture.0, &alias).unwrap();
    let runner = Arc::new(GatedRunner::default());
    let (first_started, release_first) = runner.gate();
    let (second_started, release_second) = runner.gate();
    let coordinator = coordinator(&fixture, runner.clone());
    let token = CancellationToken::default();
    let repository = coordinator.repository(&alias, &token).unwrap();
    let linked_repository = coordinator.repository(&linked, &token).unwrap();
    let first = coordinator
        .write(
            &repository,
            fixture.create("one"),
            &token,
            Arc::new(IntentLog::default()),
        )
        .unwrap();
    first_started.recv_timeout(WAIT).unwrap();
    let second = coordinator
        .write(
            &linked_repository,
            fixture.create("two"),
            &token,
            Arc::new(IntentLog::default()),
        )
        .unwrap();
    assert!(
        second_started
            .recv_timeout(Duration::from_millis(100))
            .is_err()
    );
    release_first.send(()).unwrap();
    second_started.recv_timeout(WAIT).unwrap();
    release_second.send(()).unwrap();
    assert_eq!(first.join().unwrap().outcome, GitWriteOutcome::Created);
    assert_eq!(second.join().unwrap().outcome, GitWriteOutcome::Created);
    coordinator.shutdown();
}

#[test]
fn bare_repository_linked_worktrees_should_support_writes_and_reconciliation() {
    use spaceterm_host::git::{Reconciliation, RemovalExpectation};

    let fixture = Fixture::new();
    let bare = fixture.0.join("bare.git");
    fixture.git(&[
        "clone",
        "--bare",
        fixture.0.to_str().unwrap(),
        bare.to_str().unwrap(),
    ]);
    let linked = fixture.0.join("linked");
    fixture.git(&[
        "-C",
        bare.to_str().unwrap(),
        "worktree",
        "add",
        "-b",
        "linked",
        linked.to_str().unwrap(),
    ]);
    let runner = Arc::new(GatedRunner::default());
    let coordinator = coordinator(&fixture, runner.clone());
    let token = CancellationToken::default();
    let repository = coordinator.repository(&linked, &token).unwrap();
    // Bare repositories stay hidden from presentation, but own their linked Worktree writes.
    assert!(coordinator.list(&bare, &token).unwrap().is_none());
    let created = Arc::new(IntentLog::default());
    let ticket = coordinator
        .write(
            &repository,
            fixture.create("feature"),
            &token,
            created.clone(),
        )
        .unwrap();
    assert_eq!(ticket.join().unwrap().outcome, GitWriteOutcome::Created);
    let intent = created.intents.lock().unwrap()[0].clone();
    assert_eq!(
        std::fs::canonicalize(&intent.main_root).unwrap(),
        std::fs::canonicalize(&bare).unwrap()
    );
    assert_eq!(
        coordinator.reconcile(&intent, &token).unwrap(),
        Reconciliation::ObservedApplied
    );
    let path = fixture.0.join("feature");
    let checked = coordinator.check_removal(&path, &token).unwrap();
    let removed = Arc::new(IntentLog::default());
    let ticket = coordinator
        .write(
            &repository,
            GitWrite::Remove {
                path,
                force: false,
                expected: RemovalExpectation::Worktree {
                    identity: checked.identity,
                    require_clean: true,
                },
            },
            &token,
            removed.clone(),
        )
        .unwrap();
    assert_eq!(ticket.join().unwrap().outcome, GitWriteOutcome::Removed);
    assert_eq!(
        coordinator
            .reconcile(&removed.intents.lock().unwrap()[0], &token)
            .unwrap(),
        Reconciliation::ObservedApplied
    );
    assert_eq!(runner.writes.load(Ordering::Relaxed), 2);
    coordinator.shutdown();
}

#[test]
fn different_repositories_should_write_concurrently() {
    let first_fixture = Fixture::new();
    let second_fixture = Fixture::new();
    let runner = Arc::new(GatedRunner::default());
    let (first_started, release_first) = runner.gate();
    let (second_started, release_second) = runner.gate();
    let coordinator = coordinator(&first_fixture, runner.clone());
    let token = CancellationToken::default();
    let first_repository = coordinator.repository(&first_fixture.0, &token).unwrap();
    let second_repository = coordinator.repository(&second_fixture.0, &token).unwrap();
    let first = coordinator
        .write(
            &first_repository,
            first_fixture.create("one"),
            &token,
            Arc::new(IntentLog::default()),
        )
        .unwrap();
    first_started.recv_timeout(WAIT).unwrap();
    let second = coordinator
        .write(
            &second_repository,
            second_fixture.create("two"),
            &token,
            Arc::new(IntentLog::default()),
        )
        .unwrap();
    second_started.recv_timeout(WAIT).unwrap();
    release_second.send(()).unwrap();
    assert_eq!(second.join().unwrap().outcome, GitWriteOutcome::Created);
    release_first.send(()).unwrap();
    assert_eq!(first.join().unwrap().outcome, GitWriteOutcome::Created);
    coordinator.shutdown();
}

#[test]
fn intent_failure_should_prevent_execution_and_outcome_failure_should_not_repeat_it() {
    use spaceterm_host::git::{Reconciliation, WriteError};

    let fixture = Fixture::new();
    let runner = Arc::new(GatedRunner::default());
    let coordinator = coordinator(&fixture, runner.clone());
    let token = CancellationToken::default();
    let repository = coordinator.repository(&fixture.0, &token).unwrap();
    let failed_intent = Arc::new(IntentLog {
        fail_record: true,
        ..Default::default()
    });
    let denied = coordinator
        .write(
            &repository,
            fixture.create("denied"),
            &token,
            failed_intent.clone(),
        )
        .unwrap();
    assert_eq!(denied.join(), Err(WriteError::IntentUnconfirmed));
    assert_eq!(runner.writes.load(Ordering::Relaxed), 0);
    assert_eq!(*failed_intent.events.lock().unwrap(), ["intent"]);
    assert!(!fixture.0.join("denied").exists());

    let uncertain = Arc::new(IntentLog {
        fail_finish: true,
        ..Default::default()
    });
    let ticket = coordinator
        .write(
            &repository,
            fixture.create("created"),
            &token,
            uncertain.clone(),
        )
        .unwrap();
    let completion = ticket.join().unwrap();
    assert_eq!(completion.outcome, GitWriteOutcome::Created);
    assert_eq!(completion.recording, Err(IntentError));
    let pending = uncertain.intents.lock().unwrap()[0].clone();
    assert_eq!(
        coordinator.reconcile(&pending, &token).unwrap(),
        Reconciliation::ObservedApplied
    );
    assert_eq!(runner.writes.load(Ordering::Relaxed), 1);
    coordinator.shutdown();
}

struct SlowReadRunner {
    slow: AtomicBool,
    started: mpsc::Sender<()>,
}

impl RepositoryProgramRunner for SlowReadRunner {
    fn run(
        &self,
        request: &ProgramRequest,
        stdout: &mut dyn FnMut(&[u8]),
        cancellation: &CancellationToken,
    ) -> Result<ProgramExit, ProgramError> {
        if request
            .arguments
            .iter()
            .any(|argument| argument == "for-each-ref")
            && self.slow.swap(false, Ordering::AcqRel)
        {
            let mut delayed = request.clone();
            delayed.executable = "/bin/sh".into();
            delayed.arguments = ["-c", "printf ready; exec sleep 30"].map(Into::into).into();
            let mut announced = false;
            UnixRepositoryProgramRunner::new().run(
                &delayed,
                &mut |chunk| {
                    if !announced {
                        self.started.send(()).unwrap();
                        announced = true;
                    }
                    stdout(chunk);
                },
                cancellation,
            )
        } else {
            UnixRepositoryProgramRunner::new().run(request, stdout, cancellation)
        }
    }
}

#[test]
fn shutdown_should_cancel_and_reap_a_read_without_blocking_another_read() {
    use spaceterm_host::git::RepositoryReadError;

    let fixture = Fixture::new();
    let (started, arrival) = mpsc::channel();
    let runner = Arc::new(SlowReadRunner {
        slow: AtomicBool::new(true),
        started,
    });
    let coordinator = coordinator(&fixture, runner);
    let reader = coordinator.clone();
    let root = fixture.0.clone();
    let read = std::thread::spawn(move || reader.branches(&root, &CancellationToken::default()));
    arrival.recv_timeout(WAIT).unwrap();
    assert!(
        coordinator
            .list(&fixture.0, &CancellationToken::default())
            .unwrap()
            .is_some()
    );
    coordinator.shutdown();
    assert_eq!(read.join().unwrap(), Err(RepositoryReadError::Cancelled));
    assert_eq!(
        coordinator.branches(&fixture.0, &CancellationToken::default()),
        Err(RepositoryReadError::Cancelled)
    );
}

#[test]
fn client_cancellation_should_cancel_a_read_without_closing_the_coordinator() {
    use spaceterm_host::git::RepositoryReadError;

    let fixture = Fixture::new();
    let (started, arrival) = mpsc::channel();
    let runner = Arc::new(SlowReadRunner {
        slow: AtomicBool::new(true),
        started,
    });
    let coordinator = coordinator(&fixture, runner);
    let token = CancellationToken::default();
    let reader_token = token.clone();
    let reader = coordinator.clone();
    let root = fixture.0.clone();
    let read = std::thread::spawn(move || reader.branches(&root, &reader_token));
    arrival.recv_timeout(WAIT).unwrap();
    token.cancel();
    assert_eq!(read.join().unwrap(), Err(RepositoryReadError::Cancelled));
    assert_eq!(
        coordinator
            .branches(&fixture.0, &CancellationToken::default())
            .unwrap()
            .local,
        ["main"]
    );
    coordinator.shutdown();
}

#[test]
fn removal_should_revalidate_cleanliness_and_retained_physical_identity() {
    use spaceterm_host::git::{RemovalExpectation, WorktreeRemoveError};

    let fixture = Fixture::new();
    let path = fixture.0.join("feature");
    fixture.git(&["worktree", "add", "-b", "feature", path.to_str().unwrap()]);
    let runner = Arc::new(GatedRunner::default());
    let coordinator = coordinator(&fixture, runner.clone());
    let token = CancellationToken::default();
    let repository = coordinator.repository(&fixture.0, &token).unwrap();
    let checked = coordinator.check_removal(&path, &token).unwrap();
    assert!(!checked.changes);
    std::fs::write(path.join("untracked"), "private contents").unwrap();
    let clean_expectation = RemovalExpectation::Worktree {
        identity: checked.identity.clone(),
        require_clean: true,
    };
    let remove = |expected| GitWrite::Remove {
        path: path.clone(),
        force: true,
        expected,
    };
    let ticket = coordinator
        .write(
            &repository,
            remove(clean_expectation),
            &token,
            Arc::new(IntentLog::default()),
        )
        .unwrap();
    assert_eq!(
        ticket.join().unwrap().outcome,
        GitWriteOutcome::RemoveRejected(WorktreeRemoveError::Changed)
    );
    assert_eq!(runner.writes.load(Ordering::Relaxed), 0);
    let original = fixture.0.join("retained");
    std::fs::rename(&path, &original).unwrap();
    std::fs::create_dir(&path).unwrap();
    let expectation = RemovalExpectation::Worktree {
        identity: checked.identity,
        require_clean: false,
    };
    let ticket = coordinator
        .write(
            &repository,
            remove(expectation.clone()),
            &token,
            Arc::new(IntentLog::default()),
        )
        .unwrap();
    assert_eq!(
        ticket.join().unwrap().outcome,
        GitWriteOutcome::RemoveRejected(WorktreeRemoveError::Replaced)
    );
    assert_eq!(runner.writes.load(Ordering::Relaxed), 0);
    std::fs::remove_dir(&path).unwrap();
    std::fs::rename(&original, &path).unwrap();
    let removed = Arc::new(IntentLog::default());
    let ticket = coordinator
        .write(&repository, remove(expectation), &token, removed.clone())
        .unwrap();
    assert_eq!(ticket.join().unwrap().outcome, GitWriteOutcome::Removed);
    assert_eq!(
        coordinator
            .reconcile(&removed.intents.lock().unwrap()[0], &token)
            .unwrap(),
        spaceterm_host::git::Reconciliation::ObservedApplied
    );
    assert!(
        coordinator
            .branches(&fixture.0, &token)
            .unwrap()
            .local
            .contains(&"feature".into())
    );
    coordinator.shutdown();
}

#[test]
fn retained_directory_budget_should_leave_headroom_for_admitted_writes() {
    use spaceterm_host::git::RemovalExpectation;
    use spaceterm_host::git::filesystem::DirectoryError;

    // The native identity budget is process-wide. Run alone, even in the parallel host gate.
    if std::env::var_os("SPACETERM_GIT_HANDLE_BUDGET_CHILD").is_none() {
        let output = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "retained_directory_budget_should_leave_headroom_for_admitted_writes",
                "--test-threads=1",
            ])
            .env("SPACETERM_GIT_HANDLE_BUDGET_CHILD", "1")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }

    let fixture = Fixture::new();
    let removable = fixture.0.join("removable");
    fixture.git(&[
        "worktree",
        "add",
        "-b",
        "removable",
        removable.to_str().unwrap(),
    ]);
    let runner = Arc::new(GatedRunner::default());
    let coordinator = coordinator(&fixture, runner.clone());
    let token = CancellationToken::default();
    // Forty-seven independent repository leases retain 94 identity handles. A removal proof
    // consumes one more, leaving no space for temporary retained repository leases.
    let repositories: Vec<_> = (0..47)
        .map(|_| coordinator.repository(&fixture.0, &token).unwrap())
        .collect();
    let checked = coordinator.check_removal(&removable, &token).unwrap();
    assert!(matches!(
        NativeWorktreeFilesystem.validate_directory(&fixture.0),
        Err(DirectoryError::Capacity)
    ));
    let created = Arc::new(IntentLog::default());
    let ticket = coordinator
        .write(
            &repositories[0],
            fixture.create("feature"),
            &token,
            created.clone(),
        )
        .unwrap();
    assert_eq!(ticket.join().unwrap().outcome, GitWriteOutcome::Created);
    assert_eq!(*created.events.lock().unwrap(), ["intent", "outcome"]);
    let ticket = coordinator
        .write(
            &repositories[1],
            GitWrite::Remove {
                path: removable,
                force: false,
                expected: RemovalExpectation::Worktree {
                    identity: checked.identity,
                    require_clean: true,
                },
            },
            &token,
            Arc::new(IntentLog::default()),
        )
        .unwrap();
    assert_eq!(ticket.join().unwrap().outcome, GitWriteOutcome::Removed);
    assert_eq!(runner.writes.load(Ordering::Relaxed), 2);
    coordinator.shutdown();
}

#[test]
fn unavailable_revalidation_should_not_report_a_changed_repository() {
    let fixture = Fixture::new();
    let runner = Arc::new(GatedRunner::default());
    let coordinator = coordinator(&fixture, runner.clone());
    let token = CancellationToken::default();
    let repository = coordinator.repository(&fixture.0, &token).unwrap();
    runner.unavailable.store(true, Ordering::Release);
    let intent = Arc::new(IntentLog::default());
    let ticket = coordinator
        .write(
            &repository,
            fixture.create("feature"),
            &token,
            intent.clone(),
        )
        .unwrap();
    assert_eq!(
        ticket.join().unwrap().outcome,
        GitWriteOutcome::RepositoryUnavailable
    );
    assert_eq!(runner.writes.load(Ordering::Relaxed), 0);
    assert_eq!(*intent.events.lock().unwrap(), ["intent", "outcome"]);
    coordinator.shutdown();
}

#[test]
fn bounded_write_admission_should_cancel_queued_work_before_intent() {
    use spaceterm_host::git::WriteError;

    let fixture = Fixture::new();
    let runner = Arc::new(GatedRunner::default());
    let (started, release) = runner.gate();
    let coordinator = coordinator(&fixture, runner.clone());
    let token = CancellationToken::default();
    let repository = coordinator.repository(&fixture.0, &token).unwrap();
    let first = coordinator
        .write(
            &repository,
            fixture.create("started"),
            &token,
            Arc::new(IntentLog::default()),
        )
        .unwrap();
    started.recv_timeout(WAIT).unwrap();
    let intent = Arc::new(IntentLog::default());
    let queued: Vec<_> = (0..63)
        .map(|_| {
            coordinator
                .write(
                    &repository,
                    fixture.create("queued"),
                    &token,
                    intent.clone(),
                )
                .unwrap()
        })
        .collect();
    assert!(matches!(
        coordinator.write(
            &repository,
            fixture.create("excess"),
            &token,
            intent.clone()
        ),
        Err(WriteError::Capacity)
    ));
    token.cancel();
    for ticket in queued {
        assert_eq!(ticket.join(), Err(WriteError::Cancelled));
    }
    assert!(intent.events.lock().unwrap().is_empty());
    release.send(()).unwrap();
    assert_eq!(first.join().unwrap().outcome, GitWriteOutcome::Created);
    coordinator.shutdown();
    assert_eq!(runner.writes.load(Ordering::Relaxed), 1);
}

#[test]
fn native_program_failures_should_bound_output_and_reap_at_the_deadline() {
    let fixture = Fixture::new();
    let mut request = ProgramRequest {
        executable: "/bin/sh".into(),
        arguments: ["-c", "printf confidential"].map(Into::into).into(),
        directory: fixture.0.clone(),
        environment: Vec::new(),
        stdout_limit: Some(4),
        deadline: None,
        keep_process_group_on_exit: false,
    };
    let mut bytes = Vec::new();
    let runner = UnixRepositoryProgramRunner::new();
    let result = runner.run(
        &request,
        &mut |chunk| bytes.extend_from_slice(chunk),
        &CancellationToken::default(),
    );
    assert_eq!(result, Err(ProgramError::OutputTooLarge));
    assert!(bytes.is_empty());
    assert!(!format!("{result:?}").contains("confidential"));
    request.arguments = ["-c", "exec /bin/sleep 30"].map(Into::into).into();
    request.deadline = Some(Instant::now() + Duration::from_millis(100));
    assert_eq!(
        runner.run(&request, &mut |_| {}, &CancellationToken::default()),
        Err(ProgramError::TimedOut)
    );
}

#[test]
fn reconciliation_should_classify_partial_creation_without_issuing_a_write() {
    use spaceterm_host::git::{DurableGitWrite, Reconciliation};

    let fixture = Fixture::new();
    let runner = Arc::new(GatedRunner::default());
    let coordinator = coordinator(&fixture, runner.clone());
    let token = CancellationToken::default();
    let mut intent = PendingWriteIntent {
        main_root: fixture.0.clone(),
        common_directory: fixture.0.join(".git"),
        write: DurableGitWrite::Create {
            path: fixture.0.join("feature"),
            branch: WorktreeBranch::New {
                name: "feature".into(),
                base: BranchName::Local("main".into()),
            },
        },
    };
    assert_eq!(
        coordinator.reconcile(&intent, &token).unwrap(),
        Reconciliation::ObservedNotApplied
    );
    // A terminated add can leave its new branch before its Worktree is published.
    fixture.git(&["branch", "feature"]);
    assert_eq!(
        coordinator.reconcile(&intent, &token).unwrap(),
        Reconciliation::NeedsReview
    );
    // A replacement directory is not proof that the intended checkout completed.
    std::fs::create_dir(fixture.0.join("feature")).unwrap();
    assert_eq!(
        coordinator.reconcile(&intent, &token).unwrap(),
        Reconciliation::NeedsReview
    );
    intent.write = DurableGitWrite::Remove {
        path: fixture.0.join("feature"),
        force: true,
    };
    assert_eq!(
        coordinator.reconcile(&intent, &token).unwrap(),
        Reconciliation::NeedsReview
    );
    assert_eq!(runner.writes.load(Ordering::Relaxed), 0);
    coordinator.shutdown();
}

#[test]
fn absent_removal_confirmation_should_refuse_a_dangling_replacement_link() {
    use spaceterm_host::git::{RemovalExpectation, WorktreeRemoveError};
    use std::os::unix::fs::symlink;

    let fixture = Fixture::new();
    let path = fixture.0.join("missing");
    fixture.git(&["worktree", "add", "-b", "missing", path.to_str().unwrap()]);
    std::fs::remove_dir_all(&path).unwrap();
    symlink(fixture.0.join("elsewhere"), &path).unwrap();
    let runner = Arc::new(GatedRunner::default());
    let coordinator = coordinator(&fixture, runner.clone());
    let token = CancellationToken::default();
    let repository = coordinator.repository(&fixture.0, &token).unwrap();
    let expected = RemovalExpectation::Absent(Arc::new(NativeWorktreeFilesystem));
    let ticket = coordinator
        .write(
            &repository,
            GitWrite::Remove {
                path,
                force: true,
                expected,
            },
            &token,
            Arc::new(IntentLog::default()),
        )
        .unwrap();
    assert_eq!(
        ticket.join().unwrap().outcome,
        GitWriteOutcome::RemoveRejected(WorktreeRemoveError::Replaced)
    );
    assert_eq!(runner.writes.load(Ordering::Relaxed), 0);
    coordinator.shutdown();
}

#[test]
fn reconciliation_should_not_accept_a_replacement_directory_as_a_completed_checkout() {
    use spaceterm_host::git::{DurableGitWrite, Reconciliation};

    let fixture = Fixture::new();
    let path = fixture.0.join("feature");
    fixture.git(&["worktree", "add", "-b", "feature", path.to_str().unwrap()]);
    let original = fixture.0.join("retained");
    std::fs::rename(&path, &original).unwrap();
    std::fs::create_dir(&path).unwrap();
    let runner = Arc::new(GatedRunner::default());
    let coordinator = coordinator(&fixture, runner.clone());
    let token = CancellationToken::default();
    let intent = PendingWriteIntent {
        main_root: fixture.0.clone(),
        common_directory: fixture.0.join(".git"),
        write: DurableGitWrite::Create {
            path,
            branch: WorktreeBranch::New {
                name: "feature".into(),
                base: BranchName::Local("main".into()),
            },
        },
    };
    assert_eq!(
        coordinator.reconcile(&intent, &token).unwrap(),
        Reconciliation::NeedsReview
    );
    assert_eq!(runner.writes.load(Ordering::Relaxed), 0);
    coordinator.shutdown();
}

#[test]
fn shutdown_should_cancel_queued_writes_and_all_callers_should_join_the_started_write() {
    use spaceterm_host::git::WriteError;

    let fixture = Fixture::new();
    let runner = Arc::new(GatedRunner::default());
    let (started, release) = runner.gate();
    let coordinator = coordinator(&fixture, runner);
    let token = CancellationToken::default();
    let repository = coordinator.repository(&fixture.0, &token).unwrap();
    let first = coordinator
        .write(
            &repository,
            fixture.create("first"),
            &token,
            Arc::new(IntentLog::default()),
        )
        .unwrap();
    started.recv_timeout(WAIT).unwrap();
    let queued_intent = Arc::new(IntentLog::default());
    let queued = coordinator
        .write(
            &repository,
            fixture.create("queued"),
            &token,
            queued_intent.clone(),
        )
        .unwrap();
    let (stopped, arrival) = mpsc::channel();
    let callers: Vec<_> = (0..2)
        .map(|_| {
            let coordinator = coordinator.clone();
            let stopped = stopped.clone();
            std::thread::spawn(move || {
                coordinator.shutdown();
                stopped.send(()).unwrap();
            })
        })
        .collect();
    assert_eq!(queued.join(), Err(WriteError::ShuttingDown));
    assert!(queued_intent.events.lock().unwrap().is_empty());
    assert!(arrival.recv_timeout(Duration::from_millis(100)).is_err());
    release.send(()).unwrap();
    assert_eq!(first.join().unwrap().outcome, GitWriteOutcome::Created);
    arrival.recv_timeout(WAIT).unwrap();
    arrival.recv_timeout(WAIT).unwrap();
    for caller in callers {
        caller.join().unwrap();
    }
}

struct HeldIntent {
    log: IntentLog,
    gate: Mutex<Gate>,
}

impl WriteIntentHook for HeldIntent {
    fn record(&self, intent: &PendingWriteIntent) -> Result<(), IntentError> {
        self.log.record(intent)?;
        let gate = self.gate.lock().unwrap();
        gate.entered.send(()).unwrap();
        gate.release.recv_timeout(WAIT).unwrap();
        Ok(())
    }

    fn finish(
        &self,
        intent: &PendingWriteIntent,
        outcome: GitWriteOutcome,
    ) -> Result<(), IntentError> {
        self.log.finish(intent, outcome)
    }
}

#[test]
fn write_should_revalidate_repository_identity_after_committing_intent() {
    let fixture = Fixture::new();
    let runner = Arc::new(GatedRunner::default());
    let coordinator = coordinator(&fixture, runner.clone());
    let token = CancellationToken::default();
    let repository = coordinator.repository(&fixture.0, &token).unwrap();
    let (entered, arrival) = mpsc::channel();
    let (resume, release) = mpsc::channel();
    let intent = Arc::new(HeldIntent {
        log: IntentLog::default(),
        gate: Mutex::new(Gate { entered, release }),
    });
    let ticket = coordinator
        .write(
            &repository,
            fixture.create("feature"),
            &token,
            intent.clone(),
        )
        .unwrap();
    arrival.recv_timeout(WAIT).unwrap();
    std::fs::rename(fixture.0.join(".git"), fixture.0.join(".git-old")).unwrap();
    std::fs::create_dir(fixture.0.join(".git")).unwrap();
    resume.send(()).unwrap();
    assert_eq!(
        ticket.join().unwrap().outcome,
        GitWriteOutcome::RepositoryChanged
    );
    assert_eq!(runner.writes.load(Ordering::Relaxed), 0);
    assert_eq!(*intent.log.events.lock().unwrap(), ["intent", "outcome"]);
    coordinator.shutdown();
}

#[test]
fn host_repository_status_and_worktree_writes_should_ignore_repository_hook_programs() {
    use spaceterm_host::git::{FsmonitorPolicy, ProbeOutcome};
    use std::os::unix::fs::PermissionsExt;

    let fixture = Fixture::new();
    let hooks = fixture.0.join("hooks");
    std::fs::create_dir(&hooks).unwrap();
    let script = hooks.join("post-checkout");
    std::fs::write(&script, "#!/bin/sh\nprintf unsafe > hook-ran\n").unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
    fixture.git(&["config", "core.hooksPath", hooks.to_str().unwrap()]);
    fixture.git(&["config", "core.fsmonitor", script.to_str().unwrap()]);
    let coordinator = coordinator(&fixture, Arc::new(UnixRepositoryProgramRunner::new()));
    let token = CancellationToken::default();
    let probe = coordinator.probe(&fixture.0, &token).unwrap();
    assert!(matches!(probe, ProbeOutcome::Repository(_)));
    let repository = coordinator.repository(&fixture.0, &token).unwrap();
    let ticket = coordinator
        .write(
            &repository,
            fixture.create("feature"),
            &token,
            Arc::new(IntentLog::default()),
        )
        .unwrap();
    assert_eq!(ticket.join().unwrap().outcome, GitWriteOutcome::Created);
    std::fs::write(fixture.0.join("feature/untracked"), "private contents").unwrap();
    let count = coordinator
        .count(
            &fixture.0.join("feature"),
            FsmonitorPolicy::Disabled,
            &token,
        )
        .unwrap();
    assert_eq!(count.changes.untracked, 1);
    assert!(!fixture.0.join("hook-ran").exists());
    assert!(!fixture.0.join("feature/hook-ran").exists());
    coordinator.shutdown();
}

#[test]
fn reconciliation_should_not_treat_an_unreadable_location_as_absent() {
    use spaceterm_host::git::filesystem::{DirectoryError, RetainedDirectory};
    use spaceterm_host::git::{DurableGitWrite, Reconciliation};
    use std::path::Path;

    struct UnreadableFilesystem;
    impl WorktreeFilesystem for UnreadableFilesystem {
        fn validate_directory(
            &self,
            _: &Path,
        ) -> Result<Arc<dyn RetainedDirectory>, DirectoryError> {
            Err(DirectoryError::Unavailable)
        }
        fn has_entry(&self, _: &Path) -> bool {
            false
        }
    }

    let fixture = Fixture::new();
    let coordinator = coordinator_with_filesystem(
        &fixture,
        Arc::new(UnixRepositoryProgramRunner::new()),
        Arc::new(UnreadableFilesystem),
    );
    let intent = PendingWriteIntent {
        main_root: fixture.0.clone(),
        common_directory: fixture.0.join(".git"),
        write: DurableGitWrite::Remove {
            path: fixture.0.join("inaccessible"),
            force: true,
        },
    };
    assert_eq!(
        coordinator
            .reconcile(&intent, &CancellationToken::default())
            .unwrap(),
        Reconciliation::NeedsReview
    );
    coordinator.shutdown();
}
