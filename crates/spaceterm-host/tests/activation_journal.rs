use spaceterm_host::{
    activation_journal::{
        ActivationJournalRecord, ActivationJournalState as S, NativeOperationKind,
        RecoverySelection, RetainedNativeOutcome,
    },
    installation::{ActivationEffectPlan, plan_activation, propose_lifecycle},
    native_deployment::contracts::{
        DataGenerationId, NativeFailure, NativeRequestId, VerifiedCandidateId,
    },
    native_registration::{RegistrationHeadV2, StartupBarrier},
    startup::{AccountBuildKey, BuildIdentity, DesiredServerState, InstallationId},
};
use std::num::NonZeroU64;

fn nz(v: u64) -> NonZeroU64 {
    NonZeroU64::new(v).unwrap()
}
fn head(state: DesiredServerState) -> RegistrationHeadV2 {
    RegistrationHeadV2 {
        key: AccountBuildKey {
            account_uid: 501,
            build_identity: BuildIdentity::Development,
        },
        generation: nz(7),
        lifecycle_generation: nz(2),
        desired_state: state,
        approved_installation: InstallationId(nz(11)),
        staged_installation: Some(InstallationId(nz(12))),
        approved_data_generation: DataGenerationId::new(1).unwrap(),
        active_request: None,
        startup_barrier: StartupBarrier::Open,
        profile_sha256: [1; 32],
        approved_manifest_sha256: [2; 32],
        approved_descriptor_sha256: [3; 32],
        staged_descriptor_sha256: Some([4; 32]),
        installation_high_water: 12,
        data_high_water: 2,
        server_id: [5; 16],
    }
}
fn journal(state: DesiredServerState) -> ActivationJournalRecord {
    let h = head(state);
    ActivationJournalRecord {
        schema_version: 1,
        request: NativeRequestId::new([1; 16]).unwrap(),
        operation: NativeOperationKind::Activate,
        key: h.key,
        server_id: h.server_id,
        account_binding_sha256: [6; 32],
        expected_registration_generation: h.generation,
        expected_lifecycle_generation: h.lifecycle_generation,
        original_installation: h.approved_installation,
        original_data_generation: h.approved_data_generation,
        candidate_input: None,
        candidate_installation: h.staged_installation,
        candidate_manifest_sha256: Some([7; 32]),
        candidate_data_generation: Some(DataGenerationId::new(2).unwrap()),
        desired_state: state,
        assessment_digest: Some([8; 32]),
        state: S::ActivationPrepared,
        outcome: None,
    }
}
fn inhibited(mut h: RegistrationHeadV2, j: &ActivationJournalRecord) -> RegistrationHeadV2 {
    h.generation = nz(8);
    h.active_request = Some(j.request);
    h.startup_barrier = StartupBarrier::Inhibited;
    h
}
fn committed(mut h: RegistrationHeadV2, j: &ActivationJournalRecord) -> RegistrationHeadV2 {
    h.generation = nz(9);
    h.active_request = Some(j.request);
    h.startup_barrier = StartupBarrier::ApprovedStartOnly;
    h.approved_installation = j.candidate_installation.unwrap();
    h.approved_data_generation = j.candidate_data_generation.unwrap();
    h.approved_manifest_sha256 = j.candidate_manifest_sha256.unwrap();
    h.approved_descriptor_sha256 = h.staged_descriptor_sha256.take().unwrap();
    h.staged_installation = None;
    h
}

#[test]
fn journal_rejects_duplicate_unknown_missing_noncanonical_and_mismatched_records() {
    let j = journal(DesiredServerState::Running);
    let bytes = j.encode().unwrap();
    assert_eq!(ActivationJournalRecord::decode(&bytes).unwrap(), j);
    let text = String::from_utf8(bytes).unwrap();
    for bad in [
        format!("{text}\n"),
        text.replacen('{', "{\"unknown\":0,", 1),
        text.replacen('{', "{\"state\":\"activation-prepared\",", 1),
        text.replace("\"outcome\":null,", ""),
        text.replace("\"schema_version\":1", "\"schema_version\":2"),
    ] {
        assert!(ActivationJournalRecord::decode(bad.as_bytes()).is_err());
    }
    let mut changed = j.clone();
    changed.key.account_uid += 1;
    assert_eq!(
        changed.recovery_selection(&head(DesiredServerState::Running), &[6; 32]),
        Err(NativeFailure::IdentityMismatch)
    );
}

#[test]
fn journal_crash_before_and_after_every_commit_boundary() {
    let initial = head(DesiredServerState::Running);
    let mut j = journal(DesiredServerState::Running);
    let closed = inhibited(initial.clone(), &j);
    for state in [
        S::ActivationPrepared,
        S::Quiescing,
        S::Quiesced,
        S::Migrating,
        S::MigrationReady,
    ] {
        j.state = state;
        let encoded = j.encode().unwrap();
        let restored = ActivationJournalRecord::decode(&encoded).unwrap();
        assert!(
            matches!(restored.recovery_selection(&closed, &[6;32]).unwrap(), RecoverySelection::InhibitedOriginal { installation, .. } if installation == initial.approved_installation)
        );
    }
    let published = committed(closed, &j);
    // Crash after approval-head sync but before the journal can record that sync.
    for state in [S::MigrationReady, S::ApprovalCommitted, S::Starting] {
        j.state = state;
        let restored = ActivationJournalRecord::decode(&j.encode().unwrap()).unwrap();
        assert!(
            matches!(restored.recovery_selection(&published, &[6;32]).unwrap(), RecoverySelection::Committed { installation, .. } if Some(installation) == j.candidate_installation)
        );
    }
}

#[test]
fn install_recovers_running_and_stopped_head_commits_before_journal_advance() {
    for desired in [DesiredServerState::Running, DesiredServerState::Stopped] {
        let initial = head(desired);
        let mut admitted = journal(desired);
        admitted.operation = NativeOperationKind::Install;
        admitted.candidate_input = Some(VerifiedCandidateId::new([4; 16]).unwrap());
        admitted.assessment_digest = None;
        admitted.state = S::Staged;
        let published = committed(initial.clone(), &admitted);
        for phase in [S::Staged, S::ApprovalCommitted] {
            admitted.state = phase;
            let restored = ActivationJournalRecord::decode(&admitted.encode().unwrap()).unwrap();
            assert_eq!(
                restored.recovery_selection(&published, &[6; 32]),
                Ok(RecoverySelection::Committed {
                    installation: published.approved_installation,
                    data: published.approved_data_generation,
                    desired_state: desired,
                }),
                "{desired:?} at {phase:?}"
            );
            let mut unrelated = published.clone();
            unrelated.active_request = Some(NativeRequestId::new([9; 16]).unwrap());
            assert_eq!(
                restored.recovery_selection(&unrelated, &[6; 32]),
                Err(NativeFailure::IdentityMismatch)
            );
            let mut wrong_data = published.clone();
            wrong_data.approved_data_generation = initial.approved_data_generation;
            assert_eq!(
                restored.recovery_selection(&wrong_data, &[6; 32]),
                Err(NativeFailure::UnsafeNativeState)
            );
        }
        admitted.state = S::Staged;
        assert!(matches!(
            admitted.recovery_selection(&initial, &[6; 32]),
            Ok(RecoverySelection::Original { .. })
        ));
        admitted.state = S::Staging;
        assert_eq!(
            admitted.recovery_selection(&published, &[6; 32]),
            Err(NativeFailure::UnsafeNativeState)
        );
        admitted.state = S::ApprovalCommitted;
        assert_eq!(
            admitted.recovery_selection(&initial, &[6; 32]),
            Err(NativeFailure::UnsafeNativeState)
        );
        assert!(admitted.transition(S::Aborted).is_err());
    }
}

#[test]
fn precommit_failure_preserves_old_selection() {
    let initial = head(DesiredServerState::Running);
    let j = journal(DesiredServerState::Running)
        .transition(S::Aborted)
        .unwrap();
    assert_eq!(j.outcome, Some(RetainedNativeOutcome::Aborted));
    assert_eq!(
        j.recovery_selection(&initial, &[6; 32]),
        Ok(RecoverySelection::Original {
            installation: initial.approved_installation,
            data: initial.approved_data_generation
        })
    );
}

#[test]
fn postcommit_failure_never_silently_rolls_back() {
    let initial = head(DesiredServerState::Running);
    let mut j = journal(DesiredServerState::Running);
    j.state = S::ApprovalCommitted;
    assert_eq!(
        j.recovery_selection(&initial, &[6; 32]),
        Err(NativeFailure::UnsafeNativeState)
    );
    assert!(j.transition(S::Aborted).is_err());
    let published = committed(initial, &j);
    assert!(matches!(
        j.recovery_selection(&published, &[6; 32]).unwrap(),
        RecoverySelection::Committed { .. }
    ));
}

#[test]
fn stopped_activation_has_no_credential_or_start_effects() {
    let initial = head(DesiredServerState::Stopped);
    let mut j = journal(DesiredServerState::Stopped);
    j.candidate_data_generation = Some(initial.approved_data_generation);
    assert_eq!(
        plan_activation(&initial, &j),
        Ok(ActivationEffectPlan::CommitWithoutStart)
    );
    j.state = S::Quiesced;
    assert!(j.transition(S::Migrating).is_err());
    let j = j
        .transition(S::MigrationReady)
        .unwrap()
        .transition(S::ApprovalCommitted)
        .unwrap();
    assert!(j.transition(S::Starting).is_err());
    let complete = j.transition(S::Completed).unwrap();
    assert_eq!(
        complete.outcome,
        Some(RetainedNativeOutcome::ApprovedStopped)
    );
}

#[test]
fn concurrent_stop_invalidates_activation_assessment() {
    let initial = head(DesiredServerState::Running);
    let j = journal(DesiredServerState::Running);
    let stopped = propose_lifecycle(&initial, DesiredServerState::Stopped).unwrap();
    assert_eq!(
        plan_activation(&stopped, &j),
        Err(NativeFailure::StaleAssessment)
    );
    assert_eq!(
        j.recovery_selection(&stopped, &[6; 32]),
        Err(NativeFailure::GenerationMismatch)
    );
}

#[test]
fn journal_replay_never_advances_request_or_allows_different_contents() {
    let j = journal(DesiredServerState::Running);
    let mut updated = j.clone();
    updated.state = S::Quiescing;
    assert_eq!(j.same_request(&updated), Ok(()));
    updated.candidate_installation = Some(InstallationId(nz(13)));
    assert_eq!(
        j.same_request(&updated),
        Err(NativeFailure::IdentityMismatch)
    );
}

fn lifecycle_journal(
    operation: NativeOperationKind,
) -> (RegistrationHeadV2, ActivationJournalRecord) {
    let initial = head(match operation {
        NativeOperationKind::Start => DesiredServerState::Stopped,
        _ => DesiredServerState::Running,
    });
    let mut j = journal(initial.desired_state);
    j.operation = operation;
    j.candidate_installation = None;
    j.candidate_manifest_sha256 = None;
    j.candidate_data_generation = None;
    j.desired_state = match operation {
        NativeOperationKind::Stop => DesiredServerState::Stopped,
        _ => DesiredServerState::Running,
    };
    (initial, j)
}

#[test]
fn lifecycle_recovery_recognizes_its_own_intent_commit_and_rejects_later_requests() {
    for operation in [
        NativeOperationKind::Start,
        NativeOperationKind::Stop,
        NativeOperationKind::Restart,
    ] {
        let (initial, j) = lifecycle_journal(operation);
        assert_eq!(
            j.recovery_selection(&initial, &[6; 32]),
            Ok(RecoverySelection::Original {
                installation: initial.approved_installation,
                data: initial.approved_data_generation,
            }),
            "precommit {operation:?}"
        );
        let mut published = propose_lifecycle(&initial, j.desired_state).unwrap();
        published.active_request = Some(j.request);
        published.startup_barrier = StartupBarrier::Inhibited;
        let restored = ActivationJournalRecord::decode(&j.encode().unwrap()).unwrap();
        assert_eq!(
            restored.recovery_selection(&published, &[6; 32]),
            Ok(RecoverySelection::InhibitedOriginal {
                installation: initial.approved_installation,
                data: initial.approved_data_generation,
            }),
            "crash after intent-head publication, before journal advance: {operation:?}"
        );
        let mut unrelated = published.clone();
        unrelated.active_request = Some(NativeRequestId::new([9; 16]).unwrap());
        assert!(j.recovery_selection(&unrelated, &[6; 32]).is_err());
        published.lifecycle_generation = nz(4);
        published.desired_state = DesiredServerState::Stopped;
        assert_eq!(
            j.recovery_selection(&published, &[6; 32]),
            Err(NativeFailure::GenerationMismatch),
            "later Stop must not count as this request's commit: {operation:?}"
        );
    }
}

#[test]
fn lifecycle_journals_complete_and_retain_only_their_operation_outcomes() {
    for (operation, phases, outcome) in [
        (
            NativeOperationKind::Start,
            vec![S::Starting, S::Completed],
            RetainedNativeOutcome::Running,
        ),
        (
            NativeOperationKind::Stop,
            vec![S::Quiescing, S::Quiesced, S::Completed],
            RetainedNativeOutcome::Stopped,
        ),
        (
            NativeOperationKind::Restart,
            vec![S::Quiescing, S::Quiesced, S::Starting, S::Completed],
            RetainedNativeOutcome::Running,
        ),
    ] {
        let (initial, admitted) = lifecycle_journal(operation);
        let mut j = admitted.clone();
        let mut published = propose_lifecycle(&initial, j.desired_state).unwrap();
        published.active_request = Some(j.request);
        published.startup_barrier = StartupBarrier::Inhibited;
        for next in phases {
            j = j
                .transition(next)
                .unwrap_or_else(|e| panic!("{operation:?} -> {next:?}: {e}"));
            let restored = ActivationJournalRecord::decode(&j.encode().unwrap()).unwrap();
            assert!(restored.recovery_selection(&published, &[6; 32]).is_ok());
        }
        assert_eq!(j.outcome, Some(outcome));
        assert_eq!(j.resolve_request(&admitted), Ok(outcome));
        assert!(j.transition(S::Aborted).is_err());
        published.startup_barrier = StartupBarrier::Open;
        published.active_request = None;
        published.generation = nz(9);
        assert!(j.recovery_selection(&published, &[6; 32]).is_ok());
        published.lifecycle_generation = nz(4);
        assert_eq!(
            j.recovery_selection(&published, &[6; 32]),
            Err(NativeFailure::GenerationMismatch)
        );
    }
}

#[test]
fn lifecycle_journal_rejects_wrong_intent_phase_and_terminal_outcome() {
    for operation in [
        NativeOperationKind::Start,
        NativeOperationKind::Stop,
        NativeOperationKind::Restart,
    ] {
        let (_, mut j) = lifecycle_journal(operation);
        j.state = S::Completed;
        j.outcome = Some(match operation {
            NativeOperationKind::Stop => RetainedNativeOutcome::Running,
            _ => RetainedNativeOutcome::Stopped,
        });
        assert_eq!(
            j.validate(),
            Err(NativeFailure::InvalidRecord),
            "{operation:?}"
        );
        j.state = S::ActivationPrepared;
        j.outcome = None;
        j.desired_state = match j.desired_state {
            DesiredServerState::Running => DesiredServerState::Stopped,
            DesiredServerState::Stopped => DesiredServerState::Running,
        };
        assert_eq!(
            j.validate(),
            Err(NativeFailure::InvalidRecord),
            "wrong intent: {operation:?}"
        );
    }
    let (_, start) = lifecycle_journal(NativeOperationKind::Start);
    assert!(start.transition(S::Quiescing).is_err());
    let (_, mut stop) = lifecycle_journal(NativeOperationKind::Stop);
    stop.state = S::Quiesced;
    assert!(stop.transition(S::Starting).is_err());
    assert!(stop.transition(S::Aborted).is_err());
    let (_, mut restart) = lifecycle_journal(NativeOperationKind::Restart);
    restart.state = S::Quiesced;
    assert!(restart.transition(S::Completed).is_err());
}

#[test]
fn every_native_operation_has_an_operation_specific_terminal_outcome() {
    for (operation, expected, terminal_predecessor) in [
        (
            NativeOperationKind::Install,
            RetainedNativeOutcome::Running,
            S::Starting,
        ),
        (
            NativeOperationKind::Stage,
            RetainedNativeOutcome::Staged,
            S::Staged,
        ),
        (
            NativeOperationKind::Activate,
            RetainedNativeOutcome::Running,
            S::Starting,
        ),
        (
            NativeOperationKind::Uninstall,
            RetainedNativeOutcome::Removed,
            S::Quiesced,
        ),
        (
            NativeOperationKind::Repair,
            RetainedNativeOutcome::Applied,
            S::ActivationPrepared,
        ),
        (
            NativeOperationKind::Collect,
            RetainedNativeOutcome::Applied,
            S::ActivationPrepared,
        ),
        (
            NativeOperationKind::AuthorizeRemoteAccess,
            RetainedNativeOutcome::Applied,
            S::ActivationPrepared,
        ),
        (
            NativeOperationKind::AuthorizeOwnerEnrollment,
            RetainedNativeOutcome::Applied,
            S::ActivationPrepared,
        ),
    ] {
        let mut j = journal(DesiredServerState::Running);
        j.operation = operation;
        if matches!(
            operation,
            NativeOperationKind::Install | NativeOperationKind::Stage
        ) {
            j.candidate_input = Some(VerifiedCandidateId::new([4; 16]).unwrap());
        }
        j.state = terminal_predecessor;
        if operation == NativeOperationKind::Stage {
            j.candidate_data_generation = None;
        } else if !matches!(
            operation,
            NativeOperationKind::Install | NativeOperationKind::Activate
        ) {
            j.candidate_installation = None;
            j.candidate_manifest_sha256 = None;
            j.candidate_data_generation = None;
        }
        if operation == NativeOperationKind::Uninstall {
            j.desired_state = DesiredServerState::Stopped;
        }
        let complete = j
            .transition(S::Completed)
            .unwrap_or_else(|e| panic!("{operation:?}: {e}"));
        assert_eq!(complete.outcome, Some(expected));
        for wrong in [
            RetainedNativeOutcome::Running,
            RetainedNativeOutcome::ApprovedStopped,
            RetainedNativeOutcome::Stopped,
            RetainedNativeOutcome::Staged,
            RetainedNativeOutcome::Removed,
            RetainedNativeOutcome::Applied,
            RetainedNativeOutcome::Aborted,
        ] {
            if wrong != expected {
                let mut changed = complete.clone();
                changed.outcome = Some(wrong);
                assert_eq!(
                    changed.validate(),
                    Err(NativeFailure::InvalidRecord),
                    "{operation:?}: {wrong:?}"
                );
            }
        }
    }
}

#[test]
fn uncommitted_lifecycle_journal_cannot_recover_completed_effects() {
    let (initial, mut j) = lifecycle_journal(NativeOperationKind::Restart);
    j.state = S::Starting;
    assert_eq!(
        j.recovery_selection(&initial, &[6; 32]),
        Err(NativeFailure::UnsafeNativeState)
    );
}

#[test]
fn recovery_required_is_not_cleared_by_journal_intent_or_a_newer_installation() {
    let mut j = journal(DesiredServerState::Running);
    j.state = S::RecoveryRequired;
    let closed = inhibited(head(DesiredServerState::Running), &j);
    assert_eq!(
        j.recovery_selection(&closed, &[6; 32]),
        Ok(RecoverySelection::RepairRequired)
    );
    assert!(j.transition(S::ActivationPrepared).is_err());
}

#[test]
fn stopped_activation_validates_existing_data_and_never_calls_migration() {
    use spaceterm_host::{
        installation::{
            MigrationProgress, ServerDataCompatibilityPort, ServerMigrationPort,
            validate_stopped_activation_data,
        },
        native_deployment::contracts::NativeManagementFuture,
    };
    use std::{
        future::Future,
        sync::atomic::{AtomicUsize, Ordering},
        task::{Context, Poll, Waker},
    };

    struct Store {
        result: Result<(), NativeFailure>,
        compatibility_calls: AtomicUsize,
        migration_calls: AtomicUsize,
    }
    impl ServerDataCompatibilityPort for Store {
        fn revalidate_read_compatibility(
            &self,
            request: NativeRequestId,
            source: DataGenerationId,
            candidate: InstallationId,
            assessment: [u8; 32],
        ) -> NativeManagementFuture<'_, ()> {
            assert_eq!(request, NativeRequestId::new([1; 16]).unwrap());
            assert_eq!(source, DataGenerationId::new(1).unwrap());
            assert_eq!(candidate, InstallationId(nz(12)));
            assert_eq!(assessment, [8; 32]);
            self.compatibility_calls.fetch_add(1, Ordering::SeqCst);
            Box::pin(async { self.result })
        }
    }
    impl ServerMigrationPort for Store {
        fn prepare_generation(
            &self,
            _: NativeRequestId,
            _: DataGenerationId,
            _: InstallationId,
        ) -> NativeManagementFuture<'_, MigrationProgress> {
            self.migration_calls.fetch_add(1, Ordering::SeqCst);
            Box::pin(async { Err(NativeFailure::UnsafeNativeState) })
        }
        fn validate_generation(
            &self,
            _: NativeRequestId,
            _: DataGenerationId,
        ) -> NativeManagementFuture<'_, MigrationProgress> {
            self.migration_calls.fetch_add(1, Ordering::SeqCst);
            Box::pin(async { Err(NativeFailure::UnsafeNativeState) })
        }
    }
    fn run_ready<T>(future: impl Future<Output = T>) -> T {
        let mut future = std::pin::pin!(future);
        match future
            .as_mut()
            .poll(&mut Context::from_waker(Waker::noop()))
        {
            Poll::Ready(value) => value,
            Poll::Pending => panic!("the controlled store returns immediately"),
        }
    }

    let initial = head(DesiredServerState::Stopped);
    let mut admitted = journal(DesiredServerState::Stopped);
    admitted.candidate_data_generation = None;
    let quiesced = admitted
        .transition(S::Quiescing)
        .unwrap()
        .transition(S::Quiesced)
        .unwrap();
    for result in [Ok(()), Err(NativeFailure::StorageUnavailable)] {
        let store = Store {
            result,
            compatibility_calls: AtomicUsize::new(0),
            migration_calls: AtomicUsize::new(0),
        };
        let ready = run_ready(validate_stopped_activation_data(&quiesced, &store));
        assert_eq!(store.compatibility_calls.load(Ordering::SeqCst), 1);
        assert_eq!(store.migration_calls.load(Ordering::SeqCst), 0);
        assert_eq!(quiesced.candidate_data_generation, None);
        if let Err(failure) = result {
            assert_eq!(ready, Err(failure));
            continue;
        }
        let ready = ready.unwrap();
        assert_eq!(
            ready.candidate_data_generation,
            Some(initial.approved_data_generation)
        );
        let published = committed(inhibited(initial.clone(), &ready), &ready);
        assert_eq!(
            published.approved_data_generation,
            initial.approved_data_generation
        );
        let complete = ready
            .transition(S::ApprovalCommitted)
            .unwrap()
            .transition(S::Completed)
            .unwrap();
        let restored = ActivationJournalRecord::decode(&complete.encode().unwrap()).unwrap();
        assert_eq!(
            restored.outcome,
            Some(RetainedNativeOutcome::ApprovedStopped)
        );
        assert_eq!(
            restored.recovery_selection(&published, &[6; 32]),
            Ok(RecoverySelection::Committed {
                installation: InstallationId(nz(12)),
                data: initial.approved_data_generation,
                desired_state: DesiredServerState::Stopped,
            })
        );
        assert!(ready.transition(S::Starting).is_err());
    }
    let mut incompatible = quiesced;
    incompatible.candidate_data_generation = Some(DataGenerationId::new(2).unwrap());
    assert_eq!(incompatible.validate(), Err(NativeFailure::InvalidRecord));
    incompatible.desired_state = DesiredServerState::Running;
    incompatible.candidate_data_generation = Some(incompatible.original_data_generation);
    assert_eq!(incompatible.validate(), Err(NativeFailure::InvalidRecord));
}

#[test]
fn migration_result_does_not_change_original_request_identity() {
    let mut admitted = journal(DesiredServerState::Running);
    admitted.candidate_data_generation = None;
    let mut ready = admitted.clone();
    ready.state = S::MigrationReady;
    ready.candidate_data_generation = Some(DataGenerationId::new(2).unwrap());
    assert_eq!(admitted.validate(), Ok(()));
    assert_eq!(ready.validate(), Ok(()));
    let restored = ActivationJournalRecord::decode(&ready.encode().unwrap()).unwrap();
    assert_eq!(restored.same_request(&admitted), Ok(()));
    let mut changed = admitted.clone();
    changed.candidate_installation = Some(InstallationId(nz(13)));
    assert_eq!(
        restored.same_request(&changed),
        Err(NativeFailure::IdentityMismatch)
    );
    changed = admitted;
    changed.assessment_digest = Some([9; 32]);
    assert_eq!(
        restored.same_request(&changed),
        Err(NativeFailure::IdentityMismatch)
    );
}

#[test]
fn staging_result_does_not_change_original_request_identity() {
    let mut admitted = journal(DesiredServerState::Running);
    admitted.operation = NativeOperationKind::Stage;
    admitted.candidate_input = Some(VerifiedCandidateId::new([4; 16]).unwrap());
    admitted.state = S::Staging;
    admitted.candidate_installation = None;
    admitted.candidate_manifest_sha256 = None;
    admitted.candidate_data_generation = None;
    let mut staged = admitted.clone();
    staged.candidate_installation = Some(InstallationId(nz(12)));
    staged.candidate_manifest_sha256 = Some([7; 32]);
    staged = staged
        .transition(S::Staged)
        .unwrap()
        .transition(S::Completed)
        .unwrap();
    let restored = ActivationJournalRecord::decode(&staged.encode().unwrap()).unwrap();
    assert_eq!(restored.same_request(&admitted), Ok(()));
    assert_eq!(
        restored.resolve_request(&admitted),
        Ok(RetainedNativeOutcome::Staged)
    );
    assert_eq!(
        admitted.resolve_request(&admitted),
        Err(NativeFailure::OutcomePending)
    );
    let mut changed = admitted;
    changed.candidate_input = Some(VerifiedCandidateId::new([9; 16]).unwrap());
    assert_eq!(
        restored.resolve_request(&changed),
        Err(NativeFailure::IdentityMismatch)
    );
}

#[test]
fn lifecycle_intent_commit_does_not_itself_admit_a_new_process() {
    for operation in [NativeOperationKind::Start, NativeOperationKind::Restart] {
        let (initial, mut j) = lifecycle_journal(operation);
        let mut published = propose_lifecycle(&initial, DesiredServerState::Running).unwrap();
        published.active_request = Some(j.request);
        published.startup_barrier = StartupBarrier::ApprovedStartOnly;
        assert_eq!(
            j.recovery_selection(&published, &[6; 32]),
            Err(NativeFailure::UnsafeNativeState)
        );
        j.state = S::Starting;
        assert_eq!(
            j.recovery_selection(&published, &[6; 32]),
            Ok(RecoverySelection::Committed {
                installation: initial.approved_installation,
                data: initial.approved_data_generation,
                desired_state: DesiredServerState::Running,
            })
        );
    }
}

#[test]
fn install_journal_cannot_restore_original_selection_after_approval_commit() {
    let initial = head(DesiredServerState::Running);
    let mut j = journal(DesiredServerState::Running);
    j.operation = NativeOperationKind::Install;
    j.candidate_input = Some(VerifiedCandidateId::new([4; 16]).unwrap());
    j.state = S::ApprovalCommitted;
    let original = inhibited(initial.clone(), &j);
    assert_eq!(
        j.recovery_selection(&original, &[6; 32]),
        Err(NativeFailure::UnsafeNativeState)
    );
    let approved = committed(original, &j);
    assert!(matches!(
        j.recovery_selection(&approved, &[6; 32]),
        Ok(RecoverySelection::Committed { .. })
    ));
}
