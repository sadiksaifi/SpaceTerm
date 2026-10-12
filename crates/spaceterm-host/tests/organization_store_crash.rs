//! Owned-process crashes at real authority pre-COMMIT and pre-receipt barriers.
//! Process termination is not physical power-loss evidence.

use spaceterm_core::storage::*;
use spaceterm_host::organization_store::{bootstrap::*, testing::organization_only_services, *};
use spaceterm_host::startup::{AccountBuildKey, BuildIdentity};
use spaceterm_host::storage::Database;
use std::io::{Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

#[expect(
    dead_code,
    reason = "The fault suite uses the remaining shared fixture helpers."
)]
#[path = "organization_store_support/mod.rs"]
mod support;

const BARRIER: &[u8] = b"STORAGE-TEST-BARRIER\n";
const CHILD_STAGE: &str = "SPACETERM_STORAGE_TEST_STAGE";
const CHILD_DIRECTORY: &str = "SPACETERM_STORAGE_TEST_DIRECTORY";
fn identity() -> StoreIdentity {
    StoreIdentity {
        server: spaceterm_core::identity::ServerId::new([11; 16]),
        account: AccountBuildKey {
            account_uid: 501,
            build_identity: BuildIdentity::Development,
        },
        server_public_key: [29; 32],
    }
}
fn barrier() -> ! {
    std::io::stdout().write_all(BARRIER).unwrap();
    std::io::stdout().flush().unwrap();
    loop {
        std::thread::sleep(Duration::from_millis(20));
    }
}
fn child(stage: &str) -> ! {
    use spaceterm_host::storage::testing::FaultPoint;
    let directory = std::path::PathBuf::from(std::env::var_os(CHILD_DIRECTORY).unwrap());
    if stage.starts_with("revoke-") {
        revocation_child(&directory, stage);
    }
    if stage.starts_with("git-") {
        git_child(&directory, stage);
    }
    if stage.starts_with("ack-") {
        acknowledgement_child(&directory, stage);
    }
    let database =
        Database::open_for_application(&directory, migrations(), schema::APPLICATION_ID).unwrap();
    let control = database.test_control();
    control.barrier(
        if stage == "before-commit" {
            FaultPoint::BeforeCommit
        } else {
            FaultPoint::AfterCommit
        },
        || barrier(),
    );
    let result = SqliteOrganizationStore::initialize_new(
        database,
        NewStoreSeed {
            identity: identity(),
            origin: NewServerOrigin::LocalDesktop {
                legacy_worktree_location: Some(
                    LegacyWorktreeLocation::parse("~/retained-seed/{branch}".into()).unwrap(),
                ),
            },
        },
        StoreLimits::production(),
        organization_only_services(),
    );
    panic!("authority barrier was not reached: {result:?}")
}

struct OwnedChild(Child);
impl Drop for OwnedChild {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline {
            if self.0.try_wait().ok().flatten().is_some() {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}
fn crash_at(test: &str, stage: &str) -> tempfile::TempDir {
    #[cfg(target_os = "macos")]
    let root = std::path::PathBuf::from("/private/tmp");
    #[cfg(target_os = "linux")]
    let root = std::path::PathBuf::from(std::env::var_os("HOME").unwrap());
    let directory = tempfile::tempdir_in(root).unwrap();
    std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let mut owned = OwnedChild(
        Command::new(std::env::current_exe().unwrap())
            .args(["--exact", test, "--nocapture"])
            .env(CHILD_STAGE, stage)
            .env(CHILD_DIRECTORY, directory.path())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let mut output = owned.0.stdout.take().unwrap();
    let (send, receive) = std::sync::mpsc::channel();
    let reader = std::thread::spawn(move || {
        // Bounded output scan; neither paths nor retained records are printed.
        let mut seen = Vec::new();
        let mut byte = [0];
        for _ in 0..65_536 {
            if output.read_exact(&mut byte).is_err() {
                break;
            }
            seen.push(byte[0]);
            if seen.ends_with(BARRIER) {
                let _ = send.send(());
                return;
            }
        }
    });
    let reached = receive.recv_timeout(Duration::from_secs(30));
    owned.0.kill().unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if owned.0.try_wait().unwrap().is_some() {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "owned fixture child did not exit"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    reader.join().unwrap();
    assert!(
        reached.is_ok(),
        "owned fixture child did not reach storage barrier"
    );
    directory
}

#[test]
fn crash_before_initialization_commit_recovers_empty_complete_inventory() {
    if let Ok(stage) = std::env::var(CHILD_STAGE) {
        child(&stage);
    }
    let directory = crash_at(
        "crash_before_initialization_commit_recovers_empty_complete_inventory",
        "before-commit",
    );
    let database = Database::open_existing_for_application(
        directory.path(),
        migrations(),
        schema::APPLICATION_ID,
    )
    .unwrap();
    for table in schema::TABLES {
        assert_eq!(
            database
                .query_row(&format!("SELECT count(*) FROM {table}"), [], |row| row
                    .get::<_, u64>(0))
                .unwrap(),
            0
        );
    }
}

#[test]
fn crash_after_initialization_commit_before_reply_recovers_complete_seed() {
    if let Ok(stage) = std::env::var(CHILD_STAGE) {
        child(&stage);
    }
    let directory = crash_at(
        "crash_after_initialization_commit_before_reply_recovers_complete_seed",
        "after-commit",
    );
    let database = Database::open_existing_for_application(
        directory.path(),
        migrations(),
        schema::APPLICATION_ID,
    )
    .unwrap();
    assert_eq!(
        database
            .query_row(
                "SELECT legacy_seed_decision FROM store_metadata",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        1
    );
    let mut store = SqliteOrganizationStore::open_existing_with_services(
        database,
        identity(),
        StoreLimits::production(),
        organization_only_services(),
    )
    .unwrap();
    let recovered = store.recover().unwrap();
    assert_eq!(recovered.store_revision.get(), 1);
    assert_eq!(
        recovered
            .organization
            .settings
            .worktree_location
            .value
            .as_str(),
        "~/retained-seed/{branch}"
    );
    assert!(recovered.organization.workspaces.is_empty());
}

fn acknowledgement_child(directory: &std::path::Path, stage: &str) -> ! {
    use spaceterm_host::storage::testing::FaultPoint;
    let mut fixture = support::Fixture::new(directory);
    fixture.admit(1);
    fixture.complete(1);
    let (prepared, transaction) = fixture.acknowledgement();
    fixture.control().barrier(
        if stage == "ack-before" {
            FaultPoint::BeforeCommit
        } else {
            FaultPoint::AfterCommit
        },
        || barrier(),
    );
    fixture.commit_proposal(prepared, transaction);
    panic!("acknowledgement barrier was not reached")
}

#[test]
fn crash_during_acknowledgement_compaction_never_reexecutes_retired_identity() {
    use spaceterm_core::operation_ledger::model::OperationLedger;
    use spaceterm_core::operations::*;
    if let Ok(stage) = std::env::var(CHILD_STAGE) {
        child(&stage);
    }
    for stage in ["ack-before", "ack-after"] {
        let directory = crash_at(
            "crash_during_acknowledgement_compaction_never_reexecutes_retired_identity",
            stage,
        );
        let mut store = support::reopen(directory.path());
        let recovered = store.recover().unwrap();
        let rows = recovered.ledger.unwrap();
        let binding = rows.streams[0].binding;
        assert_eq!(
            rows.streams[0].acknowledged_through,
            u64::from(stage == "ack-after")
        );
        let ledger = OperationLedger::recover(identity().server, rows).unwrap();
        let id = OperationId::new(binding.stream(), OperationSequence::new(1).unwrap());
        let lookup = ledger.lookup(&binding, id, None).unwrap();
        assert!(matches!(
            lookup,
            OperationLookup::Completed { .. } | OperationLookup::AcknowledgedRetired
        ));
    }
}

fn run_git(directory: &std::path::Path, arguments: &[&std::ffi::OsStr]) {
    let mut child = OwnedChild(
        Command::new("/usr/bin/git")
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .args([
                "-c",
                "core.hooksPath=/dev/null",
                "-c",
                "user.name=fixture",
                "-c",
                "user.email=fixture@invalid",
                "-C",
            ])
            .arg(directory)
            .args(arguments)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        if let Some(status) = child.0.try_wait().unwrap() {
            assert!(status.success(), "owned Git fixture command failed");
            break;
        }
        assert!(
            Instant::now() < deadline,
            "owned Git fixture command timed out"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}
fn git_child(directory: &std::path::Path, stage: &str) -> ! {
    use spaceterm_core::operation_ledger::model::TransitionResolution;
    use spaceterm_core::worktrees::intent::GitIntentPhase;
    use spaceterm_host::storage::testing::FaultPoint;
    use std::ffi::OsStr;
    let repository = directory.join("repository");
    let worktree = directory.join("worktree");
    std::fs::create_dir(&repository).unwrap();
    run_git(
        &repository,
        &[
            OsStr::new("init"),
            OsStr::new("--quiet"),
            OsStr::new("--initial-branch=main"),
        ],
    );
    run_git(
        &repository,
        &[
            OsStr::new("commit"),
            OsStr::new("--quiet"),
            OsStr::new("--allow-empty"),
            OsStr::new("--no-gpg-sign"),
            OsStr::new("-m"),
            OsStr::new("fixture"),
        ],
    );
    run_git(
        &repository,
        &[OsStr::new("branch"), OsStr::new("fault-fixture")],
    );
    let mut fixture = support::Fixture::new(directory);
    let binding = fixture.git_admit();
    fixture.persist_git_progress(binding, GitIntentPhase::Prepared);
    fixture.persist_git_progress(binding, GitIntentPhase::Started);
    assert_eq!(
        fixture.store.recover().unwrap().git_intents[0].phase(),
        GitIntentPhase::Started
    );
    // A real owning-fixture Git write occurs only after durable Started. The
    // authority barrier kills this child before recording completion or replying.
    run_git(
        &repository,
        &[
            OsStr::new("worktree"),
            OsStr::new("add"),
            OsStr::new("--quiet"),
            worktree.as_os_str(),
            OsStr::new("fault-fixture"),
        ],
    );
    if stage == "git-reconciliation-after" {
        // Discard live ledger state. The real native coordinator only observes
        // the already-Started write after complete store recovery; it executes none.
        drop(fixture);
        let mut store = support::reopen(directory);
        let ledger = spaceterm_core::operation_ledger::model::OperationLedger::recover(
            support::identity().server,
            store.recover().unwrap().ledger.unwrap(),
        )
        .unwrap();
        fixture = support::ReducedFixture::from_recovered(store, ledger, directory).fixture;
        reconcile_owned_git(directory);
        fixture.persist_git_progress(binding, GitIntentPhase::ExternalFinished);
        let (prepared, transaction) = fixture.git_reconciled_completion(binding);
        fixture
            .control()
            .barrier(FaultPoint::AfterCommit, || barrier());
        fixture.commit_proposal(prepared, transaction);
        panic!("Git reconciliation authority barrier was not reached")
    }
    let (prepared, transaction) = if stage == "git-after-execution" {
        fixture.git_progress(binding, GitIntentPhase::ExternalFinished)
    } else {
        fixture.persist_git_progress(binding, GitIntentPhase::ExternalFinished);
        fixture.git_completion(binding)
    };
    fixture.control().barrier(
        if stage == "git-completion-after" {
            FaultPoint::AfterCommit
        } else {
            FaultPoint::BeforeCommit
        },
        || barrier(),
    );
    let result = fixture.commit_proposal(prepared, transaction);
    assert!(
        !matches!(
            result,
            TransitionResolution::Completed | TransitionResolution::ExternalProgressRecorded
        ),
        "owned Git barrier was not reached"
    );
    panic!("owned Git authority barrier was not reached")
}

#[test]
fn crash_between_git_start_and_completion_preserves_reconciliation_intent() {
    use spaceterm_core::operation_ledger::model::{AdmissionPreparation, OperationLedger};
    use spaceterm_core::operation_ledger_contracts::*;
    use spaceterm_core::operations::*;
    use spaceterm_core::worktrees::intent::GitIntentPhase;
    if let Ok(stage) = std::env::var(CHILD_STAGE) {
        child(&stage);
    }
    for stage in [
        "git-after-execution",
        "git-completion-before",
        "git-completion-after",
    ] {
        let directory = crash_at(
            "crash_between_git_start_and_completion_preserves_reconciliation_intent",
            stage,
        );
        assert!(directory.path().join("worktree/.git").is_file());
        // Query only the owned Git fixture, never the live database inode.
        run_git(
            &directory.path().join("worktree"),
            &[
                std::ffi::OsStr::new("symbolic-ref"),
                std::ffi::OsStr::new("--quiet"),
                std::ffi::OsStr::new("HEAD"),
            ],
        );
        let recovered = support::reopen(directory.path()).recover().unwrap();
        let rows = recovered.ledger.clone().unwrap();
        let stream = rows.streams[0].binding;
        let operation = OperationId::new(stream.stream(), OperationSequence::new(1).unwrap());
        let mut ledger = OperationLedger::recover(support::identity().server, rows).unwrap();
        match stage {
            "git-after-execution" | "git-completion-before" => {
                assert!(matches!(
                    ledger.lookup(&stream, operation, None).unwrap(),
                    OperationLookup::Pending
                ));
                assert_eq!(recovered.git_intents.len(), 1);
                let intent = &recovered.git_intents[0];
                assert_eq!(intent.binding().id, operation);
                assert_eq!(
                    intent.phase(),
                    if stage == "git-after-execution" {
                        GitIntentPhase::Started
                    } else {
                        GitIntentPhase::ExternalFinished
                    }
                );
                assert_eq!(recovered.git_allocations[0].binding(), intent.binding());
                assert_eq!(recovered.git_repositories[0], intent.parts().repository);
                assert!(recovered.organization.worktrees.is_empty());
            }
            "git-completion-after" => {
                assert!(matches!(
                    ledger.lookup(&stream, operation, None).unwrap(),
                    OperationLookup::Completed {
                        result: Outcome::Git(_),
                        ..
                    }
                ));
                assert!(recovered.git_intents.is_empty());
                assert!(recovered.git_allocations.is_empty());
                assert_eq!(recovered.organization.worktrees.len(), 1);
            }
            _ => unreachable!(),
        }
        let context = TransactionContext::new(
            recovered.store_revision,
            recovered.organization.revision,
            recovered.organization.revision,
            None,
        )
        .unwrap();
        let command = Command::CreateWorktree {
            scope: RepositoryScope {
                server: support::identity().server,
                repository: spaceterm_core::identity::RepositoryId::new(
                    support::identity().server,
                    1,
                )
                .unwrap(),
            },
            branch: spaceterm_core::organization::BranchName::new("fault-fixture".into()).unwrap(),
        };
        assert!(matches!(
            ledger
                .prepare_admission(
                    &stream,
                    operation,
                    &command,
                    PendingKind::External {
                        effect: ExternalEffectKind::GitWrite
                    },
                    context
                )
                .unwrap(),
            AdmissionPreparation::Existing(_)
        ));
        if matches!(
            ledger.lookup(&stream, operation, None).unwrap(),
            OperationLookup::Pending
        ) {
            assert_eq!(
                ledger
                    .resume_recovered_atomic(&stream, operation, &command)
                    .err()
                    .unwrap()
                    .class,
                LedgerFailureClass::InvalidTransition,
            );
        }
    }
}

fn revocation_child(directory: &std::path::Path, stage: &str) -> ! {
    use spaceterm_core::{operation_ledger_contracts::PendingKind, operations::Command};
    use spaceterm_host::storage::testing::FaultPoint;
    let mut fixture = support::Fixture::new(directory);
    let stream = fixture.restricted();
    let operation = fixture.admit_command(
        stream,
        &Command::RevokePairing {
            pairing: stream.pairing(),
        },
        PendingKind::AtomicMutation,
    );
    let (prepared, proposal, _) = fixture.revocation(operation);
    fixture.control().barrier(
        if stage == "revoke-before" {
            FaultPoint::BeforeCommit
        } else {
            FaultPoint::AfterCommit
        },
        || barrier(),
    );
    fixture.commit_proposal(prepared, proposal);
    panic!("revocation authority barrier was not reached")
}
#[test]
fn crash_during_pairing_revocation_keeps_signed_receipt_and_original_outcome_atomic() {
    use spaceterm_core::{operation_ledger::model::OperationLedger, operations::*, pairing::*};
    if let Ok(stage) = std::env::var(CHILD_STAGE) {
        child(&stage);
    }
    for stage in ["revoke-before", "revoke-after"] {
        let directory = crash_at(
            "crash_during_pairing_revocation_keeps_signed_receipt_and_original_outcome_atomic",
            stage,
        );
        let recovered = support::reopen(directory.path()).recover().unwrap();
        let authorization = recovered.pairing.as_ref().unwrap().parts().authorizations[0].parts();
        let records = recovered.ledger.unwrap();
        let stream = records
            .streams
            .iter()
            .find(|v| v.binding.stream().key().local() == 2)
            .unwrap()
            .binding;
        let ledger = OperationLedger::recover(support::identity().server, records).unwrap();
        let result = ledger
            .lookup(
                &stream,
                OperationId::new(stream.stream(), OperationSequence::new(1).unwrap()),
                None,
            )
            .unwrap();
        if stage == "revoke-after" {
            assert!(matches!(
                authorization.record.state,
                PairingState::Revoked { .. }
            ));
            let proof = authorization.revocation_receipt.as_ref().unwrap();
            assert!(
                matches!(result, OperationLookup::Completed { result: Outcome::Pairing(v), .. } if *v == PairingOutcome::Revoked(proof.clone()))
            );
        } else {
            assert_eq!(authorization.record.state, PairingState::Authorized);
            assert!(authorization.revocation_receipt.is_none());
            assert!(matches!(result, OperationLookup::Pending));
        }
    }
}

fn reconcile_owned_git(directory: &std::path::Path) {
    use spaceterm_host::git::*;
    use std::sync::Arc;
    let runner: Arc<dyn RepositoryProgramRunner> = Arc::new(UnixRepositoryProgramRunner::new());
    let version = tools::parse_git_version(
        &Command::new("/usr/bin/git")
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
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
            directory.to_path_buf(),
            version,
        )),
        Arc::new(LocalRepositoryReader::new(
            runner,
            Arc::new(UnixRepositoryMarkerReader::new()),
            "/usr/bin/git".into(),
            directory.to_path_buf(),
            version,
        )),
        Arc::new(filesystem::NativeWorktreeFilesystem),
    );
    let observed = coordinator
        .reconcile(
            &PendingWriteIntent {
                main_root: directory.join("repository"),
                common_directory: directory.join("repository/.git"),
                write: DurableGitWrite::Create {
                    path: directory.join("worktree"),
                    branch: WorktreeBranch::Existing {
                        name: "fault-fixture".into(),
                    },
                },
            },
            &CancellationToken::default(),
        )
        .unwrap();
    assert_eq!(observed, Reconciliation::ObservedApplied);
    coordinator.shutdown();
}
#[test]
fn crash_after_started_git_reconciliation_before_reply_recovers_original_outcome() {
    use spaceterm_core::{operation_ledger::model::OperationLedger, operations::*, worktrees::*};
    if let Ok(stage) = std::env::var(CHILD_STAGE) {
        child(&stage);
    }
    let directory = crash_at(
        "crash_after_started_git_reconciliation_before_reply_recovers_original_outcome",
        "git-reconciliation-after",
    );
    reconcile_owned_git(directory.path());
    let recovered = support::reopen(directory.path()).recover().unwrap();
    assert!(recovered.git_intents.is_empty() && recovered.git_allocations.is_empty());
    let rows = recovered.ledger.unwrap();
    let stream = rows.streams[0].binding;
    let ledger = OperationLedger::recover(support::identity().server, rows).unwrap();
    assert!(
        matches!(ledger.lookup(&stream, OperationId::new(stream.stream(), OperationSequence::new(1).unwrap()), None).unwrap(), OperationLookup::Completed { result: Outcome::Git(v), .. } if matches!(v.parts(), GitMutationOutcomeParts::Recovered { observation: RecoveryObservation::DesiredStateObserved, .. }))
    );
}
