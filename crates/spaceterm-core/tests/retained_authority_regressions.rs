#[path = "organization_reducer/support.rs"]
mod support;

use spaceterm_core::retirement::*;
use support::*;

fn start(reducer: &mut OrganizationReducer, pane: PaneId) -> SessionKey {
    let snapshot = reducer.snapshot();
    let current = snapshot.runtime().iter().find(|v| v.pane == pane).unwrap();
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
    for event in [
        OrganizationEvent::StartupReserved(reservation),
        OrganizationEvent::RuntimeStarted { session },
    ] {
        let prepared = reducer.prepare_event(&event).unwrap();
        commit(reducer, prepared);
    }
    session
}

#[test]
fn old_epoch_confirmation_cannot_change_recovered_or_replacement_directory() {
    let mut original = reducer();
    let (_, _, pane) = create(&mut original, 1);
    let old_session = start(&mut original, pane);
    let confirmed = ConfirmedDirectoryObservation::new(
        old_session,
        RuntimeObservationSequence::new(1).unwrap(),
        ReducerTick::new(10),
        directory(b"/retained"),
    )
    .unwrap();
    let prepared = original
        .prepare_event(&OrganizationEvent::RestorationDirectoryConfirmed(confirmed))
        .unwrap();
    commit(&mut original, prepared);
    let retained_pane = original.snapshot().state().panes[0].clone();
    assert_eq!(
        retained_pane.restoration_directory(),
        &directory(b"/retained")
    );

    let mut recovered = OrganizationReducer::from_recovered(
        original.retained(),
        original.store_revision(),
        old_session.epoch.checked_successor().unwrap(),
        OrganizationLimits::default(),
    )
    .unwrap();
    let before = recovered.snapshot();
    assert_eq!(before.state().panes[0], retained_pane);
    assert_eq!(before.runtime()[0].control, ControlRecord::Unclaimed);
    assert_eq!(
        before.runtime()[0].phase,
        SessionPhase::Ended {
            reason: EndReason::ServerRestart
        }
    );
    assert_eq!(
        before.runtime()[0].session_generation,
        old_session.generation
    );
    let delayed = ConfirmedDirectoryObservation::new(
        old_session,
        RuntimeObservationSequence::new(99).unwrap(),
        ReducerTick::new(20),
        directory(b"/old-epoch"),
    )
    .unwrap();
    let prepared = recovered
        .prepare_event(&OrganizationEvent::RestorationDirectoryConfirmed(delayed))
        .unwrap();
    assert!(prepared.writes().organization_writes().is_empty());
    assert!(prepared.writes().operation_writes().is_empty());
    assert_eq!(
        prepared.resulting_organization_revision(),
        before.state().revision
    );
    assert_eq!(recovered.snapshot(), before);
    let result = commit(&mut recovered, prepared);
    assert!(result.publication.is_none());
    assert!(result.effects.is_empty());
    assert_eq!(recovered.snapshot(), before);
    assert_eq!(recovered.confirmed_directory_sequence(pane), None);

    let prepared = recovered
        .prepare_event(&OrganizationEvent::ControlChanged {
            pane,
            expected: ControlRecord::Unclaimed,
            replacement: ControlRecord::Held {
                client: context(2).principal.client,
                generation: before.runtime()[0]
                    .control_generation
                    .checked_successor()
                    .unwrap(),
            },
        })
        .unwrap();
    commit(&mut recovered, prepared);
    let replacement = start(&mut recovered, pane);
    let before = recovered.snapshot();
    assert_eq!(
        before.runtime()[0].phase,
        SessionPhase::Running {
            generation: replacement.generation
        }
    );
    // Match the live generation so the epoch, not the ended phase or generation,
    // is the only stale comparison in this observation.
    let delayed = ConfirmedDirectoryObservation::new(
        SessionKey {
            epoch: old_session.epoch,
            ..replacement
        },
        RuntimeObservationSequence::new(99).unwrap(),
        ReducerTick::new(20),
        directory(b"/old-epoch"),
    )
    .unwrap();
    let prepared = recovered
        .prepare_event(&OrganizationEvent::RestorationDirectoryConfirmed(delayed))
        .unwrap();
    assert!(prepared.writes().organization_writes().is_empty());
    assert!(prepared.writes().operation_writes().is_empty());
    assert_eq!(
        prepared.resulting_organization_revision(),
        before.state().revision
    );
    let result = commit(&mut recovered, prepared);
    assert!(result.publication.is_none());
    assert!(result.effects.is_empty());
    assert_eq!(recovered.snapshot(), before);
    assert_eq!(recovered.snapshot().state().panes[0], retained_pane);
    assert_eq!(recovered.confirmed_directory_sequence(pane), None);

    let fresh = ConfirmedDirectoryObservation::new(
        replacement,
        RuntimeObservationSequence::new(1).unwrap(),
        ReducerTick::new(21),
        directory(b"/fresh"),
    )
    .unwrap();
    let prepared = recovered
        .prepare_event(&OrganizationEvent::RestorationDirectoryConfirmed(fresh))
        .unwrap();
    let result = commit(&mut recovered, prepared);
    assert_eq!(
        replay(before, result.publication.as_ref().unwrap()),
        recovered.snapshot()
    );
    assert!(result.effects.is_empty());
    let after = recovered.snapshot();
    assert_eq!(
        after.state().panes[0].restoration_directory(),
        &directory(b"/fresh")
    );
    assert_eq!(
        after.state().panes[0]
            .confirmed_restoration_directory
            .version,
        retained_pane
            .confirmed_restoration_directory
            .version
            .checked_successor()
            .unwrap()
    );
    assert_eq!(
        recovered.confirmed_directory_sequence(pane),
        Some(RuntimeObservationSequence::new(1).unwrap())
    );
}

fn mixed_control_reducer() -> (OrganizationReducer, [SessionKey; 3]) {
    let mut reducer = OrganizationReducer::from_recovered(
        with_worktrees(empty()),
        StoreRevision::new(0),
        ServerEpoch::new(9),
        OrganizationLimits::default(),
    )
    .unwrap();
    let (_, _, first) = create(&mut reducer, 1);
    let (_, _, second) = create(&mut reducer, 2);
    let (_, _, third) = create(&mut reducer, 3);
    let sessions = [first, second, third].map(|pane| start(&mut reducer, pane));
    // Connection presence is not a reducer input. These stable grants also
    // represent Clients whose connections have disappeared without release.
    for (session, replacement) in sessions.into_iter().zip([
        ControlRecord::Unclaimed,
        ControlRecord::Held {
            client: ClientId::new(SERVER, 2).unwrap(),
            generation: ControlGeneration::new(2),
        },
        ControlRecord::Held {
            client: ClientId::new(SERVER, 3).unwrap(),
            generation: ControlGeneration::new(2),
        },
    ]) {
        let prepared = reducer
            .prepare_event(&OrganizationEvent::ControlChanged {
                pane: session.pane,
                expected: ControlRecord::Held {
                    client: context(1).principal.client,
                    generation: ControlGeneration::new(1),
                },
                replacement,
            })
            .unwrap();
        commit(&mut reducer, prepared);
    }
    (reducer, sessions)
}

#[test]
fn end_all_without_control_preserves_unclaimed_and_other_clients_exact_grants() {
    let (mut reducer, sessions) = mixed_control_reducer();
    let before = reducer.snapshot();
    let invoking_client = context(4).principal.client;
    let controls = [
        ControlRecord::Unclaimed,
        ControlRecord::Held {
            client: ClientId::new(SERVER, 2).unwrap(),
            generation: ControlGeneration::new(2),
        },
        ControlRecord::Held {
            client: ClientId::new(SERVER, 3).unwrap(),
            generation: ControlGeneration::new(2),
        },
    ];
    for (runtime, control) in before.runtime().iter().zip(controls) {
        assert_eq!(runtime.control, control);
        assert_eq!(runtime.control_generation, ControlGeneration::new(2));
        assert!(
            !matches!(runtime.control, ControlRecord::Held { client, .. } if client == invoking_client)
        );
    }
    let assessed = apply(
        &mut reducer,
        Command::AssessEndAllSessions { server: SERVER },
        4,
    );
    let Resolution::Completed(Outcome::Assessment(assessment)) = assessed.resolution else {
        panic!("assessment missing");
    };
    assert_eq!(
        assessment.summary(),
        AssessmentSummary::new(AssessmentTarget::EndAllSessions(SERVER), 3, 3, 3, 3, true).unwrap()
    );
    assert!(assessed.publication.is_none());
    assert!(assessed.effects.is_empty());
    assert_eq!(reducer.snapshot(), before);

    let context = retiring_context(5);
    let prepared = reducer
        .prepare_command(
            &Command::EndAllSessions {
                server: SERVER,
                assessment: assessment.reference(),
            },
            &context,
        )
        .unwrap();
    assert_eq!(reducer.snapshot(), before);
    assert_eq!(
        prepared.resulting_organization_revision(),
        before.state().revision.checked_successor().unwrap()
    );
    assert_eq!(
        prepared.writes().expected_high_water(),
        prepared.writes().resulting_high_water()
    );
    let expected_writes: Vec<_> = before
        .runtime()
        .iter()
        .map(|runtime| {
            OrganizationWrite::Retained(RetainedOrganizationWrite::SetPaneGenerationHighWater(
                PaneGenerationHighWater::new(
                    runtime.pane,
                    runtime.session_generation.checked_successor().unwrap(),
                    runtime.control_generation,
                ),
            ))
        })
        .collect();
    assert_eq!(prepared.writes().organization_writes(), expected_writes);
    let accepted = commit(&mut reducer, prepared);
    let Resolution::AwaitingRetirement(plan) = &accepted.resolution else {
        panic!("retirement missing");
    };
    let descriptor = &plan.descriptor;
    let after = reducer.snapshot();
    assert_eq!(descriptor.binding(), context.binding());
    assert_eq!(
        descriptor.target(),
        AssessmentTarget::EndAllSessions(SERVER)
    );
    assert_eq!(descriptor.assessment(), assessment.reference());
    assert_eq!(descriptor.acceptance_revision(), after.state().revision);
    assert!(descriptor.server_wide_barrier());
    assert_eq!(descriptor.panes().len(), 3);
    assert_eq!(
        descriptor.sessions(),
        sessions.map(|session| RetiredSession::new(
            session,
            RetirementReason::EndAllSessions,
            RetirementCleanupState::AwaitingEvidence,
        )
        .unwrap())
    );
    let delta = accepted.publication.as_ref().unwrap();
    assert_eq!(delta.base(), before.state().revision);
    assert_eq!(
        delta.revision(),
        before.state().revision.checked_successor().unwrap()
    );
    assert_eq!(delta.writes(), expected_writes);
    assert_eq!(replay(before.clone(), delta), after);

    let mut expected_state = before.state().clone();
    expected_state.revision = after.state().revision;
    let mut expected_facts = expected_state.retained.as_ref().unwrap().parts().clone();
    for mark in &mut expected_facts.pane_generations {
        *mark = PaneGenerationHighWater::new(
            mark.pane(),
            mark.session().checked_successor().unwrap(),
            mark.control(),
        );
    }
    expected_state.retained = Some(OrganizationRetainedFacts::new(expected_facts).unwrap());
    assert_eq!(after.state(), &expected_state);
    for (prior, current) in before.runtime().iter().zip(after.runtime()) {
        assert_eq!(current.control, prior.control);
        assert_eq!(current.control_generation, prior.control_generation);
        assert!(
            !matches!(current.control, ControlRecord::Held { client, .. } if client == invoking_client)
        );
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
    let mut expected_effects = Vec::new();
    for current in after.runtime() {
        expected_effects.push(OrganizationEffect::CancelStartupIntents { pane: current.pane });
        expected_effects.push(OrganizationEffect::InvalidatePaneRuntime {
            pane: current.pane,
            generation: current.session_generation,
            clear_content: true,
            clear_approvals: true,
        });
    }
    expected_effects.extend(sessions.map(|session| OrganizationEffect::RetireSession {
        batch: descriptor.batch(),
        session,
        reason: RetirementReason::EndAllSessions,
    }));
    assert_eq!(accepted.effects, expected_effects);
    assert_eq!(
        reducer
            .prepare_retirement_completion(descriptor.batch())
            .err(),
        Some(ReducerFailure::InvalidFacts)
    );

    for session in sessions {
        let prepared = reducer
            .prepare_cleanup_observation(
                descriptor.batch(),
                SessionCleanupObservation::new(
                    session,
                    RetirementReason::EndAllSessions,
                    CleanupResult::Confirmed(ConfirmedCleanupClass::ReapedAndReleased),
                )
                .unwrap(),
            )
            .unwrap();
        let progress = commit(&mut reducer, prepared);
        assert!(progress.publication.is_none());
        assert!(progress.effects.is_empty());
        assert_eq!(reducer.snapshot(), after);
    }
    let prepared = reducer
        .prepare_retirement_completion(descriptor.batch())
        .unwrap();
    let completed = commit(&mut reducer, prepared);
    let Resolution::RetirementCompleted(completed_plan) = completed.resolution else {
        panic!("completion missing");
    };
    assert_eq!(completed_plan.descriptor.binding(), context.binding());
    assert_eq!(
        completed_plan.descriptor.acceptance_revision(),
        descriptor.acceptance_revision()
    );
    assert!(completed.publication.is_none());
    assert!(completed.effects.is_empty());
    assert_eq!(reducer.snapshot(), after);
    assert!(reducer.retained().pending_retirements().is_empty());
}

#[test]
fn foreign_server_end_all_is_rejected_without_consuming_local_assessment_or_grants() {
    let (mut reducer, _) = mixed_control_reducer();
    let assessed = apply(
        &mut reducer,
        Command::AssessEndAllSessions { server: SERVER },
        4,
    );
    let Resolution::Completed(Outcome::Assessment(assessment)) = assessed.resolution else {
        panic!("assessment missing");
    };
    let before = reducer.snapshot();
    let retained = reducer.retained();
    let store_revision = reducer.store_revision();
    let foreign = ServerId::new([2; 16]);
    for command in [
        Command::AssessEndAllSessions { server: foreign },
        Command::EndAllSessions {
            server: foreign,
            assessment: assessment.reference(),
        },
    ] {
        assert_eq!(
            reducer
                .prepare_command(&command, &retiring_context(5))
                .err(),
            Some(ReducerFailure::WrongServer)
        );
        assert_eq!(reducer.snapshot(), before);
        assert_eq!(reducer.retained(), retained);
        assert_eq!(reducer.store_revision(), store_revision);
    }
    let accepted = apply(
        &mut reducer,
        Command::EndAllSessions {
            server: SERVER,
            assessment: assessment.reference(),
        },
        5,
    );
    let Resolution::AwaitingRetirement(plan) = accepted.resolution else {
        panic!("foreign rejection consumed the local assessment");
    };
    assert_eq!(plan.descriptor.assessment(), assessment.reference());
    assert_eq!(
        plan.descriptor.target(),
        AssessmentTarget::EndAllSessions(SERVER)
    );
    assert_eq!(plan.descriptor.binding(), retiring_context(5).binding());
    assert_eq!(
        replay(before.clone(), accepted.publication.as_ref().unwrap()),
        reducer.snapshot()
    );
    for (prior, current) in before.runtime().iter().zip(reducer.snapshot().runtime()) {
        assert_eq!(current.control, prior.control);
        assert_eq!(current.control_generation, prior.control_generation);
    }
}
