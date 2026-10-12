//! Real SQL auxiliary tests with explicit fakes of frozen domain ports. These do
//! not establish ledger admission, pending-progress agreement or effect permits.

use spaceterm_core::authority::*;
use spaceterm_core::identity::*;
use spaceterm_core::operation_ledger_contracts::*;
use spaceterm_core::operations::*;
use spaceterm_core::organization::*;
use spaceterm_core::organization_retained::*;
use spaceterm_core::pairing::*;
use spaceterm_core::storage::*;
use spaceterm_core::worktrees::intent::*;
use spaceterm_host::organization_store::{
    auxiliary, bootstrap::*, limits::*, pairing::PairingReceiptReader, *,
};
use spaceterm_host::startup::{AccountBuildKey, BuildIdentity};
use spaceterm_host::storage::{Database, Transaction};
use std::os::unix::fs::PermissionsExt;

struct NoReceipts;
impl PairingReceiptValidator for NoReceipts {
    fn validate_receipt(&self, _: &SignedPairingReceipt) -> Result<(), PairingContractError> {
        Err(PairingContractError::Invalid)
    }
}
impl PairingReceiptReader for NoReceipts {
    fn read_receipt(
        &self,
        _: u16,
        _: &[u8],
        _: &[u8],
    ) -> Result<SignedPairingReceipt, StoreFailure> {
        Err(StoreFailure::InvalidDurableState)
    }
}
struct SqlDomainFake;
impl DurabilityValidator for SqlDomainFake {
    fn validate_transaction_projection(&self, _: &TransactionParts) -> Result<(), StoreFailure> {
        Ok(())
    }
    fn validate_recovery(&self, recovered: &RecoveredState) -> Result<(), StoreFailure> {
        recovered
            .organization
            .retained
            .as_ref()
            .ok_or(StoreFailure::InvalidDurableState)?
            .validate_against(&recovered.organization)
            .map_err(|_| StoreFailure::InvalidDurableState)
    }
}
impl OperationProjection for SqlDomainFake {
    fn preflight(
        &self,
        writes: &[OperationWrite],
        budget: &mut SizeBudget,
    ) -> Result<usize, StoreFailure> {
        auxiliary::preflight(writes, budget)
    }
    fn apply(
        &self,
        sql: &Transaction<'_>,
        parts: &TransactionParts,
        previous: &RecoveredState,
    ) -> Result<usize, StoreFailure> {
        auxiliary::apply(sql, parts, previous, &NoReceipts)
    }
    fn recover(
        &self,
        sql: &Transaction<'_>,
        recovered: &mut RecoveredState,
        limits: &StoreLimits,
        budget: &mut SizeBudget,
    ) -> Result<(), StoreFailure> {
        auxiliary::recover(sql, recovered, limits, budget, &NoReceipts)
    }
}
fn services() -> StoreServices {
    StoreServices::new(Box::new(SqlDomainFake), Box::new(SqlDomainFake))
}
fn identity() -> StoreIdentity {
    StoreIdentity {
        server: ServerId::new([11; 16]),
        account: AccountBuildKey {
            account_uid: 501,
            build_identity: BuildIdentity::Development,
        },
        server_public_key: [29; 32],
    }
}
fn server() -> ServerId {
    identity().server
}
fn operation() -> OperationBinding {
    let stream = OperationStreamId::new(
        OperationStreamKey::new(server(), 1).unwrap(),
        PairingId::new(server(), 1).unwrap(),
        ClientId::new(server(), 1).unwrap(),
    )
    .unwrap();
    OperationBinding {
        id: OperationId::new(stream, OperationSequence::new(1).unwrap()),
        digest: RequestDigest::from_bytes([7; 32]),
    }
}
fn fixture() -> (tempfile::TempDir, SqliteOrganizationStore) {
    #[cfg(target_os = "macos")]
    let root = std::path::PathBuf::from("/private/tmp");
    #[cfg(target_os = "linux")]
    let root = std::path::PathBuf::from(std::env::var_os("HOME").unwrap());
    let directory = tempfile::tempdir_in(root).unwrap();
    std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let database = Database::open(directory.path(), migrations()).unwrap();
    let store = match SqliteOrganizationStore::initialize_test_fixture(
        database,
        NewStoreSeed {
            identity: identity(),
            origin: NewServerOrigin::LocalHeadless,
        },
        StoreLimits::production(),
        services(),
    ) {
        InitializationDisposition::Durable { store, .. } => store,
        _ => panic!("fixture initialization failed"),
    };
    drop(store);
    let mut database = Database::open_existing(directory.path(), migrations()).unwrap();
    // Stream headers are fixture inputs, not admission policy. No pending entry or
    // executable permit is fabricated. Self-revocation capacity stays dedicated.
    assert_eq!(database.transaction(|sql| {
        for (key, purpose) in [(1_u64, 0), (2_u64, 1)] {
            let zero = 0_u64.to_be_bytes();
            let one = 1_u64.to_be_bytes();
            sql.execute("INSERT INTO operation_streams VALUES (?1, ?2, ?2, ?3, NULL, 0, NULL, ?4, ?4, ?5, ?6, 1, 1, 1)", rusqlite::params![key.to_be_bytes().as_slice(), one.as_slice(), purpose, zero.as_slice(), if purpose == 1 { Some(2) } else { None }, if purpose == 1 { Some(65536_u64.to_be_bytes().to_vec()) } else { None }])?;
        }
        Ok(())
    }), spaceterm_host::storage::CommitDisposition::Durable(()));
    let mut store = SqliteOrganizationStore::from_test_database(
        database,
        identity(),
        StoreLimits::production(),
        services(),
    )
    .unwrap();
    store.recover().unwrap();
    (directory, store)
}
fn proposal(
    state: &RecoveredState,
    marks: IdentityHighWater,
    writes: Vec<OperationWrite>,
) -> DurableTransaction {
    DurableTransaction::new_with_validation(
        TransactionParts {
            server: server(),
            expected_store_revision: state.store_revision,
            resulting_store_revision: state.store_revision.checked_successor().unwrap(),
            expected_organization_revision: state.organization.revision,
            resulting_organization_revision: state.organization.revision,
            expected_epoch: state.last_epoch,
            resulting_epoch: state.last_epoch,
            expected_high_water: state.high_water,
            resulting_high_water: marks,
            organization_writes: vec![],
            operation_writes: writes,
        },
        &SqlDomainFake,
    )
    .unwrap()
}
fn authorization(id: u64, client: u64) -> PairingAuthorization {
    let pairing = PairingId::new(server(), id).unwrap();
    let client = ClientId::new(server(), client).unwrap();
    PairingAuthorization::new(PairingAuthorizationParts {
        record: PairingRecord {
            id: pairing,
            device: DeviceId::new([3; 16]),
            state: PairingState::Authorized,
        },
        authorization_version: FieldVersion::new(1),
        keys: PairingKeys::new(PairingKeysParts {
            device_identity_public_key: [4; 32],
            pairing_authentication_public_key: [5; 32],
        })
        .unwrap(),
        clients: vec![
            PairingClientStreams::new(PairingClientStreamsParts {
                client: ClientRecord {
                    id: client,
                    pairing,
                },
                ordinary: OperationStreamId::new(
                    OperationStreamKey::new(server(), 1).unwrap(),
                    pairing,
                    client,
                )
                .unwrap(),
                self_revocation: OperationStreamId::new(
                    OperationStreamKey::new(server(), 2).unwrap(),
                    pairing,
                    client,
                )
                .unwrap(),
            })
            .unwrap(),
        ],
        revocation_receipt: None,
    })
    .unwrap()
}
fn pairing_write(value: PairingAuthorization, version: Option<FieldVersion>) -> OperationWrite {
    OperationWrite::Pairing(Box::new(PairingWrite::PutAuthorization {
        expected_authorization_version: version,
        value,
    }))
}
fn marks() -> IdentityHighWater {
    IdentityHighWater {
        pairing: 1,
        client: 1,
        operation_stream: 2,
        ..IdentityHighWater::default()
    }
}
fn open(directory: &tempfile::TempDir) -> SqliteOrganizationStore {
    SqliteOrganizationStore::from_test_database(
        Database::open_existing(directory.path(), migrations()).unwrap(),
        identity(),
        StoreLimits::production(),
        services(),
    )
    .unwrap()
}

#[test]
fn pairing_projection_preserves_both_keys_and_exact_client_streams_on_reopen() {
    let (directory, mut store) = fixture();
    let before = store.recover().unwrap();
    let value = authorization(1, 1);
    assert!(matches!(
        store.commit(&proposal(
            &before,
            marks(),
            vec![pairing_write(value.clone(), None)]
        )),
        CommitDisposition::Durable(_)
    ));
    drop(store);
    let recovered = open(&directory).recover().unwrap();
    assert_eq!(
        recovered.pairing.unwrap().parts().authorizations,
        vec![value]
    );
}

#[test]
fn client_binding_cannot_move_to_a_later_pairing() {
    let (_directory, mut store) = fixture();
    let before = store.recover().unwrap();
    store.commit(&proposal(
        &before,
        marks(),
        vec![pairing_write(authorization(1, 1), None)],
    ));
    let before = store.recover().unwrap();
    let next = IdentityHighWater {
        pairing: 2,
        ..marks()
    };
    assert_eq!(
        store.commit(&proposal(
            &before,
            next,
            vec![pairing_write(authorization(2, 1), None)]
        )),
        CommitDisposition::NotApplied(StoreFailure::InvalidTransaction)
    );
    assert_eq!(store.recover().unwrap(), before);
}

#[test]
fn pairing_keys_and_existing_client_bindings_are_immutable() {
    let (_directory, mut store) = fixture();
    let before = store.recover().unwrap();
    let value = authorization(1, 1);
    store.commit(&proposal(
        &before,
        marks(),
        vec![pairing_write(value.clone(), None)],
    ));
    let before = store.recover().unwrap();
    let mut changed = value.parts().clone();
    changed.authorization_version = FieldVersion::new(2);
    changed.keys = PairingKeys::new(PairingKeysParts {
        device_identity_public_key: [8; 32],
        pairing_authentication_public_key: [9; 32],
    })
    .unwrap();
    assert_eq!(
        store.commit(&proposal(
            &before,
            marks(),
            vec![pairing_write(
                PairingAuthorization::new(changed).unwrap(),
                Some(FieldVersion::new(1))
            )]
        )),
        CommitDisposition::NotApplied(StoreFailure::InvalidTransaction)
    );
    assert_eq!(store.recover().unwrap(), before);
}

fn allocation(before: &RecoveredState) -> GitAllocationReservation {
    let mut next = before.high_water;
    next.repository += 1;
    next.worktree += 1;
    GitAllocationReservation::new(GitAllocationReservationParts {
        binding: operation(),
        expected_high_water: before.high_water,
        resulting_high_water: next,
        repository: Some(RepositoryId::new(server(), next.repository).unwrap()),
        worktree: Some(WorktreeId::new(server(), next.worktree).unwrap()),
        fallbacks: vec![],
    })
    .unwrap()
}
fn locator() -> RepositoryLocator {
    RepositoryLocator::new(RepositoryLocatorParts {
        scope: RepositoryScope {
            server: server(),
            repository: RepositoryId::new(server(), 1).unwrap(),
        },
        main_root: ServerDirectory::new(server(), b"/missing/repo".to_vec()).unwrap(),
        common_directory: ServerDirectory::new(server(), b"/missing/repo/.git".to_vec()).unwrap(),
    })
    .unwrap()
}
fn intent() -> GitWriteIntent {
    GitWriteIntent::new(GitWriteIntentParts {
        binding: operation(),
        repository: locator(),
        action: RetainedGitAction::Create {
            worktree: WorktreeId::new(server(), 1).unwrap(),
            directory: ServerDirectory::new(server(), b"/missing/worktree".to_vec()).unwrap(),
            branch: WorktreeBranchChoice::Existing {
                name: BranchName::new("feature".into()).unwrap(),
            },
            target: CreationTargetObservation::Absent,
            template_version: None,
        },
        phase: GitIntentPhase::Prepared,
        assessment_digest: GitAssessmentDigest::from_bytes([6; 32]),
        affected: RetainedRemovalScope::empty(server()),
        allocation: operation(),
        external_result: None,
    })
    .unwrap()
}
fn reserve_and_prepare(store: &mut SqliteOrganizationStore) -> RecoveredState {
    let before = store.recover().unwrap();
    assert!(matches!(
        store.commit(&proposal(
            &before,
            marks(),
            vec![pairing_write(authorization(1, 1), None)]
        )),
        CommitDisposition::Durable(_)
    ));
    let before = store.recover().unwrap();
    let reservation = allocation(&before);
    let next = reservation.parts().resulting_high_water;
    let writes = vec![
        OperationWrite::GitAllocation(GitAllocationMutation::Reserve(Box::new(reservation))),
        OperationWrite::GitRepository(GitRepositoryMutation::Put(locator())),
        OperationWrite::GitIntent(GitIntentMutation::Insert(Box::new(intent()))),
    ];
    assert!(matches!(
        store.commit(&proposal(&before, next, writes)),
        CommitDisposition::Durable(_)
    ));
    store.recover().unwrap()
}

#[test]
fn unfinished_git_projection_and_allocation_survive_reopen_without_filesystem_queries() {
    let (directory, mut store) = fixture();
    let before = reserve_and_prepare(&mut store);
    drop(store);
    let recovered = open(&directory).recover().unwrap();
    assert_eq!(recovered, before);
    assert_eq!(recovered.git_intents, vec![intent()]);
    assert_eq!(recovered.git_allocations.len(), 1);
}

#[test]
fn git_phase_and_digest_are_compared_inside_the_physical_transaction() {
    let (_directory, mut store) = fixture();
    let before = reserve_and_prepare(&mut store);
    let start = OperationWrite::GitIntent(GitIntentMutation::Advance {
        binding: operation(),
        expected_phase: GitIntentPhase::Prepared,
        next: GitIntentPhase::Started,
        result: None,
    });
    assert!(matches!(
        store.commit(&proposal(&before, before.high_water, vec![start.clone()])),
        CommitDisposition::Durable(_)
    ));
    let before = store.recover().unwrap();
    assert_eq!(
        store.commit(&proposal(&before, before.high_water, vec![start])),
        CommitDisposition::NotApplied(StoreFailure::InvalidTransaction)
    );
    let mut wrong = operation();
    wrong.digest = RequestDigest::from_bytes([99; 32]);
    let advance = OperationWrite::GitIntent(GitIntentMutation::Advance {
        binding: wrong,
        expected_phase: GitIntentPhase::Started,
        next: GitIntentPhase::ExternalFinished,
        result: Some(RetainedExternalResult {
            disposition:
                spaceterm_core::worktrees::types::ExternalDisposition::DesiredStateObserved,
            failure: None,
            ended_sessions: 0,
        }),
    });
    assert_eq!(
        store.commit(&proposal(&before, before.high_water, vec![advance])),
        CommitDisposition::NotApplied(StoreFailure::InvalidTransaction)
    );
    assert_eq!(store.recover().unwrap(), before);
}

#[test]
fn git_completion_without_same_transaction_outcome_is_rejected() {
    let (_directory, mut store) = fixture();
    let before = reserve_and_prepare(&mut store);
    let writes = vec![
        OperationWrite::GitIntent(GitIntentMutation::Complete {
            binding: operation(),
        }),
        OperationWrite::GitAllocation(GitAllocationMutation::Abandon {
            binding: operation(),
        }),
    ];
    assert_eq!(
        store.commit(&proposal(&before, before.high_water, writes)),
        CommitDisposition::NotApplied(StoreFailure::InvalidTransaction)
    );
    assert_eq!(store.recover().unwrap(), before);
}

#[test]
fn repository_locator_cannot_be_deleted_while_reconciliation_intent_exists() {
    let (_directory, mut store) = fixture();
    let before = reserve_and_prepare(&mut store);
    let write =
        OperationWrite::GitRepository(GitRepositoryMutation::Delete(locator().scope().repository));
    assert_eq!(
        store.commit(&proposal(&before, before.high_water, vec![write])),
        CommitDisposition::NotApplied(StoreFailure::InvalidTransaction)
    );
    assert_eq!(store.recover().unwrap(), before);
}

#[test]
fn duplicate_auxiliary_cas_targets_roll_back_without_partial_rows() {
    let (_directory, mut store) = fixture();
    let before = store.recover().unwrap();
    let write = pairing_write(authorization(1, 1), None);
    assert_eq!(
        store.commit(&proposal(&before, marks(), vec![write.clone(), write])),
        CommitDisposition::NotApplied(StoreFailure::InvalidTransaction)
    );
    assert_eq!(store.recover().unwrap(), before);
}

#[test]
fn auxiliary_recovery_checks_reduced_count_limit_before_decoding() {
    let (directory, mut store) = fixture();
    reserve_and_prepare(&mut store);
    drop(store);
    let limits = StoreLimits::production()
        .with_test_ceiling(Collection::GitIntents, 0)
        .unwrap();
    let mut store = SqliteOrganizationStore::from_test_database(
        Database::open_existing(directory.path(), migrations()).unwrap(),
        identity(),
        limits,
        services(),
    )
    .unwrap();
    assert_eq!(store.recover(), Err(StoreFailure::InvalidDurableState));
}

#[test]
fn git_intent_cannot_substitute_a_different_locator_for_the_same_repository() {
    let (_directory, mut store) = fixture();
    let before = store.recover().unwrap();
    assert!(matches!(
        store.commit(&proposal(
            &before,
            marks(),
            vec![pairing_write(authorization(1, 1), None)]
        )),
        CommitDisposition::Durable(_)
    ));
    let before = store.recover().unwrap();
    let reservation = allocation(&before);
    let marks = reservation.parts().resulting_high_water;
    assert!(matches!(
        store.commit(&proposal(
            &before,
            marks,
            vec![
                OperationWrite::GitAllocation(GitAllocationMutation::Reserve(Box::new(
                    reservation
                ))),
                OperationWrite::GitRepository(GitRepositoryMutation::Put(locator()))
            ]
        )),
        CommitDisposition::Durable(_)
    ));
    let before = store.recover().unwrap();
    let mut changed = intent().parts().clone();
    let mut repository = changed.repository.parts().clone();
    repository.main_root = ServerDirectory::new(server(), b"/another-repository".to_vec()).unwrap();
    changed.repository = RepositoryLocator::new(repository).unwrap();
    let write = OperationWrite::GitIntent(GitIntentMutation::Insert(Box::new(
        GitWriteIntent::new(changed).unwrap(),
    )));
    assert_eq!(
        store.commit(&proposal(&before, before.high_water, vec![write])),
        CommitDisposition::NotApplied(StoreFailure::InvalidTransaction)
    );
    assert_eq!(store.recover().unwrap(), before);
}

#[test]
fn native_retirement_projection_preserves_original_request_without_pairing_identity() {
    use spaceterm_core::native_management::*;
    use spaceterm_core::retirement::*;
    let (directory, mut store) = fixture();
    let before = store.recover().unwrap();
    let request =
        NativeManagementRequestRef::new(server(), [44; 16], RequestDigest::from_bytes([45; 32]))
            .unwrap();
    let descriptor = NativeRetirementDescriptor::new(NativeRetirementDescriptorParts {
        request,
        batch: RetirementBatchId::new(server(), ServerEpoch::new(1), 1).unwrap(),
        action: NativeRetirementAction::Stop,
        acceptance_revision: before.organization.revision,
        panes: vec![],
        sessions: vec![],
    })
    .unwrap();
    let mut parts = proposal(
        &before,
        before.high_water,
        vec![OperationWrite::NativeRetirement(
            NativeRetirementMutation::Accept(Box::new(descriptor.clone())),
        )],
    )
    .parts()
    .clone();
    parts.resulting_epoch = ServerEpoch::new(1);
    let transaction = DurableTransaction::new_with_validation(parts, &SqlDomainFake).unwrap();
    assert!(matches!(
        store.commit(&transaction),
        CommitDisposition::Durable(_)
    ));
    drop(store);
    let mut store = open(&directory);
    let recovered = store.recover().unwrap();
    assert_eq!(recovered.native_retirements, vec![descriptor.clone()]);
    let wrong = NativeManagementRequestRef::new(
        server(),
        request.id_bytes(),
        RequestDigest::from_bytes([99; 32]),
    )
    .unwrap();
    let write = OperationWrite::NativeRetirement(NativeRetirementMutation::Complete {
        request: wrong,
        batch: descriptor.batch(),
    });
    assert_eq!(
        store.commit(&proposal(&recovered, recovered.high_water, vec![write])),
        CommitDisposition::NotApplied(StoreFailure::InvalidTransaction)
    );
    assert_eq!(store.recover().unwrap(), recovered);
}

#[test]
fn enrollment_reservation_keeps_immutable_claim_and_never_returns_from_invalidated() {
    let (directory, mut store) = fixture();
    let before = store.recover().unwrap();
    drop(store);
    let mut database = Database::open_existing(directory.path(), migrations()).unwrap();
    let claim = RequestDigest::from_bytes([61; 32]);
    assert_eq!(database.transaction(|sql| {
        let one = 1_u64.to_be_bytes();
        let zero = 0_u64.to_be_bytes();
        sql.execute("INSERT INTO operation_streams VALUES (?1, ?2, ?2, 2, ?3, 0, NULL, ?4, ?4, 3, ?5, 1, 1, 1)", rusqlite::params![3_u64.to_be_bytes().as_slice(), one.as_slice(), claim.bytes().as_slice(), zero.as_slice(), 65536_u64.to_be_bytes().as_slice()])?;
        Ok(())
    }), spaceterm_host::storage::CommitDisposition::Durable(()));
    drop(database);
    let mut store = open(&directory);
    store.recover().unwrap();
    let enrollment = EnrollmentReservation::new(EnrollmentReservationParts {
        ticket: EnrollmentTicketId::new(EnrollmentTicketParts {
            server: server(),
            nonce: [51; 16],
        })
        .unwrap(),
        target: PairingTarget::new(PairingTargetParts {
            server: server(),
            device: DeviceId::new([3; 16]),
            pairing: PairingId::new(server(), 1).unwrap(),
        })
        .unwrap(),
        keys: authorization(1, 1).parts().keys.clone(),
        initial_client: ClientId::new(server(), 1).unwrap(),
        enrollment_stream: OperationStreamId::new(
            OperationStreamKey::new(server(), 3).unwrap(),
            PairingId::new(server(), 1).unwrap(),
            ClientId::new(server(), 1).unwrap(),
        )
        .unwrap(),
        claim_digest: claim,
        authorization_digest: RequestDigest::from_bytes([62; 32]),
        enrollment_request_digest: RequestDigest::from_bytes([63; 32]),
        issuer: EnrollmentIssuer::AccountOwner {
            native_request: [64; 16],
            native_request_digest: RequestDigest::from_bytes([65; 32]),
        },
        issued_epoch: ServerEpoch::new(1),
        issued_revision: before.store_revision.checked_successor().unwrap(),
        issued_at_millis: 100,
        expires_at_millis: 200,
        state: EnrollmentReservationState::Reserved,
    })
    .unwrap();
    let marks = IdentityHighWater {
        operation_stream: 3,
        ..marks()
    };
    let write = OperationWrite::Pairing(Box::new(PairingWrite::PutEnrollmentReservation {
        expected_kind: None,
        value: enrollment.clone(),
    }));
    let mut parts = proposal(&before, marks, vec![write]).parts().clone();
    parts.resulting_epoch = ServerEpoch::new(1);
    assert!(matches!(
        store.commit(&DurableTransaction::new_with_validation(parts, &SqlDomainFake).unwrap()),
        CommitDisposition::Durable(_)
    ));
    let before = store.recover().unwrap();
    let mut invalidated = enrollment.parts().clone();
    invalidated.state = EnrollmentReservationState::Invalidated;
    let mut changed = invalidated.clone();
    changed.claim_digest = RequestDigest::from_bytes([99; 32]);
    let write = OperationWrite::Pairing(Box::new(PairingWrite::PutEnrollmentReservation {
        expected_kind: Some(EnrollmentReservationKind::Reserved),
        value: EnrollmentReservation::new(changed).unwrap(),
    }));
    assert_eq!(
        store.commit(&proposal(&before, marks, vec![write])),
        CommitDisposition::NotApplied(StoreFailure::InvalidTransaction)
    );
    let invalidated = EnrollmentReservation::new(invalidated).unwrap();
    let write = OperationWrite::Pairing(Box::new(PairingWrite::PutEnrollmentReservation {
        expected_kind: Some(EnrollmentReservationKind::Reserved),
        value: invalidated.clone(),
    }));
    assert!(matches!(
        store.commit(&proposal(&before, marks, vec![write])),
        CommitDisposition::Durable(_)
    ));
    drop(store);
    let mut store = open(&directory);
    let before = store.recover().unwrap();
    assert_eq!(
        before.pairing.as_ref().unwrap().parts().enrollments,
        vec![invalidated]
    );
    let write = OperationWrite::Pairing(Box::new(PairingWrite::PutEnrollmentReservation {
        expected_kind: Some(EnrollmentReservationKind::Invalidated),
        value: enrollment,
    }));
    assert_eq!(
        store.commit(&proposal(&before, marks, vec![write])),
        CommitDisposition::NotApplied(StoreFailure::InvalidTransaction)
    );
    assert_eq!(store.recover().unwrap(), before);
}

// Supplies only the frozen pending-ledger comparison input. It neither persists
// ledger rows nor decodes a canonical outcome. The real SQL organization and Git
// allocation projections remain under test; real-producer completion is separate.
struct ReservationLedgerFake {
    pending: Option<OperationBinding>,
}
impl OperationProjection for ReservationLedgerFake {
    fn preflight(
        &self,
        writes: &[OperationWrite],
        budget: &mut SizeBudget,
    ) -> Result<usize, StoreFailure> {
        auxiliary::preflight(writes, budget)
    }
    fn apply(
        &self,
        sql: &Transaction<'_>,
        parts: &TransactionParts,
        previous: &RecoveredState,
    ) -> Result<usize, StoreFailure> {
        auxiliary::apply(sql, parts, previous, &NoReceipts)
    }
    fn recover(
        &self,
        sql: &Transaction<'_>,
        recovered: &mut RecoveredState,
        limits: &StoreLimits,
        budget: &mut SizeBudget,
    ) -> Result<(), StoreFailure> {
        auxiliary::recover(sql, recovered, limits, budget, &NoReceipts)?;
        let mut ledger = LedgerRecovery::empty(LedgerLimits::production_v1());
        if let Some(binding) = self.pending
            && !recovered.git_allocations.is_empty()
        {
            ledger
                .operations
                .push(RecoveredOperationRecord::Pending(Box::new(
                    PendingOperationRecord {
                        binding,
                        kind: PendingKind::External {
                            effect: ExternalEffectKind::GitWrite,
                        },
                        reservation: OutcomeReservation::full(ReservationClass::Ordinary),
                        admitted_at: StoreRevision::new(1),
                        admission_organization_revision: OrganizationRevision::new(0),
                        progress_recorded_at: StoreRevision::new(1),
                        external_progress: Some(
                            PendingExternalProgress::new(PendingExternalProgressParts {
                                binding,
                                effect: ExternalEffectKind::GitWrite,
                                state: ExternalProgressState::AwaitingIntent,
                            })
                            .unwrap(),
                        ),
                    },
                )));
        }
        recovered.ledger = Some(ledger);
        Ok(())
    }
}
fn reservation_store(
    directory: &tempfile::TempDir,
    pending: Option<OperationBinding>,
) -> SqliteOrganizationStore {
    SqliteOrganizationStore::from_test_database(
        Database::open_existing(directory.path(), migrations()).unwrap(),
        identity(),
        StoreLimits::production(),
        StoreServices::new(
            Box::new(SqlDomainFake),
            Box::new(ReservationLedgerFake { pending }),
        ),
    )
    .unwrap()
}
fn fallback_fixture() -> (
    tempfile::TempDir,
    SqliteOrganizationStore,
    GitReservedFallback,
) {
    let (directory, mut store) = fixture();
    let before = store.recover().unwrap();
    assert!(matches!(
        store.commit(&proposal(
            &before,
            marks(),
            vec![pairing_write(authorization(1, 1), None)]
        )),
        CommitDisposition::Durable(_)
    ));
    let before = store.recover().unwrap();
    let workspace = WorkspaceId::new(server(), 1).unwrap();
    let tab = TabId::new(server(), 1).unwrap();
    let pane = PaneId::new(server(), 1).unwrap();
    let surviving = WorktreeId::new(server(), 1).unwrap();
    let removed = WorktreeId::new(server(), 2).unwrap();
    let repository = RepositoryId::new(server(), 1).unwrap();
    let mut parts = proposal(&before, before.high_water, vec![]).parts().clone();
    parts.resulting_organization_revision =
        before.organization.revision.checked_successor().unwrap();
    parts.resulting_high_water = IdentityHighWater {
        workspace: 1,
        tab: 1,
        pane: 1,
        repository: 1,
        worktree: 2,
        creation_ordinal: 3,
        ..before.high_water
    };
    parts.organization_writes = vec![
        OrganizationWrite::PutWorkspace(WorkspaceRecord {
            id: workspace,
            name: Versioned {
                value: WorkspaceName {
                    mode: NameMode::Custom,
                    display: NameText::new("fallback fixture".into()).unwrap(),
                },
                version: FieldVersion::new(1),
            },
            pinned_directory: Versioned {
                value: None,
                version: FieldVersion::new(1),
            },
            root_tab: tab,
            creation_ordinal: CreationOrdinal::new(1),
        }),
        OrganizationWrite::PutTab(TabRecord {
            id: tab,
            workspace,
            worktree: Some(removed),
            root_pane: pane,
            creation_ordinal: CreationOrdinal::new(2),
        }),
        OrganizationWrite::PutPane(PaneRecord {
            id: pane,
            tab,
            creation_ordinal: CreationOrdinal::new(3),
            creation: PaneCreation::Initial,
            starting_directory: ServerDirectory::new(server(), b"/missing/removed".to_vec())
                .unwrap(),
            confirmed_restoration_directory: Versioned {
                value: None,
                version: FieldVersion::new(1),
            },
        }),
        OrganizationWrite::PutRepository(RepositoryRecord {
            id: repository,
            directory: locator().parts().main_root.clone(),
        }),
        OrganizationWrite::Retained(RetainedOrganizationWrite::PutWorkspaceNameBasis(
            WorkspaceNameBasis::new(
                workspace,
                NameText::new("repo".into()).unwrap(),
                NameText::new("repo".into()).unwrap(),
            ),
        )),
        OrganizationWrite::Retained(RetainedOrganizationWrite::PutCreationDefault(
            TabCreationDefault::new(tab, vec![pane]).unwrap(),
        )),
        OrganizationWrite::Retained(RetainedOrganizationWrite::SetTabMembershipVersion(
            TabMembershipVersion::new(tab, FieldVersion::new(1)),
        )),
        OrganizationWrite::Retained(RetainedOrganizationWrite::SetPaneGenerationHighWater(
            PaneGenerationHighWater::new(
                pane,
                SessionGeneration::new(0),
                ControlGeneration::new(0),
            ),
        )),
        OrganizationWrite::Retained(RetainedOrganizationWrite::SetRepositoryInventoryVersion(
            RepositoryInventoryVersion::new(repository, FieldVersion::new(1)),
        )),
    ];
    for (id, main, path) in [
        (surviving, true, b"/missing/repo".as_slice()),
        (removed, false, b"/missing/removed".as_slice()),
    ] {
        parts.organization_writes.extend([
            OrganizationWrite::PutWorktree(WorktreeRecord {
                id,
                repository,
                directory: ServerDirectory::new(server(), path.to_vec()).unwrap(),
                main,
                missing: false,
            }),
            OrganizationWrite::Retained(RetainedOrganizationWrite::SetWorktreeInventoryFacts(
                WorktreeInventoryFacts::new(id, true, FieldVersion::new(1)),
            )),
        ]);
    }
    assert!(matches!(
        store.commit(&DurableTransaction::new_with_validation(parts, &SqlDomainFake).unwrap()),
        CommitDisposition::Durable(_)
    ));
    let before = store.recover().unwrap();
    let fallback = GitReservedFallback {
        plan: WorkspaceFallbackPlan {
            workspace,
            worktree: surviving,
            directory: locator().parts().main_root.clone(),
        },
        tab: TabId::new(server(), 2).unwrap(),
        pane: PaneId::new(server(), 2).unwrap(),
        tab_creation_ordinal: CreationOrdinal::new(4),
        pane_creation_ordinal: CreationOrdinal::new(5),
    };
    let next = IdentityHighWater {
        tab: 2,
        pane: 2,
        creation_ordinal: 5,
        ..before.high_water
    };
    let reservation = GitAllocationReservation::new(GitAllocationReservationParts {
        binding: operation(),
        expected_high_water: before.high_water,
        resulting_high_water: next,
        repository: None,
        worktree: None,
        fallbacks: vec![fallback.clone()],
    })
    .unwrap();
    assert!(matches!(
        store.commit(&proposal(
            &before,
            next,
            vec![OperationWrite::GitAllocation(
                GitAllocationMutation::Reserve(Box::new(reservation))
            )]
        )),
        CommitDisposition::Durable(_)
    ));
    drop(store);
    let mut store = reservation_store(&directory, Some(operation()));
    store.recover().unwrap();
    (directory, store, fallback)
}
fn fallback_completion(
    before: &RecoveredState,
    fallback: &GitReservedFallback,
) -> TransactionParts {
    let mut parts = proposal(
        before,
        before.high_water,
        vec![
            OperationWrite::GitAllocation(GitAllocationMutation::Consume {
                binding: operation(),
            }),
            OperationWrite::Ledger(Box::new(LedgerWrite::CompleteOperation {
                record: CompletedOperationRecord {
                    binding: operation(),
                    reservation: OutcomeReservation::full(ReservationClass::Ordinary),
                    admitted_at: StoreRevision::new(1),
                    completed_at: before.store_revision.checked_successor().unwrap(),
                    // Test input to the frozen port, not an alternate compact codec.
                    outcome: CompactOutcome::from_encoded(b"fixture-only".to_vec()).unwrap(),
                    outcome_revision: before.organization.revision.checked_successor().unwrap(),
                },
                basis: CompletionRevisionBasis::RecordedProgress,
            })),
        ],
    )
    .parts()
    .clone();
    parts.resulting_organization_revision =
        before.organization.revision.checked_successor().unwrap();
    let mut workspace = before.organization.workspaces[0].clone();
    workspace.root_tab = fallback.tab;
    let old_tab = before.organization.tabs[0].id;
    let old_pane = before.organization.panes[0].id;
    let removed = before.organization.tabs[0].worktree.unwrap();
    parts.organization_writes = vec![
        OrganizationWrite::DeletePane(old_pane),
        OrganizationWrite::DeleteTab(old_tab),
        OrganizationWrite::DeleteWorktree(removed),
        OrganizationWrite::Retained(RetainedOrganizationWrite::DeleteCreationDefault(old_tab)),
        OrganizationWrite::Retained(RetainedOrganizationWrite::DeleteTabMembershipVersion(
            old_tab,
        )),
        OrganizationWrite::Retained(RetainedOrganizationWrite::DeletePaneGenerationHighWater(
            old_pane,
        )),
        OrganizationWrite::Retained(RetainedOrganizationWrite::DeleteWorktreeInventoryFacts(
            removed,
        )),
        OrganizationWrite::PutWorkspace(workspace),
        OrganizationWrite::PutTab(TabRecord {
            id: fallback.tab,
            workspace: fallback.plan.workspace,
            worktree: Some(fallback.plan.worktree),
            root_pane: fallback.pane,
            creation_ordinal: fallback.tab_creation_ordinal,
        }),
        OrganizationWrite::PutPane(PaneRecord {
            id: fallback.pane,
            tab: fallback.tab,
            creation_ordinal: fallback.pane_creation_ordinal,
            creation: PaneCreation::Initial,
            starting_directory: fallback.plan.directory.clone(),
            confirmed_restoration_directory: Versioned {
                value: None,
                version: FieldVersion::new(1),
            },
        }),
        OrganizationWrite::Retained(RetainedOrganizationWrite::PutCreationDefault(
            TabCreationDefault::new(fallback.tab, vec![fallback.pane]).unwrap(),
        )),
        OrganizationWrite::Retained(RetainedOrganizationWrite::SetTabMembershipVersion(
            TabMembershipVersion::new(fallback.tab, FieldVersion::new(1)),
        )),
        OrganizationWrite::Retained(RetainedOrganizationWrite::SetPaneGenerationHighWater(
            PaneGenerationHighWater::new(
                fallback.pane,
                SessionGeneration::new(0),
                ControlGeneration::new(0),
            ),
        )),
    ];
    parts
}

#[test]
fn reserved_fallback_tab_preserves_exact_surviving_worktree_on_reopen() {
    let (directory, mut store, fallback) = fallback_fixture();
    let before = store.recover().unwrap();
    assert_eq!(before.high_water.tab, fallback.tab.local());
    assert!(
        !before
            .organization
            .tabs
            .iter()
            .any(|v| v.id == fallback.tab)
    );
    let transaction = DurableTransaction::new_with_validation(
        fallback_completion(&before, &fallback),
        &SqlDomainFake,
    )
    .unwrap();
    assert_eq!(
        store.commit(&transaction),
        CommitDisposition::Durable(CommitReceipt {
            store_revision: transaction.parts().resulting_store_revision,
            organization_revision: transaction.parts().resulting_organization_revision,
        })
    );
    drop(store);
    let recovered = reservation_store(&directory, Some(operation()))
        .recover()
        .unwrap();
    assert_eq!(recovered.organization.tabs.len(), 1);
    assert_eq!(recovered.organization.tabs[0].id, fallback.tab);
    assert_eq!(
        recovered.organization.tabs[0].worktree,
        Some(fallback.plan.worktree)
    );
    assert_eq!(recovered.organization.workspaces[0].root_tab, fallback.tab);
    assert_eq!(
        recovered.organization.panes[0].starting_directory,
        fallback.plan.directory
    );
    assert_eq!(recovered.high_water, before.high_water);
    assert!(recovered.git_allocations.is_empty());
}

#[test]
fn reserved_fallback_tab_cannot_omit_or_substitute_worktree_membership() {
    let (_directory, mut store, fallback) = fallback_fixture();
    let before = store.recover().unwrap();
    for membership in [None, Some(WorktreeId::new(server(), 3).unwrap())] {
        let mut parts = fallback_completion(&before, &fallback);
        // Keep the substituted Worktree present so a foreign-key rejection cannot
        // hide a missing exact-reservation membership comparison.
        if let Some(worktree) = membership {
            parts.resulting_high_water.worktree = worktree.local();
            parts.organization_writes.extend([
                OrganizationWrite::PutWorktree(WorktreeRecord {
                    id: worktree,
                    repository: RepositoryId::new(server(), 1).unwrap(),
                    directory: ServerDirectory::new(server(), b"/missing/other".to_vec()).unwrap(),
                    main: false,
                    missing: false,
                }),
                OrganizationWrite::Retained(RetainedOrganizationWrite::SetWorktreeInventoryFacts(
                    WorktreeInventoryFacts::new(worktree, true, FieldVersion::new(1)),
                )),
            ]);
        }
        for write in &mut parts.organization_writes {
            if let OrganizationWrite::PutTab(tab) = write {
                tab.worktree = membership;
            }
        }
        let transaction = DurableTransaction::new_with_validation(parts, &SqlDomainFake).unwrap();
        assert_eq!(
            store.commit(&transaction),
            CommitDisposition::NotApplied(StoreFailure::InvalidTransaction)
        );
        assert_eq!(store.recover().unwrap(), before);
    }
}

#[test]
fn reserved_fallback_requires_original_pending_binding_and_atomic_consumption() {
    let (directory, mut store, fallback) = fallback_fixture();
    let before = store.recover().unwrap();
    for missing_completion in [true, false] {
        let mut parts = fallback_completion(&before, &fallback);
        parts.operation_writes.retain(|write| {
            if missing_completion {
                !matches!(write, OperationWrite::Ledger(_))
            } else {
                !matches!(write, OperationWrite::GitAllocation(_))
            }
        });
        let transaction = DurableTransaction::new_with_validation(parts, &SqlDomainFake).unwrap();
        assert_eq!(
            store.commit(&transaction),
            CommitDisposition::NotApplied(StoreFailure::InvalidTransaction)
        );
        assert_eq!(store.recover().unwrap(), before);
    }
    let mut wrong = operation();
    wrong.digest = RequestDigest::from_bytes([99; 32]);
    let mut parts = fallback_completion(&before, &fallback);
    for write in &mut parts.operation_writes {
        match write {
            OperationWrite::GitAllocation(GitAllocationMutation::Consume { binding }) => {
                *binding = wrong
            }
            OperationWrite::Ledger(value) => {
                if let LedgerWrite::CompleteOperation { record, .. } = value.as_mut() {
                    record.binding = wrong;
                }
            }
            _ => {}
        }
    }
    let transaction = DurableTransaction::new_with_validation(parts, &SqlDomainFake).unwrap();
    assert_eq!(
        store.commit(&transaction),
        CommitDisposition::NotApplied(StoreFailure::InvalidTransaction)
    );
    assert_eq!(store.recover().unwrap(), before);
    drop(store);
    for pending in [None, Some(wrong)] {
        let mut store = reservation_store(&directory, pending);
        let before = store.recover().unwrap();
        let transaction = DurableTransaction::new_with_validation(
            fallback_completion(&before, &fallback),
            &SqlDomainFake,
        )
        .unwrap();
        assert_eq!(
            store.commit(&transaction),
            CommitDisposition::NotApplied(StoreFailure::InvalidTransaction)
        );
        assert_eq!(store.recover().unwrap(), before);
    }
}
