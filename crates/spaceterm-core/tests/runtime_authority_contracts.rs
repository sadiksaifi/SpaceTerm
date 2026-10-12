use spaceterm_core::authority::*;
use spaceterm_core::identity::*;
use spaceterm_core::runtime_authority::*;

fn parts() -> RuntimeAuthorityParts {
    let server = ServerId::new([7; 16]);
    let pane = PaneId::new(server, 1).unwrap();
    let client = ClientId::new(server, 1).unwrap();
    RuntimeAuthorityParts {
        session: SessionKey {
            pane,
            epoch: ServerEpoch::new(3),
            generation: SessionGeneration::new(2),
        },
        grant: Some(ControlGrant::new(pane, client, ControlGeneration::new(1)).unwrap()),
        connection: Some(ConnectionBinding::new(client, ConnectionGeneration::new(1))),
        context: ContextGeneration::new(1),
        focused_pane: true,
        terminal_input_focus: true,
        native_effects_eligible: true,
    }
}
#[test]
fn authority_rejects_mixed_control_and_connection_clients() {
    let mut value = parts();
    value.connection = Some(ConnectionBinding::new(
        ClientId::new(value.session.pane.server(), 2).unwrap(),
        ConnectionGeneration::new(1),
    ));
    assert_eq!(RuntimeAuthority::new(value), Err(FenceError::WrongBinding));
}
#[test]
fn authority_barrier_cannot_acknowledge_another_session() {
    let expected = RuntimeAuthority::new(parts()).unwrap();
    let mut next = parts();
    next.session.generation = SessionGeneration::new(3);
    assert_eq!(
        RuntimeAuthorityTransition::new(expected, RuntimeAuthority::new(next).unwrap()),
        Err(FenceError::WrongBinding)
    );
}
#[test]
fn takeover_acknowledgement_keeps_the_exact_expected_and_new_authority() {
    let expected = RuntimeAuthority::new(parts()).unwrap();
    let mut next = parts();
    next.grant = Some(
        ControlGrant::new(
            next.session.pane,
            next.grant.unwrap().client(),
            ControlGeneration::new(2),
        )
        .unwrap(),
    );
    let next = RuntimeAuthority::new(next).unwrap();
    let transition = RuntimeAuthorityTransition::new(expected, next).unwrap();
    let acknowledgement = AuthorityApplied::new(transition);
    assert_eq!(acknowledgement.transition(), transition);
}

#[test]
fn classified_terminal_failure_survives_successful_cleanup() {
    use spaceterm_core::{organization::*, retirement::*};
    let session = parts().session;
    for status in [
        RuntimeTerminalStatus::StartupFailed {
            class: FailureClass::LaunchUnavailable,
        },
        RuntimeTerminalStatus::RuntimeFailed {
            class: FailureClass::ResourceLimit,
        },
        RuntimeTerminalStatus::RuntimeFailed {
            class: FailureClass::RuntimeFault,
        },
    ] {
        let terminal = SessionTerminalResult::new(session, status).unwrap();
        let cleanup = SessionCleanupObservation::new(
            session,
            RetirementReason::RuntimeFailure,
            CleanupResult::Confirmed(ConfirmedCleanupClass::ReapedAndReleased),
        )
        .unwrap();
        let completion = RuntimeCompletion::new(terminal, cleanup).unwrap();
        assert_eq!(completion.terminal().status(), status);
        assert_eq!(completion.cleanup().result(), cleanup.result());
        assert_eq!(
            RuntimeLifecycle::Finished(completion).terminal_result(),
            Some(terminal)
        );
    }
}

#[test]
fn cleanup_failure_does_not_replace_the_terminal_result() {
    use spaceterm_core::{organization::*, retirement::*};
    let session = parts().session;
    let terminal = SessionTerminalResult::new(
        session,
        RuntimeTerminalStatus::RuntimeFailed {
            class: FailureClass::ResourceLimit,
        },
    )
    .unwrap();
    let cleanup = SessionCleanupObservation::new(
        session,
        RetirementReason::RuntimeFailure,
        CleanupResult::Failed(FailureClass::RuntimeUnavailable),
    )
    .unwrap();
    let completion = RuntimeCompletion::new(terminal, cleanup).unwrap();
    assert_eq!(completion.terminal(), terminal);
    assert_eq!(completion.cleanup(), cleanup);
}

#[test]
fn terminal_and_cleanup_must_bind_the_same_session_and_first_reason() {
    use spaceterm_core::{organization::*, retirement::*};
    let session = parts().session;
    let terminal = SessionTerminalResult::new(
        session,
        RuntimeTerminalStatus::RuntimeFailed {
            class: FailureClass::RuntimeFault,
        },
    )
    .unwrap();
    for cleanup in [
        SessionCleanupObservation::new(
            SessionKey {
                generation: SessionGeneration::new(3),
                ..session
            },
            RetirementReason::RuntimeFailure,
            CleanupResult::Failed(FailureClass::RuntimeUnavailable),
        )
        .unwrap(),
        SessionCleanupObservation::new(
            session,
            RetirementReason::Stop,
            CleanupResult::Failed(FailureClass::RuntimeUnavailable),
        )
        .unwrap(),
    ] {
        assert_eq!(
            RuntimeCompletion::new(terminal, cleanup),
            Err(FenceError::WrongBinding)
        );
    }
    assert_eq!(
        SessionTerminalResult::new(
            SessionKey {
                epoch: ServerEpoch::new(0),
                ..session
            },
            RuntimeTerminalStatus::OrdinaryExit {
                exit_status: Some(0)
            }
        ),
        Err(FenceError::WrongBinding)
    );
}

#[test]
fn ordinary_exit_and_explicit_retirement_remain_distinct_while_cleanup_is_pending() {
    use spaceterm_core::organization::RetirementReason;
    let session = parts().session;
    let exit = SessionTerminalResult::new(
        session,
        RuntimeTerminalStatus::OrdinaryExit {
            exit_status: Some(7),
        },
    )
    .unwrap();
    assert_eq!(exit.retirement_reason(), RetirementReason::OrdinaryExit);
    let stopped = SessionTerminalResult::new(
        session,
        RuntimeTerminalStatus::Retired {
            reason: RetirementReason::Stop,
        },
    )
    .unwrap();
    assert_eq!(
        RuntimeLifecycle::Retiring { terminal: stopped }.terminal_result(),
        Some(stopped)
    );
    for reason in [
        RetirementReason::OrdinaryExit,
        RetirementReason::RuntimeFailure,
    ] {
        assert_eq!(
            SessionTerminalResult::new(session, RuntimeTerminalStatus::Retired { reason }),
            Err(FenceError::WrongBinding)
        );
    }
}
