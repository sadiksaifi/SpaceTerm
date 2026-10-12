//! Real VFS uncertainty and original-identity recovery through the public store.
//! Ledger and auxiliary rows use the real core validators and SQL helpers.
//! The absent pairing receipt provider is injected only in test composition.

use spaceterm_core::identity::*;
use spaceterm_core::operation_ledger::model::*;
use spaceterm_core::operation_ledger_contracts::*;
use spaceterm_core::operations::*;
use spaceterm_core::organization::*;
use spaceterm_core::storage::*;
use spaceterm_host::storage::testing::FaultPoint;

#[expect(
    dead_code,
    reason = "The composition suite uses the shared reducer and enrollment helpers."
)]
#[path = "organization_store_support/mod.rs"]
mod support;
use support::*;

#[test]
fn dropped_acknowledgement_recovers_real_store_rows_and_restores_writes() {
    let directory = private_directory();
    let mut fixture = Fixture::new(directory.path());
    fixture.admit(1);
    fixture.complete(1);
    let context = fixture.context();
    let prepared = fixture
        .ledger
        .prepare_acknowledgement(&fixture.binding, 1, context)
        .unwrap()
        .unwrap();
    let transaction = fixture.transaction(&prepared, false);
    assert!(matches!(
        fixture.store.commit(&transaction),
        CommitDisposition::Durable(_)
    ));
    drop(prepared); // No resolve_commit call and no Indeterminate disposition.
    let rows = fixture.store.recover().unwrap();
    fixture
        .ledger
        .install_recovery(rows.ledger.unwrap())
        .unwrap();
    assert!(!fixture.ledger.requires_recovery());
    assert_eq!(
        fixture
            .ledger
            .lookup(&fixture.binding, fixture.operation(1), None)
            .unwrap(),
        OperationLookup::AcknowledgedRetired
    );
    fixture.admit(2);
}

#[test]
fn vfs_write_or_sync_failure_is_indeterminate_and_latches_recovery_required() {
    for point in [
        FaultPoint::Write,
        FaultPoint::DatabaseWrite,
        FaultPoint::Sync,
        FaultPoint::DatabaseSync,
        FaultPoint::AfterCommit,
    ] {
        let directory = private_directory();
        let mut fixture = Fixture::new(directory.path());
        let prepared = fixture.prepare_admission(1);
        let transaction = fixture.transaction(&prepared, false);
        let control = fixture.control();
        control.fail_once(point, 0);
        assert_eq!(
            fixture.store.commit(&transaction),
            CommitDisposition::Indeterminate
        );
        assert!(!control.fault_pending());
        assert!(matches!(
            fixture
                .ledger
                .resolve_commit(prepared, CommitDisposition::Indeterminate)
                .unwrap(),
            TransitionResolution::RecoveryRequired
        ));
        assert_eq!(fixture.store.recover(), Err(StoreFailure::RecoveryRequired));
    }
}

#[test]
fn indeterminate_rejects_later_commit_without_sql_or_external_effect() {
    let directory = private_directory();
    let mut fixture = Fixture::new(directory.path());
    let prepared = fixture.prepare_admission(1);
    let transaction = fixture.transaction(&prepared, false);
    let control = fixture.control();
    control.fail_once(FaultPoint::Sync, 0);
    assert_eq!(
        fixture.store.commit(&transaction),
        CommitDisposition::Indeterminate
    );
    let calls = control.sql_calls();
    let mut effects = 0;
    for _ in 0..3 {
        let result = fixture.store.commit(&transaction);
        if matches!(result, CommitDisposition::Durable(_)) {
            effects += 1;
        }
        assert_eq!(
            result,
            CommitDisposition::NotApplied(StoreFailure::RecoveryRequired)
        );
    }
    assert_eq!(control.sql_calls(), calls);
    assert_eq!(effects, 0);
}

#[test]
fn successful_repeat_sync_does_not_clear_indeterminate() {
    let directory = private_directory();
    let mut fixture = Fixture::new(directory.path());
    let prepared = fixture.prepare_admission(1);
    let transaction = fixture.transaction(&prepared, false);
    let control = fixture.control();
    control.fail_once(FaultPoint::Sync, 0);
    assert_eq!(
        fixture.store.commit(&transaction),
        CommitDisposition::Indeterminate
    );
    control.repeat_sync().unwrap();
    assert_eq!(fixture.store.recover(), Err(StoreFailure::RecoveryRequired));
    assert_eq!(
        fixture.store.commit(&transaction),
        CommitDisposition::NotApplied(StoreFailure::RecoveryRequired)
    );
}

#[test]
fn reopen_resolves_original_operation_identity_from_validated_state() {
    for point in [
        FaultPoint::Write,
        FaultPoint::DatabaseWrite,
        FaultPoint::Sync,
        FaultPoint::DatabaseSync,
        FaultPoint::AfterCommit,
    ] {
        let directory = private_directory();
        let mut fixture = Fixture::new(directory.path());
        fixture.admit(1);
        let prepared = fixture.completion(1, false);
        let transaction = fixture.transaction(&prepared, false);
        fixture.control().fail_once(point, 0);
        assert_eq!(
            fixture.store.commit(&transaction),
            CommitDisposition::Indeterminate
        );
        fixture
            .ledger
            .resolve_commit(prepared, CommitDisposition::Indeterminate)
            .unwrap();
        let Fixture {
            store,
            mut ledger,
            binding,
            ..
        } = fixture;
        drop(store);
        let mut store = reopen(directory.path());
        ledger
            .install_recovery(store.recover().unwrap().ledger.unwrap())
            .unwrap();
        assert!(!ledger.requires_recovery());
        let id = OperationId::new(binding.stream(), OperationSequence::new(1).unwrap());
        assert!(matches!(
            ledger.lookup(&binding, id, None).unwrap(),
            OperationLookup::Pending | OperationLookup::Completed { .. }
        ));
    }
}

#[test]
fn faults_cannot_expose_metadata_without_its_outcome_or_outcome_without_metadata() {
    for point in [
        FaultPoint::Write,
        FaultPoint::DatabaseWrite,
        FaultPoint::Sync,
        FaultPoint::DatabaseSync,
        FaultPoint::AfterCommit,
    ] {
        let directory = private_directory();
        let mut fixture = Fixture::new(directory.path());
        fixture.admit(1);
        let before = fixture.store.recover().unwrap();
        let prepared = fixture.completion(1, true);
        let transaction = fixture.transaction(&prepared, true);
        fixture.control().fail_once(point, 0);
        assert_eq!(
            fixture.store.commit(&transaction),
            CommitDisposition::Indeterminate
        );
        let Fixture { store, .. } = fixture;
        drop(store);
        let recovered = reopen(directory.path()).recover().unwrap();
        let completed = matches!(
            &recovered.ledger.as_ref().unwrap().operations[0],
            RecoveredOperationRecord::Completed(_)
        );
        assert_eq!(
            recovered
                .organization
                .settings
                .worktree_location
                .value
                .as_str()
                == "~/sensitive-sentinel/{branch}",
            completed
        );
        assert_eq!(
            recovered.organization.revision != before.organization.revision,
            completed
        );
    }
}

#[test]
fn failure_before_commit_with_confirmed_rollback_is_not_applied() {
    let directory = private_directory();
    let mut fixture = Fixture::new(directory.path());
    let prepared = fixture.prepare_admission(1);
    let transaction = fixture.transaction(&prepared, false);
    let before = fixture.store.recover().unwrap();
    fixture.control().fail_once(FaultPoint::BeforeCommit, 0);
    assert_eq!(
        fixture.store.commit(&transaction),
        CommitDisposition::NotApplied(StoreFailure::Io)
    );
    assert_eq!(fixture.store.recover().unwrap(), before);
}

#[test]
fn failure_metadata_redacts_all_sensitive_sentinels() {
    let directory = private_directory();
    let mut fixture = Fixture::new(directory.path());
    let prepared = fixture.prepare_admission(1);
    let transaction = fixture.transaction(&prepared, false);
    fixture.control().fail_once(FaultPoint::Sync, 0);
    let disposition = fixture.store.commit(&transaction);
    let debug = format!("{disposition:?} {:?} {:?}", fixture.store, fixture.ledger);
    assert!(!debug.contains("sensitive-sentinel"));
    assert!(!debug.contains(directory.path().to_str().unwrap()));
    assert_eq!(disposition, CommitDisposition::Indeterminate);
}

use spaceterm_core::pairing::{PairingOutcome, PairingState};
use spaceterm_core::worktrees::intent::GitIntentPhase;

const COMMIT_FAULTS: &[FaultPoint] = &[
    FaultPoint::BeforeCommit,
    FaultPoint::Write,
    FaultPoint::Sync,
    FaultPoint::DatabaseWrite,
    FaultPoint::DatabaseSync,
    FaultPoint::DirectorySync,
    #[cfg(target_os = "macos")]
    FaultPoint::DeleteFullSync,
    FaultPoint::AfterCommit,
];

fn fail_commit(
    fixture: &mut Fixture,
    prepared: PreparedLedgerTransition,
    transaction: DurableTransaction,
    point: FaultPoint,
) {
    let control = fixture.control();
    control.fail_once(point, 0);
    let disposition = fixture.store.commit(&transaction);
    assert!(!control.fault_pending(), "fault was not reached");
    if point == FaultPoint::BeforeCommit {
        assert_eq!(disposition, CommitDisposition::NotApplied(StoreFailure::Io));
        assert!(matches!(
            fixture
                .ledger
                .resolve_commit(prepared, disposition)
                .unwrap(),
            TransitionResolution::NotApplied(StoreFailure::Io)
        ));
    } else {
        assert_eq!(disposition, CommitDisposition::Indeterminate);
        assert!(matches!(
            fixture
                .ledger
                .resolve_commit(prepared, disposition)
                .unwrap(),
            TransitionResolution::RecoveryRequired
        ));
        let calls = control.sql_calls();
        assert_eq!(
            fixture.store.commit(&transaction),
            CommitDisposition::NotApplied(StoreFailure::RecoveryRequired)
        );
        assert_eq!(fixture.store.recover(), Err(StoreFailure::RecoveryRequired));
        assert_eq!(control.sql_calls(), calls);
        control.repeat_sync().unwrap();
        assert_eq!(fixture.store.recover(), Err(StoreFailure::RecoveryRequired));
        assert_eq!(control.sql_calls(), calls);
    }
}

#[test]
fn acknowledgement_compaction_faults_never_reexecute_original_identity() {
    for &point in COMMIT_FAULTS {
        let directory = private_directory();
        let mut fixture = Fixture::new(directory.path());
        fixture.admit(1);
        fixture.complete(1);
        let before = fixture.store.recover().unwrap();
        let (prepared, transaction) = fixture.acknowledgement();
        fail_commit(&mut fixture, prepared, transaction, point);
        let Fixture {
            store,
            mut ledger,
            binding,
            ..
        } = fixture;
        drop(store);
        let mut store = reopen(directory.path());
        let recovered = store.recover().unwrap();
        assert_eq!(recovered.organization, before.organization);
        let rows = recovered.ledger.as_ref().unwrap();
        assert_eq!(
            rows.operations.len() as u64,
            1 - rows.streams[0].acknowledged_through
        );
        if point == FaultPoint::AfterCommit {
            assert_eq!(rows.streams[0].acknowledged_through, 1);
        }
        if point == FaultPoint::BeforeCommit {
            assert_eq!(recovered, before);
        }
        ledger.install_recovery(recovered.ledger.unwrap()).unwrap();
        let context = TransactionContext::new(
            recovered.store_revision,
            recovered.organization.revision,
            recovered.organization.revision,
            None,
        )
        .unwrap();
        let id = OperationId::new(binding.stream(), OperationSequence::new(1).unwrap());
        let command = Command::SetWorktreeLocation {
            server: identity().server,
            expected_version: spaceterm_core::authority::FieldVersion::new(1),
            template: WorktreeLocationTemplate::new("~/sensitive-sentinel/{branch}".into())
                .unwrap(),
        };
        assert!(matches!(
            ledger
                .prepare_admission(&binding, id, &command, PendingKind::AtomicMutation, context)
                .unwrap(),
            AdmissionPreparation::Existing(
                OperationLookup::Completed { .. } | OperationLookup::AcknowledgedRetired
            )
        ));
    }
}

#[test]
fn signed_revocation_faults_keep_authorization_receipt_and_outcome_atomic() {
    for &point in COMMIT_FAULTS {
        let directory = private_directory();
        let mut fixture = Fixture::new(directory.path());
        let restricted = fixture.restricted();
        let command = Command::RevokePairing {
            pairing: fixture.binding.pairing(),
        };
        let binding = fixture.admit_command(restricted, &command, PendingKind::AtomicMutation);
        let before = fixture.store.recover().unwrap();
        let (prepared, transaction, proof) = fixture.revocation(binding);
        fail_commit(&mut fixture, prepared, transaction, point);
        let Fixture {
            store, mut ledger, ..
        } = fixture;
        drop(store);
        let recovered = reopen(directory.path()).recover().unwrap();
        if point == FaultPoint::BeforeCommit {
            assert_eq!(recovered, before);
        }
        ledger
            .install_recovery(recovered.ledger.clone().unwrap())
            .unwrap();
        let authorization = &recovered.pairing.as_ref().unwrap().parts().authorizations[0];
        match ledger
            .lookup(&restricted, binding.id, Some(binding.digest))
            .unwrap()
        {
            OperationLookup::Completed {
                result: outcome, ..
            } => {
                assert!(matches!(
                    authorization.parts().record.state,
                    PairingState::Revoked { .. }
                ));
                assert_eq!(
                    authorization.parts().revocation_receipt.as_ref(),
                    Some(&proof)
                );
                assert_eq!(
                    outcome,
                    Outcome::Pairing(Box::new(PairingOutcome::Revoked(proof)))
                );
                assert!(
                    recovered
                        .ledger
                        .as_ref()
                        .unwrap()
                        .streams
                        .iter()
                        .all(|s| s.status
                            == StreamStatus::Retired(StreamRetirementReason::PairingRevoked))
                );
            }
            OperationLookup::Pending => {
                assert_eq!(authorization.parts().record.state, PairingState::Authorized);
                assert!(authorization.parts().revocation_receipt.is_none());
                assert!(
                    recovered
                        .ledger
                        .as_ref()
                        .unwrap()
                        .streams
                        .iter()
                        .all(|s| s.status == StreamStatus::Active)
                );
                assert_ne!(point, FaultPoint::AfterCommit);
            }
            _ => panic!("revocation identity was lost"),
        }
        let context = TransactionContext::new(
            recovered.store_revision,
            recovered.organization.revision,
            recovered.organization.revision,
            None,
        )
        .unwrap();
        assert!(matches!(
            ledger
                .prepare_admission(
                    &restricted,
                    binding.id,
                    &command,
                    PendingKind::AtomicMutation,
                    context
                )
                .unwrap(),
            AdmissionPreparation::Existing(_)
        ));
    }
}

#[test]
fn git_progress_and_final_outcome_faults_preserve_original_identity() {
    for stage in 0..4 {
        for &point in COMMIT_FAULTS {
            let directory = private_directory();
            let mut fixture = Fixture::new(directory.path());
            let binding = fixture.git_admit();
            for phase in [
                GitIntentPhase::Prepared,
                GitIntentPhase::Started,
                GitIntentPhase::ExternalFinished,
            ]
            .into_iter()
            .take(stage)
            {
                fixture.persist_git_progress(binding, phase);
            }
            let before = fixture.store.recover().unwrap();
            let (prepared, transaction) = match stage {
                0 => fixture.git_progress(binding, GitIntentPhase::Prepared),
                1 => fixture.git_progress(binding, GitIntentPhase::Started),
                2 => fixture.git_progress(binding, GitIntentPhase::ExternalFinished),
                3 => fixture.git_completion(binding),
                _ => unreachable!(),
            };
            fail_commit(&mut fixture, prepared, transaction, point);
            let Fixture {
                store,
                mut ledger,
                binding: stream,
                ..
            } = fixture;
            drop(store);
            let recovered = reopen(directory.path()).recover().unwrap();
            if point == FaultPoint::BeforeCommit {
                assert_eq!(recovered, before);
            }
            ledger
                .install_recovery(recovered.ledger.clone().unwrap())
                .unwrap();
            match ledger
                .lookup(&stream, binding.id, Some(binding.digest))
                .unwrap()
            {
                OperationLookup::Completed {
                    result: outcome,
                    publication_revision: revision,
                } => {
                    assert_eq!(stage, 3);
                    assert!(matches!(outcome, Outcome::Git(_)));
                    assert_eq!(revision, recovered.organization.revision);
                    assert!(recovered.git_intents.is_empty());
                    assert!(recovered.git_allocations.is_empty());
                    assert_eq!(recovered.organization.repositories.len(), 1);
                    assert_eq!(recovered.organization.worktrees.len(), 1);
                }
                OperationLookup::Pending => {
                    assert!(recovered.organization.repositories.is_empty());
                    assert!(recovered.organization.worktrees.is_empty());
                    if stage == 0 && recovered.git_intents.is_empty() {
                        assert!(recovered.git_allocations.is_empty());
                        assert!(recovered.git_repositories.is_empty());
                    } else {
                        assert_eq!(recovered.git_intents.len(), 1);
                        assert_eq!(recovered.git_allocations.len(), 1);
                        assert_eq!(recovered.git_repositories.len(), 1);
                        let intent = &recovered.git_intents[0];
                        assert_eq!(intent.binding(), binding);
                        let RecoveredOperationRecord::Pending(record) =
                            &recovered.ledger.as_ref().unwrap().operations[0]
                        else {
                            panic!("expected pending Git")
                        };
                        let ExternalProgressState::RecordedIntent { reference, .. } =
                            record.external_progress.as_ref().unwrap().state()
                        else {
                            panic!("Git progress was lost")
                        };
                        let phase = match intent.phase() {
                            GitIntentPhase::Prepared => ExternalIntentPhase::Prepared,
                            GitIntentPhase::Started => ExternalIntentPhase::Started,
                            GitIntentPhase::ExternalFinished => {
                                ExternalIntentPhase::ExternalFinished
                            }
                        };
                        assert_eq!(reference.phase(), phase);
                        if point == FaultPoint::AfterCommit && stage < 3 {
                            assert_eq!(
                                intent.phase(),
                                [
                                    GitIntentPhase::Prepared,
                                    GitIntentPhase::Started,
                                    GitIntentPhase::ExternalFinished
                                ][stage]
                            );
                        }
                    }
                    assert!(!(point == FaultPoint::AfterCommit && stage == 3));
                }
                _ => panic!("Git identity was lost"),
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
                    server: identity().server,
                    repository: RepositoryId::new(identity().server, 1).unwrap(),
                },
                branch: BranchName::new("fault-fixture".into()).unwrap(),
            };
            assert!(matches!(
                ledger
                    .prepare_admission(
                        &stream,
                        binding.id,
                        &command,
                        PendingKind::External {
                            effect: ExternalEffectKind::GitWrite
                        },
                        context
                    )
                    .unwrap(),
                AdmissionPreparation::Existing(_)
            ));
        }
    }
}

#[test]
fn statement_failure_with_confirmed_rollback_leaves_all_tables_unchanged() {
    let directory = private_directory();
    let mut fixture = Fixture::new(directory.path());
    fixture.admit(1);
    let before = fixture.store.recover().unwrap();
    let prepared = fixture.completion(1, true);
    let organization = fixture
        .transaction(&prepared, true)
        .parts()
        .organization_writes
        .clone();
    let binding = OperationBinding {
        id: fixture.operation(1),
        digest: spaceterm_core::operation_ledger::canonical::canonical_request_digest(
            &fixture.binding,
            fixture.operation(1),
            &fixture.command(),
        )
        .unwrap(),
    };
    // Ledger and Settings SQL execute first. This missing intent's checked CAS
    // then rejects, so confirmed rollback must undo both completed projections.
    let transaction = fixture.proposal(
        &prepared,
        vec![OperationWrite::GitIntent(
            spaceterm_core::worktrees::intent::GitIntentMutation::Complete { binding },
        )],
        organization,
        None,
    );
    assert_eq!(
        fixture.store.commit(&transaction),
        CommitDisposition::NotApplied(StoreFailure::InvalidTransaction)
    );
    assert_eq!(fixture.store.recover().unwrap(), before);
}

#[test]
fn initialization_faults_never_create_partial_identity_or_seed_state() {
    use spaceterm_host::organization_store::{bootstrap::*, *};
    use spaceterm_host::storage::Database;
    for &point in COMMIT_FAULTS {
        let directory = private_directory();
        let database =
            Database::open_for_application(directory.path(), migrations(), schema::APPLICATION_ID)
                .unwrap();
        let control = database.test_control();
        control.fail_once(point, 0);
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
            services(),
        );
        assert!(!control.fault_pending());
        if point == FaultPoint::BeforeCommit {
            assert!(matches!(
                result,
                InitializationDisposition::NotApplied(StoreFailure::Io)
            ));
        } else {
            assert!(matches!(result, InitializationDisposition::Indeterminate));
        }
        let database = Database::open_existing_for_application(
            directory.path(),
            migrations(),
            schema::APPLICATION_ID,
        )
        .unwrap();
        let initialized = database
            .query_row("SELECT count(*) FROM store_metadata", [], |r| {
                r.get::<_, u64>(0)
            })
            .unwrap();
        assert!(initialized <= 1);
        if point == FaultPoint::AfterCommit {
            assert_eq!(initialized, 1);
        }
        if point == FaultPoint::BeforeCommit {
            assert_eq!(initialized, 0);
        }
        if initialized == 1 {
            let mut store = SqliteOrganizationStore::open_existing(
                database,
                identity(),
                StoreLimits::production(),
                services(),
            )
            .unwrap();
            let recovered = store.recover().unwrap();
            assert_eq!(recovered.organization.server, identity().server);
            assert_eq!(
                recovered
                    .organization
                    .settings
                    .worktree_location
                    .value
                    .as_str(),
                "~/retained-seed/{branch}"
            );
        } else {
            for table in schema::TABLES {
                assert_eq!(
                    database
                        .query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r
                            .get::<_, u64>(0))
                        .unwrap(),
                    0
                );
            }
            assert!(matches!(
                SqliteOrganizationStore::initialize_new(
                    database,
                    NewStoreSeed {
                        identity: identity(),
                        origin: NewServerOrigin::LocalHeadless
                    },
                    StoreLimits::production(),
                    services()
                ),
                InitializationDisposition::NotApplied(StoreFailure::RecoveryRequired)
            ));
        }
    }
}
