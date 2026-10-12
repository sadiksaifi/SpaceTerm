use super::support::*;
use spaceterm_core::retirement::*;

fn event(reducer: &mut OrganizationReducer, event: OrganizationEvent) -> CommittedReduction {
    let prepared = reducer.prepare_event(&event).unwrap();
    commit(reducer, prepared)
}
fn runtime(reducer: &OrganizationReducer, pane: PaneId) -> PaneRuntimeRecord {
    *reducer
        .snapshot()
        .runtime()
        .iter()
        .find(|v| v.pane == pane)
        .unwrap()
}
fn reserve(reducer: &mut OrganizationReducer, pane: PaneId) -> SessionKey {
    let current = runtime(reducer, pane);
    let session = SessionKey {
        pane,
        epoch: reducer.snapshot().epoch(),
        generation: current.session_generation.checked_successor().unwrap(),
    };
    let reservation = StartupReservation::new(
        pane,
        current.session_generation,
        current.control_generation,
        current.control,
        session,
    )
    .unwrap();
    let before = reducer.snapshot();
    let prepared = reducer
        .prepare_event(&OrganizationEvent::StartupReserved(reservation))
        .unwrap();
    assert_eq!(reducer.snapshot(), before);
    let result = commit(reducer, prepared);
    assert!(result.effects.is_empty());
    assert_eq!(
        replay(before, result.publication.as_ref().unwrap()),
        reducer.snapshot()
    );
    assert_eq!(
        runtime(reducer, pane).phase,
        SessionPhase::Starting {
            generation: session.generation
        }
    );
    session
}
fn start(reducer: &mut OrganizationReducer, pane: PaneId) -> SessionKey {
    let session = reserve(reducer, pane);
    event(reducer, OrganizationEvent::RuntimeStarted { session });
    session
}

#[test]
fn measured_start_reserves_generation_before_start_and_cancellation_never_reuses_it() {
    let mut reducer = reducer();
    let (_, _, pane) = create(&mut reducer, 1);
    let first = reserve(&mut reducer, pane);
    let stale = SessionKey {
        generation: first.generation.checked_successor().unwrap(),
        ..first
    };
    assert!(
        event(
            &mut reducer,
            OrganizationEvent::RuntimeStarted { session: stale }
        )
        .publication
        .is_none()
    );
    event(
        &mut reducer,
        OrganizationEvent::StartupCancelled { session: first },
    );
    assert!(matches!(
        runtime(&reducer, pane).phase,
        SessionPhase::Ended { .. }
    ));
    assert!(
        event(
            &mut reducer,
            OrganizationEvent::RuntimeStarted { session: first }
        )
        .publication
        .is_none()
    );
    let second = start(&mut reducer, pane);
    assert!(second.generation > first.generation);
    assert!(
        event(
            &mut reducer,
            OrganizationEvent::RuntimeStarted { session: second }
        )
        .publication
        .is_none()
    );
    reducer
        .retained()
        .validate(OrganizationLimits::default())
        .unwrap();
}

#[test]
fn ordinary_exit_reduces_without_any_client_or_subscriber_for_every_exit_class() {
    for class in [
        OrdinaryExitClass::Success,
        OrdinaryExitClass::Nonzero,
        OrdinaryExitClass::Signal,
    ] {
        let mut reducer = reducer();
        let (_, _, pane) = create(&mut reducer, 1);
        let session = start(&mut reducer, pane);
        let before = reducer.snapshot();
        let result = event(
            &mut reducer,
            OrganizationEvent::OrdinaryExit { session, class },
        );
        assert!(reducer.snapshot().state().workspaces.is_empty());
        assert_eq!(
            replay(before, result.publication.as_ref().unwrap()),
            reducer.snapshot()
        );
        assert_eq!(result.effects.len(), 2);
        let duplicate = event(
            &mut reducer,
            OrganizationEvent::OrdinaryExit { session, class },
        );
        assert!(duplicate.publication.is_none() && duplicate.effects.is_empty());
        reducer
            .retained()
            .validate(OrganizationLimits::default())
            .unwrap();
    }
}

#[test]
fn retained_ordinary_exit_before_runtime_started_cascades_once_and_fences_delayed_start() {
    use spaceterm_core::runtime_authority::*;
    for exit_status in [Some(0), Some(7), None] {
        let mut reducer = reducer();
        let (_, _, pane) = create(&mut reducer, 1);
        let session = reserve(&mut reducer, pane);
        let before = reducer.snapshot();
        let terminal = SessionTerminalResult::new(
            session,
            RuntimeTerminalStatus::OrdinaryExit { exit_status },
        )
        .unwrap();
        let prepared = reducer.prepare_terminal_result(terminal).unwrap();
        assert_eq!(reducer.snapshot(), before);
        let result = commit(&mut reducer, prepared);
        let after = reducer.snapshot();
        assert!(after.state().panes.is_empty());
        assert!(after.state().tabs.is_empty());
        assert!(after.state().workspaces.is_empty());
        assert_eq!(
            after.state().revision,
            before.state().revision.checked_successor().unwrap()
        );
        assert_eq!(replay(before, result.publication.as_ref().unwrap()), after);
        assert_eq!(result.effects.len(), 2);
        let delayed = event(&mut reducer, OrganizationEvent::RuntimeStarted { session });
        assert!(delayed.publication.is_none() && delayed.effects.is_empty());
        let duplicate = reducer.prepare_terminal_result(terminal).unwrap();
        let result = commit(&mut reducer, duplicate);
        assert!(result.publication.is_none() && result.effects.is_empty());
        assert_eq!(reducer.snapshot(), after);
        reducer
            .retained()
            .validate(OrganizationLimits::default())
            .unwrap();
    }
}

#[test]
fn runtime_failure_keeps_failed_pane_and_roots_and_rejects_stale_terminal_results() {
    let mut reducer = reducer();
    let (workspace, tab, pane) = create(&mut reducer, 1);
    let session = start(&mut reducer, pane);
    event(
        &mut reducer,
        OrganizationEvent::RuntimeFailed {
            session,
            class: FailureClass::ResourceLimit,
        },
    );
    let failed = reducer.snapshot();
    assert_eq!(
        runtime(&reducer, pane).phase,
        SessionPhase::Failed {
            class: FailureClass::ResourceLimit
        }
    );
    assert_eq!(failed.state().workspaces[0].id, workspace);
    assert_eq!(failed.state().tabs[0].id, tab);
    for observation in [
        OrganizationEvent::OrdinaryExit {
            session,
            class: OrdinaryExitClass::Success,
        },
        OrganizationEvent::RuntimeFailed {
            session,
            class: FailureClass::RuntimeFault,
        },
    ] {
        let result = event(&mut reducer, observation);
        assert!(result.publication.is_none());
    }
    let prepared = reducer
        .prepare_runtime_cleanup(
            SessionCleanupObservation::new(session, RetirementReason::RuntimeFailure, CONFIRMED)
                .unwrap(),
        )
        .unwrap();
    commit(&mut reducer, prepared);
    let replacement = start(&mut reducer, pane);
    assert!(replacement.generation > session.generation);
    assert!(
        event(
            &mut reducer,
            OrganizationEvent::OrdinaryExit {
                session,
                class: OrdinaryExitClass::Success
            }
        )
        .publication
        .is_none()
    );
}

#[test]
fn display_and_confirmation_use_independent_source_sequences_and_stale_sessions_are_inert() {
    let mut reducer = reducer();
    let (_, _, pane) = create(&mut reducer, 1);
    let session = start(&mut reducer, pane);
    let sequence = RuntimeObservationSequence::new(2).unwrap();
    let display = DisplayDirectoryObservation::new(
        session,
        sequence,
        ReducerTick::new(10),
        directory(b"/display"),
        DirectoryPresentation::OwningServer,
    )
    .unwrap();
    let result = event(
        &mut reducer,
        OrganizationEvent::DisplayDirectoryObserved(display),
    );
    assert!(result.publication.is_some());
    assert!(
        reducer.snapshot().state().panes[0]
            .confirmed_restoration_directory
            .value
            .is_none()
    );
    let confirmed = ConfirmedDirectoryObservation::new(
        session,
        sequence,
        ReducerTick::new(10),
        directory(b"/confirmed"),
    )
    .unwrap();
    let before = reducer.snapshot();
    let result = event(
        &mut reducer,
        OrganizationEvent::RestorationDirectoryConfirmed(confirmed.clone()),
    );
    assert_eq!(
        replay(before, result.publication.as_ref().unwrap()),
        reducer.snapshot()
    );
    assert_eq!(
        reducer.snapshot().state().panes[0].restoration_directory(),
        &directory(b"/confirmed")
    );
    assert!(
        event(
            &mut reducer,
            OrganizationEvent::RestorationDirectoryConfirmed(confirmed)
        )
        .publication
        .is_none()
    );
    let older = ConfirmedDirectoryObservation::new(
        session,
        RuntimeObservationSequence::new(1).unwrap(),
        ReducerTick::new(9),
        directory(b"/older"),
    )
    .unwrap();
    assert!(
        event(
            &mut reducer,
            OrganizationEvent::RestorationDirectoryConfirmed(older)
        )
        .publication
        .is_none()
    );
    event(
        &mut reducer,
        OrganizationEvent::RuntimeFailed {
            session,
            class: FailureClass::RuntimeFault,
        },
    );
    let prepared = reducer
        .prepare_runtime_cleanup(
            SessionCleanupObservation::new(session, RetirementReason::RuntimeFailure, CONFIRMED)
                .unwrap(),
        )
        .unwrap();
    commit(&mut reducer, prepared);
    let replacement = start(&mut reducer, pane);
    assert!(replacement.generation > session.generation);
    let stale = ConfirmedDirectoryObservation::new(
        session,
        RuntimeObservationSequence::new(3).unwrap(),
        ReducerTick::new(11),
        directory(b"/stale"),
    )
    .unwrap();
    assert!(
        event(
            &mut reducer,
            OrganizationEvent::RestorationDirectoryConfirmed(stale)
        )
        .publication
        .is_none()
    );
}

#[test]
fn close_assessment_runtime_revalidation_real_lifecycle_composition() {
    let mut reducer = reducer();
    let (_, tab, pane) = create(&mut reducer, 1);
    let session = start(&mut reducer, pane);
    let work = CloseWorkObservation::new(
        session,
        RuntimeObservationSequence::new(1).unwrap(),
        CloseWorkGeneration::new(1),
        ReducerTick::new(10),
        CloseWorkState::Idle,
    )
    .unwrap();
    assert!(
        event(&mut reducer, OrganizationEvent::CloseWorkObserved(work))
            .publication
            .is_none()
    );
    assert_eq!(reducer.close_work_observation(pane), Some(work));
    let assessed = apply(
        &mut reducer,
        Command::AssessClose {
            target: CloseTarget::Tab(tab),
        },
        2,
    );
    let assessment = match assessed.resolution {
        Resolution::Completed(Outcome::Assessment(v)) => v,
        _ => panic!("assessment missing"),
    };
    assert!(!assessment.summary().requires_confirmation());
    let changed = CloseWorkObservation::new(
        session,
        RuntimeObservationSequence::new(2).unwrap(),
        CloseWorkGeneration::new(2),
        ReducerTick::new(11),
        CloseWorkState::RunningWork,
    )
    .unwrap();
    event(&mut reducer, OrganizationEvent::CloseWorkObserved(changed));
    assert_eq!(
        reducer.validate_assessment(
            &context(3).principal,
            &assessment.reference(),
            &AssessmentTarget::Close(CloseTarget::Tab(tab)),
            ReducerTick::new(12)
        ),
        Err(AssessmentFailure::StaleAssessment)
    );
    assert!(
        event(&mut reducer, OrganizationEvent::CloseWorkObserved(work))
            .publication
            .is_none()
    );
    assert_eq!(reducer.close_work_observation(pane), Some(changed));
}

#[test]
fn repair_directory_unavailable_pane_restores_startup_eligibility_without_launching() {
    let mut reducer = reducer();
    let (workspace, _, pane) = create(&mut reducer, 1);
    let original = reducer.snapshot().state().workspaces[0].clone();
    event(
        &mut reducer,
        OrganizationEvent::DirectoryUnavailable {
            pane,
            expected_generation: SessionGeneration::new(0),
        },
    );
    assert_eq!(
        runtime(&reducer, pane).phase,
        SessionPhase::DirectoryUnavailable
    );
    let command = Command::RepairPaneDirectory {
        pane,
        expected_version: FieldVersion::new(1),
        value: directory(b"/repaired"),
    };
    let result = apply(&mut reducer, command, 2);
    assert!(result.effects.is_empty());
    assert_eq!(runtime(&reducer, pane).phase, SessionPhase::NotStarted);
    assert_eq!(
        reducer
            .snapshot()
            .state()
            .workspaces
            .iter()
            .find(|v| v.id == workspace)
            .unwrap(),
        &original
    );
    start(&mut reducer, pane);
}

use spaceterm_core::operation_ledger_contracts::*;

// The ledger implementation belongs to another track. This fake encodes only
// Applied through the frozen port and grants no durable or native authority.
struct OutcomeCodec;
impl LedgerCodec for OutcomeCodec {
    fn canonical_request_digest(
        &self,
        _: &StreamBinding,
        _: OperationId,
        _: &Command,
    ) -> Result<RequestDigest, LedgerFailure> {
        Err(LedgerFailure::new(LedgerFailureClass::InvalidRequest))
    }
    fn encode_canonical_request(
        &self,
        _: &StreamBinding,
        _: OperationId,
        _: &Command,
    ) -> Result<Vec<u8>, LedgerFailure> {
        Err(LedgerFailure::new(LedgerFailureClass::InvalidRequest))
    }
    fn decode_canonical_request(
        &self,
        _: &[u8],
    ) -> Result<CanonicalOperationRequest, LedgerFailure> {
        Err(LedgerFailure::new(LedgerFailureClass::InvalidRequest))
    }
    fn encode_compact_outcome(&self, outcome: &Outcome) -> Result<Vec<u8>, LedgerFailure> {
        if *outcome == Outcome::Applied {
            Ok(vec![0xa5])
        } else {
            Err(LedgerFailure::new(
                LedgerFailureClass::InvalidCompactOutcome,
            ))
        }
    }
    fn decode_compact_outcome(&self, bytes: &[u8]) -> Result<Outcome, LedgerFailure> {
        if bytes == [0xa5] {
            Ok(Outcome::Applied)
        } else {
            Err(LedgerFailure::new(
                LedgerFailureClass::InvalidCompactOutcome,
            ))
        }
    }
}
fn retiring_context(sequence: u64) -> ReductionContext {
    let mut context = context(sequence);
    context.facts.retirement_outcome = Some(AppliedRetirementOutcome::new(&OutcomeCodec).unwrap());
    context
}
fn assess_end_all(reducer: &mut OrganizationReducer, sequence: u64) -> AssessmentRef {
    let result = apply(
        reducer,
        Command::AssessEndAllSessions { server: SERVER },
        sequence,
    );
    match result.resolution {
        Resolution::Completed(Outcome::Assessment(v)) => v.reference(),
        _ => panic!("assessment missing"),
    }
}
fn end_all(
    reducer: &mut OrganizationReducer,
    assessment: AssessmentRef,
    sequence: u64,
) -> CommittedReduction {
    let prepared = reducer
        .prepare_command(
            &Command::EndAllSessions {
                server: SERVER,
                assessment,
            },
            &retiring_context(sequence),
        )
        .unwrap();
    commit(reducer, prepared)
}
fn plan(result: &CommittedReduction) -> &RetirementDescriptor {
    match &result.resolution {
        Resolution::AwaitingRetirement(plan) => &plan.descriptor,
        _ => panic!("retirement missing"),
    }
}
const CONFIRMED: CleanupResult = CleanupResult::Confirmed(ConfirmedCleanupClass::ReapedAndReleased);

#[test]
fn end_all_preserves_organization_and_disconnected_control_grants_and_invalidates_every_pane() {
    let mut reducer = reducer();
    let (_, _, first) = create(&mut reducer, 1);
    let (_, _, second) = create(&mut reducer, 2);
    let session = start(&mut reducer, first);
    let before = reducer.snapshot();
    let handle = assess_end_all(&mut reducer, 3);
    let result = end_all(&mut reducer, handle, 4);
    let descriptor = plan(&result);
    assert_eq!(descriptor.sessions()[0].session(), session);
    assert_eq!(
        descriptor.acceptance_revision(),
        reducer.snapshot().state().revision
    );
    assert_eq!(descriptor.binding(), retiring_context(4).binding());
    assert_eq!(
        descriptor.sessions()[0].first_reason(),
        RetirementReason::EndAllSessions
    );
    assert_eq!(
        descriptor.sessions()[0].cleanup(),
        RetirementCleanupState::AwaitingEvidence
    );
    assert_eq!(
        replay(before.clone(), result.publication.as_ref().unwrap()),
        reducer.snapshot()
    );
    assert_eq!(
        reducer.snapshot().state().workspaces,
        before.state().workspaces
    );
    assert_eq!(reducer.snapshot().state().tabs, before.state().tabs);
    assert_eq!(reducer.snapshot().state().panes, before.state().panes);
    for pane in [first, second] {
        let prior = *before.runtime().iter().find(|v| v.pane == pane).unwrap();
        let current = runtime(&reducer, pane);
        assert_eq!(current.control, prior.control);
        assert_eq!(
            current.session_generation,
            prior.session_generation.checked_successor().unwrap()
        );
        assert_eq!(
            current.phase,
            SessionPhase::Ended {
                reason: EndReason::EndAllSessions
            }
        );
    }
    assert_eq!(result.effects.len(), 5);
    assert!(
        result
            .publication
            .as_ref()
            .unwrap()
            .writes()
            .iter()
            .all(|v| matches!(
                v,
                OrganizationWrite::Retained(RetainedOrganizationWrite::SetPaneGenerationHighWater(
                    _
                ))
            ))
    );
    let result = apply(
        &mut reducer,
        Command::CreateWorkspace {
            server: SERVER,
            source: WorkspaceCreationSource::ServerHome { server: SERVER },
            name: NameDraft::Automatic,
        },
        5,
    );
    assert!(matches!(
        result.resolution,
        Resolution::Completed(Outcome::Failed(OperationFailure::BusyRetiring))
    ));
    assert!(
        event(
            &mut reducer,
            OrganizationEvent::OrdinaryExit {
                session,
                class: OrdinaryExitClass::Success
            }
        )
        .publication
        .is_none()
    );
    assert_eq!(reducer.snapshot().state().panes.len(), 2);
}

#[test]
fn exit_race_before_end_all_acceptance_requires_reassessment() {
    let mut reducer = reducer();
    let (_, _, pane) = create(&mut reducer, 1);
    let session = start(&mut reducer, pane);
    let handle = assess_end_all(&mut reducer, 2);
    event(
        &mut reducer,
        OrganizationEvent::OrdinaryExit {
            session,
            class: OrdinaryExitClass::Success,
        },
    );
    let rejected = end_all(&mut reducer, handle, 3);
    assert!(matches!(
        rejected.resolution,
        Resolution::Completed(Outcome::Failed(OperationFailure::StaleAssessment))
    ));
    assert!(rejected.publication.is_none() && rejected.effects.is_empty());
}

#[test]
fn end_all_remains_pending_until_every_cleanup_is_confirmed() {
    let mut reducer = reducer();
    let (_, _, first) = create(&mut reducer, 1);
    let (_, _, second) = create(&mut reducer, 2);
    let a = start(&mut reducer, first);
    let b = reserve(&mut reducer, second);
    let handle = assess_end_all(&mut reducer, 3);
    let result = end_all(&mut reducer, handle, 4);
    let descriptor = plan(&result).clone();
    assert_eq!(descriptor.sessions().len(), 2);
    let before = reducer.snapshot();
    event(
        &mut reducer,
        OrganizationEvent::RetirementCleanup {
            batch: descriptor.batch(),
            session: a,
            result: CONFIRMED,
        },
    );
    assert!(!reducer.retained().pending_retirements()[0].cleanup_confirmed());
    let failed = CleanupResult::Failed(FailureClass::RuntimeFault);
    event(
        &mut reducer,
        OrganizationEvent::RetirementCleanup {
            batch: descriptor.batch(),
            session: b,
            result: failed,
        },
    );
    assert!(!reducer.retained().pending_retirements()[0].cleanup_confirmed());
    let duplicate = event(
        &mut reducer,
        OrganizationEvent::RetirementCleanup {
            batch: descriptor.batch(),
            session: b,
            result: CONFIRMED,
        },
    );
    assert!(duplicate.effects.is_empty() && duplicate.publication.is_none());
    assert_eq!(
        reducer.retained().pending_retirements()[0].sessions()[1].cleanup(),
        RetirementCleanupState::Failed(FailureClass::RuntimeFault)
    );
    assert_eq!(reducer.snapshot(), before);
}

fn cleanup(reducer: &mut OrganizationReducer, descriptor: &RetirementDescriptor) {
    for session in descriptor.sessions() {
        event(
            reducer,
            OrganizationEvent::RetirementCleanup {
                batch: descriptor.batch(),
                session: session.session(),
                result: CONFIRMED,
            },
        );
    }
}
fn complete(reducer: &mut OrganizationReducer, batch: RetirementBatchId) -> CommittedReduction {
    let prepared = reducer.prepare_retirement_completion(batch).unwrap();
    commit(reducer, prepared)
}
fn native_request(index: u8) -> spaceterm_core::native_management::NativeManagementRequestRef {
    spaceterm_core::native_management::NativeManagementRequestRef::new(
        SERVER,
        [index; 16],
        RequestDigest::from_bytes([index; 32]),
    )
    .unwrap()
}
fn stop(reducer: &mut OrganizationReducer, index: u8) -> CommittedReduction {
    let prepared = reducer
        .prepare_native_retirement(
            native_request(index),
            spaceterm_core::native_management::NativeRetirementAction::Stop,
            ReducerTick::new(12),
        )
        .unwrap();
    commit(reducer, prepared)
}
fn native_plan(result: &CommittedReduction) -> &NativeRetirementDescriptor {
    match &result.resolution {
        Resolution::AwaitingNativeRetirement(v) => v,
        _ => panic!("native retirement missing"),
    }
}

#[test]
fn completion_preserves_original_acceptance_revision_and_does_not_publish_or_restart() {
    let mut reducer = reducer();
    let (workspace, _, pane) = create(&mut reducer, 1);
    let session = start(&mut reducer, pane);
    let handle = assess_end_all(&mut reducer, 2);
    let accepted = end_all(&mut reducer, handle, 3);
    let descriptor = plan(&accepted).clone();
    assert_eq!(
        reducer
            .prepare_retirement_completion(descriptor.batch())
            .err(),
        Some(ReducerFailure::InvalidFacts)
    );
    apply(
        &mut reducer,
        Command::RenameWorkspace {
            workspace,
            expected_version: FieldVersion::new(1),
            draft: NameDraft::Custom(NameText::new("renamed".into()).unwrap()),
        },
        4,
    );
    assert!(reducer.snapshot().state().revision > descriptor.acceptance_revision());
    cleanup(&mut reducer, &descriptor);
    let before = reducer.snapshot();
    let prepared = reducer
        .prepare_retirement_completion(descriptor.batch())
        .unwrap();
    assert_eq!(reducer.retained().pending_retirements().len(), 1);
    let completed = commit(&mut reducer, prepared);
    assert!(completed.publication.is_none() && completed.effects.is_empty());
    match completed.resolution {
        Resolution::RetirementCompleted(v) => {
            assert_eq!(
                v.descriptor.acceptance_revision(),
                descriptor.acceptance_revision()
            );
            assert_eq!(v.descriptor.binding(), descriptor.binding());
            assert_eq!(
                v.descriptor
                    .parts()
                    .eventual_result
                    .outcome(&OutcomeCodec)
                    .unwrap(),
                Outcome::Applied
            );
        }
        _ => panic!("completion missing"),
    }
    assert_eq!(reducer.snapshot(), before);
    assert!(reducer.retained().pending_retirements().is_empty());
    let replacement = start(&mut reducer, pane);
    assert!(replacement.generation > session.generation);
    let late = event(
        &mut reducer,
        OrganizationEvent::RetirementCleanup {
            batch: descriptor.batch(),
            session,
            result: CONFIRMED,
        },
    );
    assert!(late.publication.is_none() && late.effects.is_empty());
    assert!(matches!(
        runtime(&reducer, pane).phase,
        SessionPhase::Running { .. }
    ));
}

#[test]
fn stop_overlap_keeps_first_owner_reason_and_shares_cleanup_without_overwriting_stop_phase() {
    let mut reducer = reducer();
    let (_, _, pane) = create(&mut reducer, 1);
    let session = start(&mut reducer, pane);
    let handle = assess_end_all(&mut reducer, 2);
    let accepted = end_all(&mut reducer, handle, 3);
    let first = plan(&accepted).clone();
    let native = stop(&mut reducer, 1);
    let second = native_plan(&native).clone();
    assert_eq!(second.sessions()[0].session(), session);
    assert_eq!(
        second.sessions()[0].first_reason(),
        RetirementReason::EndAllSessions
    );
    assert!(
        !native
            .effects
            .iter()
            .any(|v| matches!(v, OrganizationEffect::RetireSession { .. }))
    );
    assert_eq!(
        runtime(&reducer, pane).phase,
        SessionPhase::Ended {
            reason: EndReason::Stop
        }
    );
    assert!(
        event(
            &mut reducer,
            OrganizationEvent::OrdinaryExit {
                session,
                class: OrdinaryExitClass::Nonzero
            }
        )
        .publication
        .is_none()
    );
    let before = reducer.snapshot();
    let cleanup = event(
        &mut reducer,
        OrganizationEvent::RetirementCleanup {
            batch: first.batch(),
            session,
            result: CONFIRMED,
        },
    );
    match cleanup.resolution {
        Resolution::RetirementProgress { operations, native } => {
            assert_eq!(operations.len(), 1);
            assert_eq!(native.len(), 1);
        }
        _ => panic!("shared progress missing"),
    }
    assert_eq!(reducer.snapshot(), before);
    assert!(reducer.retained().pending_retirements()[0].cleanup_confirmed());
    assert!(reducer.retained().native_retirements()[0].cleanup_confirmed());
    complete(&mut reducer, first.batch());
    let result = complete(&mut reducer, second.batch());
    assert!(result.publication.is_none() && result.effects.is_empty());
    assert_eq!(
        runtime(&reducer, pane).phase,
        SessionPhase::Ended {
            reason: EndReason::Stop
        }
    );
    let current = runtime(&reducer, pane);
    let reservation = StartupReservation::new(
        pane,
        current.session_generation,
        current.control_generation,
        current.control,
        SessionKey {
            pane,
            epoch: reducer.snapshot().epoch(),
            generation: current.session_generation.checked_successor().unwrap(),
        },
    )
    .unwrap();
    let blocked = event(
        &mut reducer,
        OrganizationEvent::StartupReserved(reservation),
    );
    assert!(matches!(
        blocked.resolution,
        Resolution::Completed(Outcome::Failed(OperationFailure::BusyRetiring))
    ));
}

fn live_close(
    reducer: &mut OrganizationReducer,
    pane: PaneId,
    sequence: u64,
) -> RetirementDescriptor {
    let assessed = apply(
        reducer,
        Command::AssessClose {
            target: CloseTarget::Pane(pane),
        },
        sequence,
    );
    let handle = match assessed.resolution {
        Resolution::Completed(Outcome::Assessment(v)) => v.reference(),
        _ => panic!("assessment missing"),
    };
    let prepared = reducer
        .prepare_command(
            &Command::CloseConfirmed {
                target: CloseTarget::Pane(pane),
                assessment: handle,
            },
            &retiring_context(sequence + 1),
        )
        .unwrap();
    plan(&commit(reducer, prepared)).clone()
}

#[test]
fn end_all_assessment_revalidates_already_retiring_session_set() {
    let mut reducer = reducer();
    let (_, _, pane) = create(&mut reducer, 1);
    start(&mut reducer, pane);
    let close = live_close(&mut reducer, pane, 2);
    assert!(reducer.snapshot().state().panes.is_empty());
    let handle = assess_end_all(&mut reducer, 4);
    cleanup(&mut reducer, &close);
    complete(&mut reducer, close.batch());
    let rejected = end_all(&mut reducer, handle, 5);
    assert!(matches!(
        rejected.resolution,
        Resolution::Completed(Outcome::Failed(OperationFailure::StaleAssessment))
    ));
    assert!(rejected.publication.is_none() && rejected.effects.is_empty());
}

#[test]
fn end_all_over_deleted_retiring_sessions_awaits_exact_evidence_without_second_process_effect() {
    let mut reducer = reducer();
    let (_, _, pane) = create(&mut reducer, 1);
    let session = start(&mut reducer, pane);
    let close = live_close(&mut reducer, pane, 2);
    let assessed = apply(
        &mut reducer,
        Command::AssessEndAllSessions { server: SERVER },
        4,
    );
    let assessment = match assessed.resolution {
        Resolution::Completed(Outcome::Assessment(v)) => v,
        _ => panic!("assessment missing"),
    };
    assert_eq!(assessment.summary().live_session_count(), 1);
    assert_eq!(assessment.summary().pane_count(), 1);
    let result = end_all(&mut reducer, assessment.reference(), 5);
    let end = plan(&result).clone();
    assert_eq!(end.sessions()[0].session(), session);
    assert_eq!(
        end.sessions()[0].first_reason(),
        RetirementReason::ConfirmedClose
    );
    assert!(result.effects.is_empty() && result.publication.is_none());
    event(
        &mut reducer,
        OrganizationEvent::RetirementCleanup {
            batch: end.batch(),
            session,
            result: CONFIRMED,
        },
    );
    assert!(
        reducer
            .retained()
            .pending_retirements()
            .iter()
            .all(RetirementDescriptor::cleanup_confirmed)
    );
    complete(&mut reducer, close.batch());
    complete(&mut reducer, end.batch());
    assert!(reducer.snapshot().state().workspaces.is_empty());
}

#[test]
fn accepted_close_recovery_cannot_infer_cleanup_from_missing_handles_and_completion_keeps_deletion()
{
    let mut reducer = reducer();
    let (_, _, pane) = create(&mut reducer, 1);
    let session = start(&mut reducer, pane);
    let descriptor = live_close(&mut reducer, pane, 2);
    let retained = reducer.retained();
    let mut recovered = OrganizationReducer::from_recovered(
        retained,
        reducer.store_revision(),
        ServerEpoch::new(10),
        OrganizationLimits::default(),
    )
    .unwrap();
    assert!(recovered.snapshot().state().panes.is_empty());
    assert_eq!(
        recovered
            .prepare_retirement_completion(descriptor.batch())
            .err(),
        Some(ReducerFailure::InvalidFacts)
    );
    event(
        &mut recovered,
        OrganizationEvent::OrdinaryExit {
            session,
            class: OrdinaryExitClass::Success,
        },
    );
    assert!(!recovered.retained().pending_retirements()[0].cleanup_confirmed());
    event(
        &mut recovered,
        OrganizationEvent::RetirementCleanup {
            batch: descriptor.batch(),
            session,
            result: CleanupResult::Confirmed(
                ConfirmedCleanupClass::AlreadyAbsentWithConfirmedOwnership,
            ),
        },
    );
    let completed = complete(&mut recovered, descriptor.batch());
    assert!(completed.publication.is_none() && completed.effects.is_empty());
    assert!(recovered.snapshot().state().workspaces.is_empty());
}

#[test]
fn end_all_without_live_sessions_still_commits_ended_state_and_completes_without_effects() {
    for saved_pane in [false, true] {
        let mut reducer = reducer();
        let pane = saved_pane.then(|| create(&mut reducer, 1).2);
        let handle = assess_end_all(&mut reducer, 2);
        let before = reducer.snapshot();
        let result = end_all(&mut reducer, handle, 3);
        let descriptor = plan(&result).clone();
        assert_eq!(descriptor.assessment(), handle);
        assert_eq!(descriptor.panes().len(), usize::from(saved_pane));
        assert!(descriptor.sessions().is_empty() && descriptor.cleanup_confirmed());
        assert_eq!(
            descriptor.acceptance_revision(),
            reducer.snapshot().state().revision
        );
        assert_eq!(
            reducer.snapshot().state().workspaces,
            before.state().workspaces
        );
        assert_eq!(reducer.snapshot().state().tabs, before.state().tabs);
        assert_eq!(reducer.snapshot().state().panes, before.state().panes);
        if let Some(pane) = pane {
            assert_eq!(runtime(&reducer, pane).control, before.runtime()[0].control);
            assert_eq!(
                runtime(&reducer, pane).phase,
                SessionPhase::Ended {
                    reason: EndReason::EndAllSessions
                }
            );
            assert_eq!(
                replay(before, result.publication.as_ref().unwrap()),
                reducer.snapshot()
            );
        } else {
            assert!(result.publication.is_none());
            assert_eq!(reducer.snapshot(), before);
        }
        assert_eq!(result.effects.len(), usize::from(saved_pane) * 2);
        let ended = reducer.snapshot();
        let completed = complete(&mut reducer, descriptor.batch());
        assert!(completed.publication.is_none() && completed.effects.is_empty());
        assert!(matches!(
            completed.resolution,
            Resolution::RetirementCompleted(plan) if plan.descriptor == descriptor
        ));
        assert_eq!(reducer.snapshot(), ended);
        assert!(reducer.retained().pending_retirements().is_empty());
        if let Some(pane) = pane {
            start(&mut reducer, pane);
        } else {
            create(&mut reducer, 4);
        }
    }
}

#[test]
fn native_stop_without_live_sessions_records_acceptance_and_completes_store_only() {
    for saved_pane in [false, true] {
        let mut reducer = reducer();
        let pane = saved_pane.then(|| create(&mut reducer, 1).2);
        let before = reducer.snapshot();
        let result = stop(&mut reducer, 1);
        let descriptor = native_plan(&result).clone();
        assert_eq!(descriptor.request(), native_request(1));
        assert_eq!(descriptor.panes().len(), usize::from(saved_pane));
        assert!(descriptor.sessions().is_empty() && descriptor.cleanup_confirmed());
        assert_eq!(
            descriptor.acceptance_revision(),
            reducer.snapshot().state().revision
        );
        assert_eq!(
            reducer.snapshot().state().workspaces,
            before.state().workspaces
        );
        assert_eq!(reducer.snapshot().state().tabs, before.state().tabs);
        assert_eq!(reducer.snapshot().state().panes, before.state().panes);
        if let Some(pane) = pane {
            assert_eq!(runtime(&reducer, pane).control, before.runtime()[0].control);
            assert_eq!(
                runtime(&reducer, pane).phase,
                SessionPhase::Ended {
                    reason: EndReason::Stop
                }
            );
            assert_eq!(
                replay(before, result.publication.as_ref().unwrap()),
                reducer.snapshot()
            );
        } else {
            assert!(result.publication.is_none());
            assert_eq!(reducer.snapshot(), before);
        }
        assert_eq!(result.effects.len(), usize::from(saved_pane) * 2);
        let ended = reducer.snapshot();
        let completed = complete(&mut reducer, descriptor.batch());
        assert!(completed.publication.is_none() && completed.effects.is_empty());
        assert!(matches!(
            completed.resolution,
            Resolution::NativeRetirementCompleted(value) if *value == descriptor
        ));
        assert_eq!(reducer.snapshot(), ended);
        assert!(reducer.retained().native_retirements().is_empty());
        let rejected = apply(
            &mut reducer,
            Command::CreateWorkspace {
                server: SERVER,
                source: WorkspaceCreationSource::ServerHome { server: SERVER },
                name: NameDraft::Automatic,
            },
            2,
        );
        assert_eq!(
            rejected.resolution,
            Resolution::Completed(Outcome::Failed(OperationFailure::BusyRetiring))
        );
        assert!(rejected.publication.is_none() && rejected.effects.is_empty());
    }
}

#[test]
fn native_stop_uses_real_identity_projection_and_keeps_saved_organization_after_cleanup() {
    let mut reducer = reducer();
    let (_, _, pane) = create(&mut reducer, 1);
    let session = start(&mut reducer, pane);
    let before = reducer.snapshot();
    assert_eq!(
        reducer.prepare_stop(ReducerTick::new(10)).err(),
        Some(ReducerFailure::InvalidFacts)
    );
    let prepared = reducer
        .prepare_native_retirement(
            native_request(2),
            spaceterm_core::native_management::NativeRetirementAction::Stop,
            ReducerTick::new(10),
        )
        .unwrap();
    assert!(matches!(
        prepared.writes().operation_writes(),
        [OperationWrite::NativeRetirement(
            NativeRetirementMutation::Accept(_)
        )]
    ));
    assert_eq!(reducer.snapshot(), before);
    let result = commit(&mut reducer, prepared);
    let descriptor = native_plan(&result).clone();
    assert_eq!(descriptor.request(), native_request(2));
    assert_eq!(
        descriptor.sessions()[0].first_reason(),
        RetirementReason::Stop
    );
    assert_eq!(
        replay(before.clone(), result.publication.as_ref().unwrap()),
        reducer.snapshot()
    );
    assert_eq!(reducer.snapshot().state().panes, before.state().panes);
    let prepared = reducer
        .prepare_event(&OrganizationEvent::RetirementCleanup {
            batch: descriptor.batch(),
            session,
            result: CONFIRMED,
        })
        .unwrap();
    match &prepared.writes().operation_writes()[0] {
        OperationWrite::NativeRetirement(v) => v.validate().unwrap(),
        _ => panic!("native progress missing"),
    }
    commit(&mut reducer, prepared);
    assert!(
        complete(&mut reducer, descriptor.batch())
            .publication
            .is_none()
    );
    assert!(matches!(
        runtime(&reducer, pane).phase,
        SessionPhase::Ended {
            reason: EndReason::Stop
        }
    ));
}

#[test]
fn recovery_rejects_overlapping_descriptors_with_different_first_reason_or_cleanup() {
    for changed_cleanup in [false, true] {
        let mut reducer = reducer();
        let (_, _, pane) = create(&mut reducer, 1);
        start(&mut reducer, pane);
        let handle = assess_end_all(&mut reducer, 2);
        end_all(&mut reducer, handle, 3);
        let stopped = stop(&mut reducer, 1);
        let mut parts = native_plan(&stopped).parts().clone();
        let old = parts.sessions[0];
        parts.sessions[0] = RetiredSession::new(
            old.session(),
            if changed_cleanup {
                old.first_reason()
            } else {
                RetirementReason::Stop
            },
            if changed_cleanup {
                RetirementCleanupState::Confirmed(ConfirmedCleanupClass::ReapedAndReleased)
            } else {
                old.cleanup()
            },
        )
        .unwrap();
        let invalid = reducer
            .retained()
            .with_native_retirements(vec![NativeRetirementDescriptor::new(parts).unwrap()]);
        assert_eq!(
            invalid.validate(OrganizationLimits::default()),
            Err(ReducerFailure::InvalidDurableState(
                DurableInvariant::Retirement
            ))
        );
    }
}

#[test]
fn recovery_rejects_native_invalidation_above_retained_numeric_generation() {
    let mut reducer = reducer();
    let (_, _, pane) = create(&mut reducer, 1);
    start(&mut reducer, pane);
    let stopped = stop(&mut reducer, 1);
    let mut parts = native_plan(&stopped).parts().clone();
    let old = parts.panes[0];
    parts.panes[0] = RetirementPane::new(
        old.workspace(),
        old.tab(),
        pane,
        old.worktree(),
        old.invalidation_generation().checked_successor().unwrap(),
    )
    .unwrap();
    let invalid = reducer
        .retained()
        .with_native_retirements(vec![NativeRetirementDescriptor::new(parts).unwrap()]);
    assert_eq!(
        invalid.validate(OrganizationLimits::default()),
        Err(ReducerFailure::InvalidDurableState(
            DurableInvariant::Retirement
        ))
    );
}

#[test]
fn retirement_count_byte_and_runtime_session_limits_reject_before_fencing() {
    for limit in [
        OrganizationLimits {
            retirement_batches: 0,
            ..OrganizationLimits::default()
        },
        OrganizationLimits {
            retirement_bytes: 496,
            ..OrganizationLimits::default()
        },
    ] {
        let mut reducer = OrganizationReducer::from_recovered(
            empty(),
            StoreRevision::new(0),
            ServerEpoch::new(9),
            limit,
        )
        .unwrap();
        let (_, _, pane) = create(&mut reducer, 1);
        start(&mut reducer, pane);
        let handle = assess_end_all(&mut reducer, 2);
        let before = reducer.snapshot();
        let batch = reducer.next_retirement_batch().unwrap();
        let result = end_all(&mut reducer, handle, 3);
        assert!(matches!(
            result.resolution,
            Resolution::Completed(Outcome::Failed(OperationFailure::Capacity))
        ));
        assert!(result.effects.is_empty() && result.publication.is_none());
        assert_eq!(reducer.snapshot(), before);
        assert_eq!(reducer.next_retirement_batch().unwrap(), batch);
    }
    for count in [64, 65] {
        let mut reducer = reducer();
        let (_, tab, source) = create(&mut reducer, 1);
        let mut panes = vec![source];
        for sequence in 2..=count {
            apply(
                &mut reducer,
                Command::CreatePane {
                    tab,
                    source,
                    axis: SplitAxis::Horizontal,
                    edge: SplitEdge::After,
                },
                sequence,
            );
            panes.push(reducer.snapshot().state().panes.last().unwrap().id);
        }
        for pane in panes {
            start(&mut reducer, pane);
        }
        let handle = assess_end_all(&mut reducer, count + 1);
        let before = reducer.snapshot();
        let result = end_all(&mut reducer, handle, count + 2);
        if count == 64 {
            assert_eq!(plan(&result).sessions().len(), 64);
            assert_eq!(
                result
                    .effects
                    .iter()
                    .filter(|v| matches!(v, OrganizationEffect::RetireSession { .. }))
                    .count(),
                64
            );
        } else {
            assert!(matches!(
                result.resolution,
                Resolution::Completed(Outcome::Failed(OperationFailure::Capacity))
            ));
            assert_eq!(reducer.snapshot(), before);
            assert!(result.effects.is_empty() && result.publication.is_none());
        }
    }
}

#[test]
fn generation_exhaustion_rejects_entire_server_retirement_before_any_effect() {
    let mut seed = reducer();
    create(&mut seed, 1);
    create(&mut seed, 2);
    let retained = seed.retained();
    let mut org = retained.organization().clone();
    let mut facts = org.retained.as_ref().unwrap().parts().clone();
    let pane = facts.pane_generations[1].pane();
    facts.pane_generations[1] = PaneGenerationHighWater::new(
        pane,
        SessionGeneration::new(u64::MAX),
        ControlGeneration::new(1),
    );
    org.retained = Some(OrganizationRetainedFacts::new(facts).unwrap());
    let retained =
        RetainedOrganization::new(org, retained.high_water(), retained.server_home().clone());
    let mut reducer = OrganizationReducer::from_recovered(
        retained,
        seed.store_revision(),
        ServerEpoch::new(10),
        OrganizationLimits::default(),
    )
    .unwrap();
    let handle = assess_end_all(&mut reducer, 3);
    let before = reducer.snapshot();
    assert_eq!(
        reducer
            .prepare_command(
                &Command::EndAllSessions {
                    server: SERVER,
                    assessment: handle
                },
                &retiring_context(4)
            )
            .err(),
        Some(ReducerFailure::CounterExhausted)
    );
    assert_eq!(
        reducer
            .prepare_native_retirement(
                native_request(1),
                spaceterm_core::native_management::NativeRetirementAction::Stop,
                ReducerTick::new(10)
            )
            .err(),
        Some(ReducerFailure::CounterExhausted)
    );
    assert_eq!(reducer.snapshot(), before);
    assert!(reducer.retained().pending_retirements().is_empty());
}

#[test]
fn startup_control_and_commit_races_cannot_resurrect_or_launch_old_reservations() {
    let mut reducer = reducer();
    let (_, _, pane) = create(&mut reducer, 1);
    let first = runtime(&reducer, pane);
    let reserved = StartupReservation::new(
        pane,
        first.session_generation,
        first.control_generation,
        first.control,
        SessionKey {
            pane,
            epoch: reducer.snapshot().epoch(),
            generation: SessionGeneration::new(1),
        },
    )
    .unwrap();
    let stale = reducer
        .prepare_event(&OrganizationEvent::StartupReserved(reserved))
        .unwrap();
    event(
        &mut reducer,
        OrganizationEvent::ControlChanged {
            pane,
            expected: first.control,
            replacement: ControlRecord::Held {
                client: ClientId::new(SERVER, 2).unwrap(),
                generation: first.control_generation.checked_successor().unwrap(),
            },
        },
    );
    let receipt = CommitReceipt {
        store_revision: stale.expected_store_revision().checked_successor().unwrap(),
        organization_revision: stale.resulting_organization_revision(),
    };
    assert_eq!(
        reducer.apply_committed(stale, &receipt).err(),
        Some(ReducerFailure::StalePreparation)
    );
    let blocked = event(&mut reducer, OrganizationEvent::StartupReserved(reserved));
    assert!(blocked.effects.is_empty() && blocked.publication.is_none());
    assert_eq!(runtime(&reducer, pane).phase, SessionPhase::NotStarted);
    let session = reserve(&mut reducer, pane);
    let starting = runtime(&reducer, pane);
    event(
        &mut reducer,
        OrganizationEvent::ControlChanged {
            pane,
            expected: starting.control,
            replacement: ControlRecord::Unclaimed,
        },
    );
    assert!(
        event(&mut reducer, OrganizationEvent::RuntimeStarted { session })
            .publication
            .is_none()
    );
    event(
        &mut reducer,
        OrganizationEvent::StartupCancelled { session },
    );
    assert!(matches!(
        runtime(&reducer, pane).phase,
        SessionPhase::Ended { .. }
    ));
}

#[test]
fn generated_mixed_creation_field_deletion_lifecycle_retirement_sequence_replays() {
    for seed in [7u64, 29, 521] {
        let mut random = seed;
        let mut reducer = reducer();
        let mut replica = reducer.snapshot();
        let mut sequence = 1;
        for step in 0..80 {
            random = random.wrapping_mul(6364136223846793005).wrapping_add(1);
            let snapshot = reducer.snapshot();
            let result = if snapshot.state().panes.is_empty() {
                let command = Command::CreateWorkspace {
                    server: SERVER,
                    source: WorkspaceCreationSource::ServerHome { server: SERVER },
                    name: NameDraft::Automatic,
                };
                apply(&mut reducer, command, sequence)
            } else {
                let pane =
                    snapshot.state().panes[(random as usize) % snapshot.state().panes.len()].id;
                let tab = snapshot
                    .state()
                    .panes
                    .iter()
                    .find(|v| v.id == pane)
                    .unwrap()
                    .tab;
                let workspace = snapshot
                    .state()
                    .tabs
                    .iter()
                    .find(|v| v.id == tab)
                    .unwrap()
                    .workspace;
                let generation = runtime(&reducer, pane).session_generation;
                match (random >> 32) % 7 {
                    0 => apply(
                        &mut reducer,
                        Command::CreatePane {
                            tab,
                            source: pane,
                            axis: SplitAxis::Vertical,
                            edge: SplitEdge::Before,
                        },
                        sequence,
                    ),
                    1 => apply(
                        &mut reducer,
                        Command::RenameWorkspace {
                            workspace,
                            expected_version: snapshot
                                .state()
                                .workspaces
                                .iter()
                                .find(|v| v.id == workspace)
                                .unwrap()
                                .name
                                .version,
                            draft: NameDraft::Custom(
                                NameText::new(format!("Workspace {step}")).unwrap(),
                            ),
                        },
                        sequence,
                    ),
                    2 if matches!(runtime(&reducer, pane).phase, SessionPhase::Running { .. }) => {
                        event(
                            &mut reducer,
                            OrganizationEvent::OrdinaryExit {
                                session: SessionKey {
                                    pane,
                                    epoch: snapshot.epoch(),
                                    generation,
                                },
                                class: OrdinaryExitClass::Nonzero,
                            },
                        )
                    }
                    3 if matches!(runtime(&reducer, pane).phase, SessionPhase::Running { .. }) => {
                        event(
                            &mut reducer,
                            OrganizationEvent::RuntimeFailed {
                                session: SessionKey {
                                    pane,
                                    epoch: snapshot.epoch(),
                                    generation,
                                },
                                class: FailureClass::RuntimeFault,
                            },
                        )
                    }
                    4 => {
                        let handle = assess_end_all(&mut reducer, sequence);
                        sequence += 1;
                        let result = end_all(&mut reducer, handle, sequence);
                        let descriptor = plan(&result).clone();
                        cleanup(&mut reducer, &descriptor);
                        let completed = complete(&mut reducer, descriptor.batch());
                        assert!(
                            completed.publication.is_none() && completed.effects.is_empty(),
                            "seed {seed}"
                        );
                        result
                    }
                    _ if !matches!(runtime(&reducer, pane).phase, SessionPhase::Running { .. }) => {
                        let current = runtime(&reducer, pane);
                        let session = SessionKey {
                            pane,
                            epoch: snapshot.epoch(),
                            generation: current.session_generation.checked_successor().unwrap(),
                        };
                        let reservation = StartupReservation::new(
                            pane,
                            current.session_generation,
                            current.control_generation,
                            current.control,
                            session,
                        )
                        .unwrap();
                        let reserved = event(
                            &mut reducer,
                            OrganizationEvent::StartupReserved(reservation),
                        );
                        if let Some(delta) = reserved.publication {
                            replica = replay(replica, &delta);
                        }
                        event(&mut reducer, OrganizationEvent::RuntimeStarted { session })
                    }
                    _ => apply(
                        &mut reducer,
                        Command::CreateTab {
                            workspace,
                            source: TabCreationSource::Pane(pane),
                        },
                        sequence,
                    ),
                }
            };
            sequence += 1;
            if let Some(delta) = result.publication {
                replica = replay(replica, &delta);
            }
            assert_eq!(replica, reducer.snapshot(), "seed {seed}");
            assert!(
                reducer
                    .retained()
                    .validate(OrganizationLimits::default())
                    .is_ok(),
                "seed {seed}"
            );
        }
    }
}

#[test]
fn retained_terminal_failure_and_independent_cleanup_never_reduce_failed_pane_as_ordinary_exit() {
    use spaceterm_core::runtime_authority::*;
    for startup in [false, true] {
        let mut reducer = reducer();
        let (_, _, pane) = create(&mut reducer, 1);
        let session = if startup {
            reserve(&mut reducer, pane)
        } else {
            start(&mut reducer, pane)
        };
        let class = FailureClass::ResourceLimit;
        let terminal = SessionTerminalResult::new(
            session,
            if startup {
                RuntimeTerminalStatus::StartupFailed { class }
            } else {
                RuntimeTerminalStatus::RuntimeFailed { class }
            },
        )
        .unwrap();
        let cleanup =
            SessionCleanupObservation::new(session, RetirementReason::RuntimeFailure, CONFIRMED)
                .unwrap();
        let completion = RuntimeCompletion::new(terminal, cleanup).unwrap();
        let prepared = reducer
            .prepare_terminal_result(completion.terminal())
            .unwrap();
        commit(&mut reducer, prepared);
        assert_eq!(
            runtime(&reducer, pane).phase,
            SessionPhase::Failed { class }
        );
        assert_eq!(reducer.snapshot().state().panes.len(), 1);
        let duplicate = reducer
            .prepare_terminal_result(completion.terminal())
            .unwrap();
        assert!(commit(&mut reducer, duplicate).publication.is_none());
    }
}

fn fail_retained_runtime(
    reducer: &mut OrganizationReducer,
    pane: PaneId,
    startup: bool,
) -> SessionKey {
    use spaceterm_core::runtime_authority::*;
    let session = if startup {
        reserve(reducer, pane)
    } else {
        start(reducer, pane)
    };
    let terminal = SessionTerminalResult::new(
        session,
        if startup {
            RuntimeTerminalStatus::StartupFailed {
                class: FailureClass::ResourceLimit,
            }
        } else {
            RuntimeTerminalStatus::RuntimeFailed {
                class: FailureClass::RuntimeFault,
            }
        },
    )
    .unwrap();
    let lifecycle = RuntimeLifecycle::Retiring { terminal };
    let prepared = reducer
        .prepare_terminal_result(lifecycle.terminal_result().unwrap())
        .unwrap();
    commit(reducer, prepared);
    assert!(matches!(
        runtime(reducer, pane).phase,
        SessionPhase::Failed { .. }
    ));
    assert!(runtime(reducer, pane).session_generation > session.generation);
    session
}

#[test]
fn failure_before_end_all_and_stop_retains_exact_owner_and_waits_for_independent_cleanup() {
    for startup in [false, true] {
        for cleanup_result in [CONFIRMED, CleanupResult::Failed(FailureClass::RuntimeFault)] {
            let mut reducer = reducer();
            let (_, _, pane) = create(&mut reducer, 1);
            let session = fail_retained_runtime(&mut reducer, pane, startup);
            let handle = assess_end_all(&mut reducer, 2);
            let accepted = end_all(&mut reducer, handle, 3);
            let end = plan(&accepted).clone();
            assert_eq!(end.sessions().len(), 1);
            assert_eq!(end.sessions()[0].session(), session);
            assert_eq!(
                end.sessions()[0].first_reason(),
                RetirementReason::RuntimeFailure
            );
            assert_eq!(
                end.sessions()[0].cleanup(),
                RetirementCleanupState::AwaitingEvidence
            );
            assert_eq!(
                reducer.prepare_retirement_completion(end.batch()).err(),
                Some(ReducerFailure::InvalidFacts)
            );
            assert!(
                !accepted
                    .effects
                    .iter()
                    .any(|v| matches!(v, OrganizationEffect::RetireSession { .. }))
            );
            let accepted = stop(&mut reducer, 1);
            let native = native_plan(&accepted).clone();
            assert_eq!(native.sessions(), end.sessions());
            assert_eq!(
                reducer.prepare_retirement_completion(native.batch()).err(),
                Some(ReducerFailure::InvalidFacts)
            );
            assert!(
                !accepted
                    .effects
                    .iter()
                    .any(|v| matches!(v, OrganizationEffect::RetireSession { .. }))
            );
            let before = reducer.snapshot();
            let observation = SessionCleanupObservation::new(
                session,
                RetirementReason::RuntimeFailure,
                cleanup_result,
            )
            .unwrap();
            let prepared = reducer
                .prepare_cleanup_observation(end.batch(), observation)
                .unwrap();
            let progress = commit(&mut reducer, prepared);
            assert!(
                matches!(progress.resolution, Resolution::RetirementProgress { operations, native } if operations.len() == 1 && native.len() == 1)
            );
            assert!(progress.publication.is_none() && progress.effects.is_empty());
            if cleanup_result == CONFIRMED {
                let completed = complete(&mut reducer, end.batch());
                assert!(
                    matches!(completed.resolution, Resolution::RetirementCompleted(v) if v.descriptor.acceptance_revision() == end.acceptance_revision())
                );
                complete(&mut reducer, native.batch());
            } else {
                for batch in [end.batch(), native.batch()] {
                    assert_eq!(
                        reducer.prepare_retirement_completion(batch).err(),
                        Some(ReducerFailure::InvalidFacts)
                    );
                }
                let confirmed = SessionCleanupObservation::new(
                    session,
                    RetirementReason::RuntimeFailure,
                    CONFIRMED,
                )
                .unwrap();
                let prepared = reducer
                    .prepare_cleanup_observation(native.batch(), confirmed)
                    .unwrap();
                commit(&mut reducer, prepared);
                assert!(!reducer.retained().pending_retirements()[0].cleanup_confirmed());
                assert!(!reducer.retained().native_retirements()[0].cleanup_confirmed());
            }
            assert_eq!(reducer.snapshot(), before);
            assert_eq!(
                runtime(&reducer, pane).phase,
                SessionPhase::Ended {
                    reason: EndReason::Stop
                }
            );
            reducer
                .retained()
                .validate(OrganizationLimits::default())
                .unwrap();
        }
    }
}

#[test]
fn ordinary_terminal_cleanup_after_cascade_is_still_awaited_by_server_retirement() {
    use spaceterm_core::runtime_authority::*;
    let mut reducer = reducer();
    let (workspace, tab, pane) = create(&mut reducer, 1);
    let session = reserve(&mut reducer, pane);
    let terminal = SessionTerminalResult::new(
        session,
        RuntimeTerminalStatus::OrdinaryExit {
            exit_status: Some(0),
        },
    )
    .unwrap();
    let prepared = reducer.prepare_terminal_result(terminal).unwrap();
    commit(&mut reducer, prepared);
    assert!(reducer.snapshot().state().workspaces.is_empty());
    let handle = assess_end_all(&mut reducer, 2);
    let result = end_all(&mut reducer, handle, 3);
    let descriptor = plan(&result).clone();
    assert_eq!(descriptor.sessions().len(), 1);
    assert_eq!(descriptor.sessions()[0].session(), session);
    assert_eq!(
        descriptor.sessions()[0].first_reason(),
        RetirementReason::OrdinaryExit
    );
    assert_eq!(descriptor.panes()[0].workspace(), workspace);
    assert_eq!(descriptor.panes()[0].tab(), tab);
    assert_eq!(
        reducer
            .prepare_retirement_completion(descriptor.batch())
            .err(),
        Some(ReducerFailure::InvalidFacts)
    );
    assert!(
        !result
            .effects
            .iter()
            .any(|v| matches!(v, OrganizationEffect::RetireSession { .. }))
    );
    let result = stop(&mut reducer, 1);
    let native = native_plan(&result).clone();
    assert_eq!(native.sessions(), descriptor.sessions());
    let observation =
        SessionCleanupObservation::new(session, RetirementReason::OrdinaryExit, CONFIRMED).unwrap();
    let prepared = reducer.prepare_runtime_cleanup(observation).unwrap();
    let result = commit(&mut reducer, prepared);
    assert!(
        matches!(result.resolution, Resolution::RetirementProgress { operations, native } if operations.len() == 1 && native.len() == 1)
    );
    complete(&mut reducer, descriptor.batch());
    complete(&mut reducer, native.batch());
    assert!(reducer.snapshot().state().workspaces.is_empty());
    reducer
        .retained()
        .validate(OrganizationLimits::default())
        .unwrap();
}

#[test]
fn independent_failure_cleanup_fences_retry_and_repair_until_trusted_owner_evidence() {
    for startup in [false, true] {
        let mut reducer = reducer();
        let (_, _, pane) = create(&mut reducer, 1);
        let session = fail_retained_runtime(&mut reducer, pane, startup);
        let current = runtime(&reducer, pane);
        let next = SessionKey {
            generation: current.session_generation.checked_successor().unwrap(),
            ..session
        };
        let reservation = StartupReservation::new(
            pane,
            current.session_generation,
            current.control_generation,
            current.control,
            next,
        )
        .unwrap();
        let prepare_retry = |reducer: &OrganizationReducer| {
            let prepared = reducer
                .prepare_event(&OrganizationEvent::StartupReserved(reservation))
                .unwrap();
            assert!(matches!(
                prepared.resolution(),
                Resolution::Completed(Outcome::Failed(OperationFailure::BusyRetiring))
            ));
        };
        prepare_retry(&reducer);
        let repair = Command::RepairPaneDirectory {
            pane,
            expected_version: FieldVersion::new(1),
            value: directory(b"/repair"),
        };
        let prepared = reducer
            .prepare_command(&repair, &facts(&reducer, &repair, 2))
            .unwrap();
        assert!(matches!(
            prepared.resolution(),
            Resolution::Completed(Outcome::Failed(OperationFailure::BusyRetiring))
        ));
        let prepared = reducer
            .prepare_command(
                &Command::AssessClose {
                    target: CloseTarget::Pane(pane),
                },
                &context(3),
            )
            .unwrap();
        assert!(matches!(
            prepared.resolution(),
            Resolution::Completed(Outcome::Failed(OperationFailure::BusyRetiring))
        ));
        let wrong_reason =
            SessionCleanupObservation::new(session, RetirementReason::EndAllSessions, CONFIRMED)
                .unwrap();
        assert_eq!(
            reducer.prepare_runtime_cleanup(wrong_reason).err(),
            Some(ReducerFailure::InvalidFacts)
        );
        for wrong in [
            next,
            SessionKey {
                epoch: session.epoch.checked_successor().unwrap(),
                ..session
            },
        ] {
            let observation =
                SessionCleanupObservation::new(wrong, RetirementReason::RuntimeFailure, CONFIRMED)
                    .unwrap();
            let prepared = reducer.prepare_runtime_cleanup(observation).unwrap();
            let result = commit(&mut reducer, prepared);
            assert!(result.publication.is_none() && result.effects.is_empty());
            prepare_retry(&reducer);
        }
        let before = reducer.snapshot();
        let observation =
            SessionCleanupObservation::new(session, RetirementReason::RuntimeFailure, CONFIRMED)
                .unwrap();
        let prepared = reducer.prepare_runtime_cleanup(observation).unwrap();
        prepare_retry(&reducer);
        let result = commit(&mut reducer, prepared);
        assert!(result.publication.is_none() && result.effects.is_empty());
        assert_eq!(reducer.snapshot(), before);
        let replacement = start(&mut reducer, pane);
        assert_eq!(replacement, next);
        let before = reducer.snapshot();
        let prepared = reducer.prepare_runtime_cleanup(observation).unwrap();
        let result = commit(&mut reducer, prepared);
        assert!(result.publication.is_none() && result.effects.is_empty());
        assert_eq!(reducer.snapshot(), before);
    }
}

#[test]
fn independent_failed_cleanup_is_retained_in_later_end_all_and_cannot_become_success() {
    let mut reducer = reducer();
    let (_, _, pane) = create(&mut reducer, 1);
    let session = fail_retained_runtime(&mut reducer, pane, false);
    for result in [CleanupResult::Failed(FailureClass::RuntimeFault), CONFIRMED] {
        let observation =
            SessionCleanupObservation::new(session, RetirementReason::RuntimeFailure, result)
                .unwrap();
        let prepared = reducer.prepare_runtime_cleanup(observation).unwrap();
        let result = commit(&mut reducer, prepared);
        assert!(result.publication.is_none() && result.effects.is_empty());
    }
    let handle = assess_end_all(&mut reducer, 2);
    let accepted = end_all(&mut reducer, handle, 3);
    let descriptor = plan(&accepted);
    assert_eq!(descriptor.sessions().len(), 1);
    assert_eq!(descriptor.sessions()[0].session(), session);
    assert_eq!(
        descriptor.sessions()[0].first_reason(),
        RetirementReason::RuntimeFailure
    );
    assert_eq!(
        descriptor.sessions()[0].cleanup(),
        RetirementCleanupState::Failed(FailureClass::RuntimeFault)
    );
    assert_eq!(
        reducer
            .prepare_retirement_completion(descriptor.batch())
            .err(),
        Some(ReducerFailure::InvalidFacts)
    );
}

#[test]
fn confirmed_failure_cleanup_before_retirement_does_not_leave_a_phantom_owner() {
    let mut reducer = reducer();
    let (_, _, pane) = create(&mut reducer, 1);
    let session = fail_retained_runtime(&mut reducer, pane, true);
    let observation =
        SessionCleanupObservation::new(session, RetirementReason::RuntimeFailure, CONFIRMED)
            .unwrap();
    let prepared = reducer.prepare_runtime_cleanup(observation).unwrap();
    commit(&mut reducer, prepared);
    let handle = assess_end_all(&mut reducer, 2);
    let result = end_all(&mut reducer, handle, 3);
    assert!(plan(&result).sessions().is_empty());
    complete(&mut reducer, plan(&result).batch());
    let result = stop(&mut reducer, 1);
    assert!(native_plan(&result).sessions().is_empty());
    complete(&mut reducer, native_plan(&result).batch());
}

#[test]
fn failure_before_native_stop_retains_exact_owner_and_cannot_complete_without_cleanup() {
    for startup in [false, true] {
        let mut reducer = reducer();
        let (_, _, pane) = create(&mut reducer, 1);
        let session = fail_retained_runtime(&mut reducer, pane, startup);
        let accepted = stop(&mut reducer, 1);
        let descriptor = native_plan(&accepted).clone();
        assert_eq!(descriptor.sessions().len(), 1);
        assert_eq!(descriptor.sessions()[0].session(), session);
        assert_eq!(
            descriptor.sessions()[0].first_reason(),
            RetirementReason::RuntimeFailure
        );
        assert_eq!(
            reducer
                .prepare_retirement_completion(descriptor.batch())
                .err(),
            Some(ReducerFailure::InvalidFacts)
        );
        let observation =
            SessionCleanupObservation::new(session, RetirementReason::RuntimeFailure, CONFIRMED)
                .unwrap();
        let prepared = reducer
            .prepare_cleanup_observation(descriptor.batch(), observation)
            .unwrap();
        commit(&mut reducer, prepared);
        complete(&mut reducer, descriptor.batch());
        assert_eq!(reducer.snapshot().state().panes.len(), 1);
    }
}

fn worktree_reducer() -> (
    OrganizationReducer,
    WorkspaceId,
    TabId,
    PaneId,
    RepositoryScope,
    WorktreeId,
) {
    let mut base = reducer();
    let (workspace, tab, pane) = create(&mut base, 1);
    let retained = with_worktrees(base.retained());
    let mut org = retained.organization().clone();
    let worktree = WorktreeId::new(SERVER, 2).unwrap();
    org.tabs[0].worktree = Some(worktree);
    let retained =
        RetainedOrganization::new(org, retained.high_water(), retained.server_home().clone());
    let mut reducer = OrganizationReducer::from_recovered(
        retained,
        base.store_revision(),
        ServerEpoch::new(10),
        OrganizationLimits::default(),
    )
    .unwrap();
    event(
        &mut reducer,
        OrganizationEvent::ControlChanged {
            pane,
            expected: ControlRecord::Unclaimed,
            replacement: ControlRecord::Held {
                client: context(2).principal.client,
                generation: ControlGeneration::new(2),
            },
        },
    );
    (
        reducer,
        workspace,
        tab,
        pane,
        RepositoryScope {
            server: SERVER,
            repository: RepositoryId::new(SERVER, 1).unwrap(),
        },
        worktree,
    )
}
fn retire_worktree(
    reducer: &mut OrganizationReducer,
    scope: RepositoryScope,
    worktree: WorktreeId,
) -> RetirementDescriptor {
    let prepared = reducer
        .prepare_worktree_assessment(scope, worktree, &context(3))
        .unwrap();
    let handle = match commit(reducer, prepared).resolution {
        Resolution::Completed(Outcome::Assessment(v)) => v.reference(),
        _ => panic!("assessment missing"),
    };
    let prepared = reducer
        .prepare_worktree_retirement(scope, worktree, &handle, &retiring_context(4))
        .unwrap();
    plan(&commit(reducer, prepared)).clone()
}

#[test]
fn worktree_retirement_is_non_deleting_and_failed_settlement_leaves_latest_ended_state() {
    let (mut reducer, _, _, pane, scope, worktree) = worktree_reducer();
    let session = start(&mut reducer, pane);
    let before = reducer.snapshot();
    let descriptor = retire_worktree(&mut reducer, scope, worktree);
    assert_eq!(
        descriptor.completion(),
        RetirementCompletion::ContinueGitSettlement
    );
    assert_eq!(reducer.snapshot().state().panes, before.state().panes);
    assert_eq!(reducer.snapshot().state().tabs, before.state().tabs);
    assert_eq!(
        runtime(&reducer, pane).phase,
        SessionPhase::Ended {
            reason: EndReason::WorktreeRemoval
        }
    );
    let handle = assess_end_all(&mut reducer, 5);
    let result = end_all(&mut reducer, handle, 6);
    let overlapping = plan(&result).clone();
    assert_eq!(
        overlapping.sessions()[0].first_reason(),
        RetirementReason::WorktreeRemoval
    );
    assert!(
        !result
            .effects
            .iter()
            .any(|v| matches!(v, OrganizationEffect::RetireSession { .. }))
    );
    let wrong =
        SessionCleanupObservation::new(session, RetirementReason::EndAllSessions, CONFIRMED)
            .unwrap();
    assert_eq!(
        reducer
            .prepare_cleanup_observation(overlapping.batch(), wrong)
            .err(),
        Some(ReducerFailure::InvalidFacts)
    );
    let actual =
        SessionCleanupObservation::new(session, RetirementReason::WorktreeRemoval, CONFIRMED)
            .unwrap();
    let prepared = reducer
        .prepare_cleanup_observation(overlapping.batch(), actual)
        .unwrap();
    commit(&mut reducer, prepared);
    complete(&mut reducer, overlapping.batch());
    assert_eq!(
        reducer
            .prepare_retirement_completion(descriptor.batch())
            .err(),
        Some(ReducerFailure::WrongReducer)
    );
    let prepared = reducer
        .prepare_worktree_retirement_release(descriptor.batch(), descriptor.binding())
        .unwrap();
    let result = commit(&mut reducer, prepared);
    assert!(matches!(
        result.resolution,
        Resolution::GitRetirementReleased(_)
    ));
    assert!(result.publication.is_none() && result.effects.is_empty());
    assert_eq!(
        runtime(&reducer, pane).phase,
        SessionPhase::Ended {
            reason: EndReason::EndAllSessions
        }
    );
    assert_eq!(reducer.snapshot().state().panes, before.state().panes);
    start(&mut reducer, pane);
}

#[test]
fn worktree_retirement_consumes_own_durable_fallback_reservation_then_settles() {
    use spaceterm_core::worktrees::intent::*;
    let (mut reducer, workspace, tab, pane, scope, worktree) = worktree_reducer();
    let session = start(&mut reducer, pane);
    let prepared = reducer
        .prepare_worktree_assessment(scope, worktree, &context(3))
        .unwrap();
    let handle = match commit(&mut reducer, prepared).resolution {
        Resolution::Completed(Outcome::Assessment(v)) => v.reference(),
        _ => panic!("assessment missing"),
    };
    let context = retiring_context(4);
    let prepared = reducer
        .prepare_git_allocation(
            &GitAllocationRequest {
                repository: false,
                worktree: false,
                fallbacks: vec![WorkspaceFallbackPlan {
                    workspace,
                    worktree: WorktreeId::new(SERVER, 1).unwrap(),
                    directory: directory(b"/repo/main"),
                }],
            },
            &context,
        )
        .unwrap();
    let reservation = match prepared.resolution() {
        Resolution::AllocationReserved(v) => (**v).clone(),
        _ => panic!("reservation missing"),
    };
    commit(&mut reducer, prepared);
    assert_eq!(
        reducer.validate_assessment(
            &context.principal,
            &handle,
            &AssessmentTarget::RemoveWorktree { scope, worktree },
            context.now,
        ),
        Err(AssessmentFailure::BusyRetiring)
    );
    for other in [retiring_context(5), {
        let mut other = retiring_context(4);
        other.request_digest = RequestDigest::from_bytes([9; 32]);
        other
    }] {
        let prepared = reducer
            .prepare_worktree_retirement(scope, worktree, &handle, &other)
            .unwrap();
        assert!(matches!(
            prepared.resolution(),
            Resolution::Completed(Outcome::Failed(OperationFailure::BusyRetiring))
        ));
    }
    let conflicting = retiring_context(6);
    let prepared = reducer
        .prepare_git_allocation(
            &GitAllocationRequest {
                repository: false,
                worktree: false,
                fallbacks: vec![reservation.parts().fallbacks[0].plan.clone()],
            },
            &conflicting,
        )
        .unwrap();
    commit(&mut reducer, prepared);
    let rejected = reducer
        .prepare_worktree_retirement(scope, worktree, &handle, &context)
        .unwrap();
    assert!(matches!(
        rejected.resolution(),
        Resolution::Completed(Outcome::Failed(OperationFailure::BusyRetiring))
    ));
    let prepared = reducer
        .prepare_abandon_git_allocation(conflicting.binding())
        .unwrap();
    commit(&mut reducer, prepared);
    // The uncontrolled store/intent owner records Started before retirement.
    // This receipt models that boundary, not real Git or ledger durability.
    reducer
        .observe_ledger_commit(&CommitReceipt {
            store_revision: reducer.store_revision().checked_successor().unwrap(),
            organization_revision: reducer.snapshot().state().revision,
        })
        .unwrap();
    let before = reducer.snapshot();
    let prepared = reducer
        .prepare_worktree_retirement(scope, worktree, &handle, &context)
        .unwrap();
    let accepted = commit(&mut reducer, prepared);
    assert!(matches!(
        accepted.resolution,
        Resolution::AwaitingRetirement(_)
    ));
    let descriptor = plan(&accepted).clone();
    assert_eq!(descriptor.binding(), reservation.binding());
    assert_eq!(descriptor.sessions()[0].session(), session);
    assert_eq!(
        descriptor.completion(),
        RetirementCompletion::ContinueGitSettlement
    );
    assert_eq!(reducer.snapshot().state().tabs, before.state().tabs);
    assert_eq!(reducer.snapshot().state().panes, before.state().panes);
    let patch = WorktreeOrganizationPatch {
        binding: context.binding(),
        scope,
        expected_inventory_version: Some(FieldVersion::new(1)),
        repositories: vec![],
        worktrees: vec![],
        memberships: vec![],
        settlement: Some(GitOrganizationSettlement {
            reservation: reservation.clone(),
            phase: GitIntentPhase::Started,
            remove_tabs: vec![
                before
                    .state()
                    .tabs
                    .iter()
                    .find(|v| v.id == tab)
                    .unwrap()
                    .clone(),
            ],
            creator_still_authorized: false,
        }),
    };
    let blocked = event(
        &mut reducer,
        OrganizationEvent::WorktreeOrganizationChanged(Box::new(patch.clone())),
    );
    assert!(matches!(
        blocked.resolution,
        Resolution::Completed(Outcome::Failed(OperationFailure::BusyRetiring))
    ));
    cleanup(&mut reducer, &descriptor);
    let before = reducer.snapshot();
    let prepared = reducer
        .prepare_event(&OrganizationEvent::WorktreeOrganizationChanged(Box::new(
            patch,
        )))
        .unwrap();
    assert!(prepared.writes().operation_writes().iter().any(|v| matches!(
        v,
        OperationWrite::GitAllocation(GitAllocationMutation::Consume { binding }) if *binding == context.binding()
    )));
    let settled = commit(&mut reducer, prepared);
    let after = reducer.snapshot();
    assert_eq!(replay(before, settled.publication.as_ref().unwrap()), after);
    assert_eq!(after.state().workspaces[0].id, workspace);
    assert_eq!(
        after.state().tabs[0].id,
        reservation.parts().fallbacks[0].tab
    );
    assert_eq!(
        after.state().panes[0].id,
        reservation.parts().fallbacks[0].pane
    );
    assert_eq!(after.runtime()[0].phase, SessionPhase::NotStarted);
    assert_eq!(after.runtime()[0].control, ControlRecord::Unclaimed);
    assert!(settled.effects.is_empty());
    assert!(reducer.retained().git_allocations().is_empty());
    assert!(reducer.retained().pending_retirements().is_empty());
    reducer
        .retained()
        .validate(OrganizationLimits::default())
        .unwrap();
}

#[test]
fn started_git_settlement_waits_for_independent_failed_owner_cleanup() {
    use spaceterm_core::runtime_authority::*;
    use spaceterm_core::worktrees::intent::*;
    let (mut reducer, workspace, _, pane, scope, _) = worktree_reducer();
    let context = retiring_context(4);
    let prepared = reducer
        .prepare_git_allocation(
            &GitAllocationRequest {
                repository: false,
                worktree: false,
                fallbacks: vec![WorkspaceFallbackPlan {
                    workspace,
                    worktree: WorktreeId::new(SERVER, 1).unwrap(),
                    directory: directory(b"/repo/main"),
                }],
            },
            &context,
        )
        .unwrap();
    let reservation = match prepared.resolution() {
        Resolution::AllocationReserved(v) => (**v).clone(),
        _ => panic!("reservation missing"),
    };
    commit(&mut reducer, prepared);
    let session = start(&mut reducer, pane);
    let terminal = SessionTerminalResult::new(
        session,
        RuntimeTerminalStatus::RuntimeFailed {
            class: FailureClass::RuntimeFault,
        },
    )
    .unwrap();
    let prepared = reducer.prepare_terminal_result(terminal).unwrap();
    commit(&mut reducer, prepared);
    let patch = WorktreeOrganizationPatch {
        binding: context.binding(),
        scope,
        expected_inventory_version: Some(FieldVersion::new(1)),
        repositories: vec![],
        worktrees: vec![],
        memberships: vec![],
        settlement: Some(GitOrganizationSettlement {
            reservation,
            phase: GitIntentPhase::Started,
            remove_tabs: reducer.snapshot().state().tabs.clone(),
            creator_still_authorized: false,
        }),
    };
    let prepared = reducer
        .prepare_event(&OrganizationEvent::WorktreeOrganizationChanged(Box::new(
            patch.clone(),
        )))
        .unwrap();
    assert!(matches!(
        prepared.resolution(),
        Resolution::Completed(Outcome::Failed(OperationFailure::BusyRetiring))
    ));
    let observation =
        SessionCleanupObservation::new(session, RetirementReason::RuntimeFailure, CONFIRMED)
            .unwrap();
    let prepared = reducer.prepare_runtime_cleanup(observation).unwrap();
    commit(&mut reducer, prepared);
    let result = event(
        &mut reducer,
        OrganizationEvent::WorktreeOrganizationChanged(Box::new(patch)),
    );
    assert!(matches!(
        result.resolution,
        Resolution::Completed(Outcome::Applied)
    ));
    assert!(result.effects.is_empty());
    reducer
        .retained()
        .validate(OrganizationLimits::default())
        .unwrap();
}

#[test]
fn worktree_retirement_with_own_reservation_still_revalidates_exact_runtime() {
    use spaceterm_core::worktrees::intent::*;
    let (mut reducer, workspace, _, pane, scope, worktree) = worktree_reducer();
    let session = reserve(&mut reducer, pane);
    let prepared = reducer
        .prepare_worktree_assessment(scope, worktree, &context(3))
        .unwrap();
    let handle = match commit(&mut reducer, prepared).resolution {
        Resolution::Completed(Outcome::Assessment(v)) => v.reference(),
        _ => panic!("assessment missing"),
    };
    let context = retiring_context(4);
    let prepared = reducer
        .prepare_git_allocation(
            &GitAllocationRequest {
                repository: false,
                worktree: false,
                fallbacks: vec![WorkspaceFallbackPlan {
                    workspace,
                    worktree: WorktreeId::new(SERVER, 1).unwrap(),
                    directory: directory(b"/repo/main"),
                }],
            },
            &context,
        )
        .unwrap();
    commit(&mut reducer, prepared);
    event(&mut reducer, OrganizationEvent::RuntimeStarted { session });
    let prepared = reducer
        .prepare_worktree_retirement(scope, worktree, &handle, &context)
        .unwrap();
    assert!(matches!(
        prepared.resolution(),
        Resolution::Completed(Outcome::Failed(OperationFailure::StaleAssessment))
    ));
    let rejected = commit(&mut reducer, prepared);
    assert!(rejected.publication.is_none() && rejected.effects.is_empty());
    assert!(reducer.retained().pending_retirements().is_empty());
}

#[test]
fn started_git_settlement_after_end_all_with_revoked_creator_uses_reserved_metadata_only() {
    use spaceterm_core::worktrees::intent::*;
    let (mut reducer, workspace, tab, pane, scope, worktree) = worktree_reducer();
    start(&mut reducer, pane);
    let context = retiring_context(4);
    let request = GitAllocationRequest {
        repository: false,
        worktree: false,
        fallbacks: vec![WorkspaceFallbackPlan {
            workspace,
            worktree: WorktreeId::new(SERVER, 1).unwrap(),
            directory: directory(b"/repo/main"),
        }],
    };
    let prepared = reducer.prepare_git_allocation(&request, &context).unwrap();
    let reservation = match prepared.resolution() {
        Resolution::AllocationReserved(v) => (**v).clone(),
        _ => panic!("reservation missing"),
    };
    commit(&mut reducer, prepared);
    let handle = assess_end_all(&mut reducer, 5);
    let result = end_all(&mut reducer, handle, 6);
    let end = plan(&result).clone();
    // A previously admitted Started intent needs no new ordinary principal grant.
    cleanup(&mut reducer, &end);
    let old = reducer
        .snapshot()
        .state()
        .tabs
        .iter()
        .find(|v| v.id == tab)
        .unwrap()
        .clone();
    let patch = WorktreeOrganizationPatch {
        binding: context.binding(),
        scope,
        expected_inventory_version: Some(FieldVersion::new(1)),
        repositories: vec![],
        worktrees: vec![],
        memberships: vec![],
        settlement: Some(GitOrganizationSettlement {
            reservation,
            phase: GitIntentPhase::Started,
            remove_tabs: vec![old],
            creator_still_authorized: false,
        }),
    };
    let blocked = event(
        &mut reducer,
        OrganizationEvent::WorktreeOrganizationChanged(Box::new(patch.clone())),
    );
    assert!(matches!(
        blocked.resolution,
        Resolution::Completed(Outcome::Failed(OperationFailure::BusyRetiring))
    ));
    complete(&mut reducer, end.batch());
    let before = reducer.snapshot();
    let settled = event(
        &mut reducer,
        OrganizationEvent::WorktreeOrganizationChanged(Box::new(patch)),
    );
    assert_eq!(
        replay(before, settled.publication.as_ref().unwrap()),
        reducer.snapshot()
    );
    assert_eq!(reducer.snapshot().state().workspaces[0].id, workspace);
    assert_ne!(reducer.snapshot().state().tabs[0].worktree, Some(worktree));
    assert_eq!(
        reducer.snapshot().runtime()[0].control,
        ControlRecord::Unclaimed
    );
    assert_eq!(
        reducer.snapshot().runtime()[0].phase,
        SessionPhase::NotStarted
    );
    assert!(settled.effects.is_empty());
}

#[test]
fn recovery_rejects_retirement_pane_membership_that_disagrees_with_surviving_hierarchy() {
    let mut reducer = reducer();
    let (_, _, pane) = create(&mut reducer, 1);
    let (other_workspace, other_tab, _) = create(&mut reducer, 2);
    start(&mut reducer, pane);
    let stopped = stop(&mut reducer, 1);
    let mut parts = native_plan(&stopped).parts().clone();
    let old = parts.panes[0];
    parts.panes[0] = RetirementPane::new(
        other_workspace,
        other_tab,
        pane,
        None,
        old.invalidation_generation(),
    )
    .unwrap();
    let invalid = reducer
        .retained()
        .with_native_retirements(vec![NativeRetirementDescriptor::new(parts).unwrap()]);
    assert_eq!(
        invalid.validate(OrganizationLimits::default()),
        Err(ReducerFailure::InvalidDurableState(
            DurableInvariant::Retirement
        ))
    );
}

#[test]
fn stop_closes_ordinary_mutations_but_allows_started_git_settlement_without_shell_or_grant() {
    use spaceterm_core::runtime_authority::*;
    use spaceterm_core::worktrees::intent::*;
    let (mut reducer, workspace, tab, pane, scope, _) = worktree_reducer();
    let session = start(&mut reducer, pane);
    let (_, _, other_pane) = create(&mut reducer, 2);
    let other_session = start(&mut reducer, other_pane);
    let admitted = retiring_context(4);
    let prepared = reducer
        .prepare_git_allocation(
            &GitAllocationRequest {
                repository: false,
                worktree: false,
                fallbacks: vec![WorkspaceFallbackPlan {
                    workspace,
                    worktree: WorktreeId::new(SERVER, 1).unwrap(),
                    directory: directory(b"/repo/main"),
                }],
            },
            &admitted,
        )
        .unwrap();
    let reservation = match prepared.resolution() {
        Resolution::AllocationReserved(v) => (**v).clone(),
        _ => panic!("reservation missing"),
    };
    commit(&mut reducer, prepared);
    // The uncontrolled intent owner records Started through the fake commit boundary.
    reducer
        .observe_ledger_commit(&CommitReceipt {
            store_revision: reducer.store_revision().checked_successor().unwrap(),
            organization_revision: reducer.snapshot().state().revision,
        })
        .unwrap();
    let terminal = SessionTerminalResult::new(
        session,
        RuntimeTerminalStatus::RuntimeFailed {
            class: FailureClass::RuntimeFault,
        },
    )
    .unwrap();
    let prepared = reducer.prepare_terminal_result(terminal).unwrap();
    commit(&mut reducer, prepared);
    let native = stop(&mut reducer, 1);
    let descriptor = native_plan(&native).clone();
    assert!(reducer.retained().pending_retirements().is_empty());
    assert!(descriptor.sessions().iter().any(|v| {
        v.session() == session
            && v.first_reason() == RetirementReason::RuntimeFailure
            && v.cleanup() == RetirementCleanupState::AwaitingEvidence
    }));
    assert!(!native.effects.iter().any(|v| matches!(
        v,
        OrganizationEffect::RetireSession { session: retired, .. } if *retired == session
    )));
    let before = reducer.snapshot();
    let result = apply(
        &mut reducer,
        Command::RenameWorkspace {
            workspace,
            expected_version: FieldVersion::new(1),
            draft: NameDraft::Custom(NameText::new("after Stop".into()).unwrap()),
        },
        5,
    );
    assert!(matches!(
        result.resolution,
        Resolution::Completed(Outcome::Failed(OperationFailure::BusyRetiring))
    ));
    assert!(result.publication.is_none() && result.effects.is_empty());
    assert_eq!(reducer.snapshot(), before);
    let old = before
        .state()
        .tabs
        .iter()
        .find(|v| v.id == tab)
        .unwrap()
        .clone();
    let fallback = reservation.parts().fallbacks[0].clone();
    let patch = WorktreeOrganizationPatch {
        binding: admitted.binding(),
        scope,
        expected_inventory_version: Some(FieldVersion::new(1)),
        repositories: vec![],
        worktrees: vec![],
        memberships: vec![],
        settlement: Some(GitOrganizationSettlement {
            reservation: reservation.clone(),
            phase: GitIntentPhase::Started,
            remove_tabs: vec![old],
            creator_still_authorized: false,
        }),
    };
    let prepared = reducer
        .prepare_event(&OrganizationEvent::WorktreeOrganizationChanged(Box::new(
            patch.clone(),
        )))
        .unwrap();
    assert!(matches!(
        prepared.resolution(),
        Resolution::Completed(Outcome::Failed(OperationFailure::BusyRetiring))
    ));
    assert!(prepared.writes().organization_writes().is_empty());
    assert!(
        prepared
            .writes()
            .operation_writes()
            .iter()
            .all(|v| matches!(v, OperationWrite::ReductionBasis(_)))
    );
    let blocked = commit(&mut reducer, prepared);
    assert!(blocked.publication.is_none() && blocked.effects.is_empty());
    assert_eq!(reducer.snapshot(), before);
    assert_eq!(reducer.retained().git_allocations(), &[reservation]);
    let observation =
        SessionCleanupObservation::new(session, RetirementReason::RuntimeFailure, CONFIRMED)
            .unwrap();
    let prepared = reducer.prepare_runtime_cleanup(observation).unwrap();
    commit(&mut reducer, prepared);
    // Settlement waits for its affected owner, not every Session in the Stop batch.
    assert!(!reducer.retained().native_retirements()[0].cleanup_confirmed());
    let prepared = reducer
        .prepare_event(&OrganizationEvent::WorktreeOrganizationChanged(Box::new(
            patch,
        )))
        .unwrap();
    assert!(prepared.writes().operation_writes().iter().any(|v| matches!(
        v,
        OperationWrite::GitAllocation(GitAllocationMutation::Consume { binding }) if *binding == admitted.binding()
    )));
    let result = commit(&mut reducer, prepared);
    assert!(matches!(
        result.resolution,
        Resolution::Completed(Outcome::Applied)
    ));
    assert!(result.publication.is_some() && result.effects.is_empty());
    assert_eq!(
        replay(before, result.publication.as_ref().unwrap()),
        reducer.snapshot()
    );
    assert!(
        reducer
            .snapshot()
            .state()
            .workspaces
            .iter()
            .any(|v| v.id == workspace)
    );
    assert!(!reducer.snapshot().state().tabs.iter().any(|v| v.id == tab));
    assert!(
        !reducer
            .snapshot()
            .state()
            .panes
            .iter()
            .any(|v| v.id == pane)
    );
    assert!(
        reducer
            .snapshot()
            .state()
            .tabs
            .iter()
            .any(|v| v.id == fallback.tab)
    );
    assert_eq!(
        runtime(&reducer, fallback.pane).phase,
        SessionPhase::NotStarted
    );
    assert_eq!(
        runtime(&reducer, fallback.pane).control,
        ControlRecord::Unclaimed
    );
    assert!(reducer.retained().git_allocations().is_empty());
    assert!(!reducer.retained().native_retirements()[0].cleanup_confirmed());
    assert_eq!(
        reducer
            .prepare_retirement_completion(descriptor.batch())
            .err(),
        Some(ReducerFailure::InvalidFacts)
    );
    let observation =
        SessionCleanupObservation::new(other_session, RetirementReason::Stop, CONFIRMED).unwrap();
    let prepared = reducer.prepare_runtime_cleanup(observation).unwrap();
    commit(&mut reducer, prepared);
    complete(&mut reducer, descriptor.batch());
    reducer
        .retained()
        .validate(OrganizationLimits::default())
        .unwrap();
}

#[test]
fn started_git_settlement_cannot_bypass_failed_cleanup_transferred_to_native_stop() {
    use spaceterm_core::runtime_authority::*;
    use spaceterm_core::worktrees::intent::*;
    let (mut reducer, workspace, _, pane, scope, _) = worktree_reducer();
    let session = start(&mut reducer, pane);
    let admitted = retiring_context(4);
    let prepared = reducer
        .prepare_git_allocation(
            &GitAllocationRequest {
                repository: false,
                worktree: false,
                fallbacks: vec![WorkspaceFallbackPlan {
                    workspace,
                    worktree: WorktreeId::new(SERVER, 1).unwrap(),
                    directory: directory(b"/repo/main"),
                }],
            },
            &admitted,
        )
        .unwrap();
    let reservation = match prepared.resolution() {
        Resolution::AllocationReserved(v) => (**v).clone(),
        _ => panic!("reservation missing"),
    };
    commit(&mut reducer, prepared);
    // The uncontrolled intent owner records Started through the fake commit boundary.
    reducer
        .observe_ledger_commit(&CommitReceipt {
            store_revision: reducer.store_revision().checked_successor().unwrap(),
            organization_revision: reducer.snapshot().state().revision,
        })
        .unwrap();
    let terminal = SessionTerminalResult::new(
        session,
        RuntimeTerminalStatus::RuntimeFailed {
            class: FailureClass::RuntimeFault,
        },
    )
    .unwrap();
    let prepared = reducer.prepare_terminal_result(terminal).unwrap();
    commit(&mut reducer, prepared);
    let observation = SessionCleanupObservation::new(
        session,
        RetirementReason::RuntimeFailure,
        CleanupResult::Failed(FailureClass::RuntimeUnavailable),
    )
    .unwrap();
    let prepared = reducer.prepare_runtime_cleanup(observation).unwrap();
    commit(&mut reducer, prepared);
    let native = stop(&mut reducer, 1);
    let descriptor = native_plan(&native).clone();
    assert_eq!(descriptor.sessions()[0].session(), session);
    assert_eq!(
        descriptor.sessions()[0].first_reason(),
        RetirementReason::RuntimeFailure
    );
    assert_eq!(
        descriptor.sessions()[0].cleanup(),
        RetirementCleanupState::Failed(FailureClass::RuntimeUnavailable)
    );
    let before = reducer.snapshot();
    let patch = WorktreeOrganizationPatch {
        binding: admitted.binding(),
        scope,
        expected_inventory_version: Some(FieldVersion::new(1)),
        repositories: vec![],
        worktrees: vec![],
        memberships: vec![],
        settlement: Some(GitOrganizationSettlement {
            reservation: reservation.clone(),
            phase: GitIntentPhase::Started,
            remove_tabs: before.state().tabs.clone(),
            creator_still_authorized: false,
        }),
    };
    let prepared = reducer
        .prepare_event(&OrganizationEvent::WorktreeOrganizationChanged(Box::new(
            patch,
        )))
        .unwrap();
    assert!(matches!(
        prepared.resolution(),
        Resolution::Completed(Outcome::Failed(OperationFailure::BusyRetiring))
    ));
    assert!(prepared.writes().organization_writes().is_empty());
    assert!(
        prepared
            .writes()
            .operation_writes()
            .iter()
            .all(|v| matches!(v, OperationWrite::ReductionBasis(_)))
    );
    let blocked = commit(&mut reducer, prepared);
    assert!(blocked.publication.is_none() && blocked.effects.is_empty());
    assert_eq!(reducer.snapshot(), before);
    assert_eq!(reducer.retained().git_allocations(), &[reservation]);
    assert_eq!(reducer.retained().native_retirements(), &[descriptor]);
    reducer
        .retained()
        .validate(OrganizationLimits::default())
        .unwrap();
}

#[test]
fn confirmed_close_cleanup_failure_never_recreates_deleted_hierarchy() {
    let mut reducer = reducer();
    let (_, _, pane) = create(&mut reducer, 1);
    let session = start(&mut reducer, pane);
    let descriptor = live_close(&mut reducer, pane, 2);
    let deleted = reducer.snapshot();
    event(
        &mut reducer,
        OrganizationEvent::RetirementCleanup {
            batch: descriptor.batch(),
            session,
            result: CleanupResult::Failed(FailureClass::RuntimeUnavailable),
        },
    );
    assert_eq!(
        reducer
            .prepare_retirement_completion(descriptor.batch())
            .err(),
        Some(ReducerFailure::InvalidFacts)
    );
    event(
        &mut reducer,
        OrganizationEvent::OrdinaryExit {
            session,
            class: OrdinaryExitClass::Success,
        },
    );
    assert_eq!(reducer.snapshot(), deleted);
    assert_eq!(
        reducer.retained().pending_retirements()[0].sessions()[0].cleanup(),
        RetirementCleanupState::Failed(FailureClass::RuntimeUnavailable)
    );
    assert!(reducer.snapshot().state().workspaces.is_empty());
}

#[test]
fn end_all_recovery_requires_old_owner_evidence_and_completion_preserves_new_epoch_restored_state()
{
    let mut reducer = reducer();
    let (_, _, pane) = create(&mut reducer, 1);
    let session = start(&mut reducer, pane);
    let handle = assess_end_all(&mut reducer, 2);
    let accepted = end_all(&mut reducer, handle, 3);
    let descriptor = plan(&accepted).clone();
    let mut recovered = OrganizationReducer::from_recovered(
        reducer.retained(),
        reducer.store_revision(),
        ServerEpoch::new(10),
        OrganizationLimits::default(),
    )
    .unwrap();
    assert_eq!(runtime(&recovered, pane).control, ControlRecord::Unclaimed);
    assert_eq!(
        runtime(&recovered, pane).phase,
        SessionPhase::Ended {
            reason: EndReason::ServerRestart
        }
    );
    assert_eq!(
        recovered
            .prepare_retirement_completion(descriptor.batch())
            .err(),
        Some(ReducerFailure::InvalidFacts)
    );
    assert!(
        event(
            &mut recovered,
            OrganizationEvent::RuntimeFailed {
                session,
                class: FailureClass::RuntimeFault
            }
        )
        .publication
        .is_none()
    );
    let evidence = SessionCleanupObservation::new(
        session,
        RetirementReason::EndAllSessions,
        CleanupResult::Confirmed(ConfirmedCleanupClass::AlreadyAbsentWithConfirmedOwnership),
    )
    .unwrap();
    let prepared = recovered
        .prepare_cleanup_observation(descriptor.batch(), evidence)
        .unwrap();
    let before = recovered.snapshot();
    assert!(!recovered.retained().pending_retirements()[0].cleanup_confirmed());
    commit(&mut recovered, prepared);
    let completed = complete(&mut recovered, descriptor.batch());
    assert!(completed.publication.is_none() && completed.effects.is_empty());
    assert_eq!(recovered.snapshot(), before);
    match completed.resolution {
        Resolution::RetirementCompleted(v) => assert_eq!(
            v.descriptor.acceptance_revision(),
            descriptor.acceptance_revision()
        ),
        _ => panic!("completion missing"),
    }
}
