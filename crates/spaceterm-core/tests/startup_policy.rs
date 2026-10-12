use spaceterm_core::{
    authority::*, control::*, geometry::*, identity::*, operations::*, organization::*,
    startup_policy::*,
};

fn facts(source: StartupSource) -> (StartupIntent, StartupContext) {
    let server = ServerId::new([1; 16]);
    let pane = PaneId::new(server, 1).unwrap();
    let client = ClientId::new(server, 1).unwrap();
    let stream = OperationStreamId::new(
        OperationStreamKey::new(server, 1).unwrap(),
        PairingId::new(server, 1).unwrap(),
        client,
    )
    .unwrap();
    let session = SessionKey {
        epoch: ServerEpoch::new(2),
        pane,
        generation: SessionGeneration::new(3),
    };
    let grant = ControlGrant::new(pane, client, ControlGeneration::new(4)).unwrap();
    let connection = ConnectionBinding::new(client, ConnectionGeneration::new(5));
    let fence =
        EffectFence::new(session, grant, connection, ContextGeneration::new(6), None).unwrap();
    let intent = StartupIntent::new(
        OperationId::new(stream, OperationSequence::new(1).unwrap()),
        fence,
        source,
    )
    .unwrap();
    (
        intent,
        StartupContext {
            epoch: session.epoch,
            runtime: PaneRuntimeRecord {
                pane,
                session_generation: session.generation,
                phase: SessionPhase::Ended {
                    reason: EndReason::EndAllSessions,
                },
                control_generation: grant.generation(),
                control: ControlRecord::Held {
                    client,
                    generation: grant.generation(),
                },
            },
            tab: TabId::new(server, 1).unwrap(),
            connection,
            context: fence.context(),
            admissions_open: true,
        },
    )
}

#[test]
fn all_three_explicit_sources_can_complete_nondefault_measurement() {
    for source in [
        StartupSource::ExplicitSelection,
        StartupSource::StartSession,
        StartupSource::TabOpening {
            tab: TabId::new(ServerId::new([1; 16]), 1).unwrap(),
        },
    ] {
        let (intent, context) = facts(source);
        let grid = MeasuredGrid::new(
            41,
            117,
            CellPixels::new(9, 18).unwrap(),
            SUPPORTED_GRID_LIMITS,
        )
        .unwrap();
        assert_eq!(
            validate_measurement(
                intent,
                MeasuredStartupMeasurement {
                    intent: intent.id(),
                    fence: intent.fence(),
                    grid
                },
                context,
                SUPPORTED_GRID_LIMITS
            ),
            Ok(grid)
        );
    }
}

#[test]
fn reconnect_and_changed_context_cannot_complete_old_intent() {
    let (intent, mut context) = facts(StartupSource::ExplicitSelection);
    context.connection.generation = ConnectionGeneration::new(7);
    assert_eq!(
        validate_explicit_use(intent, context),
        Err(StartupPolicyError::StaleAuthority)
    );
    context = facts(StartupSource::ExplicitSelection).1;
    context.context = ContextGeneration::new(7);
    assert_eq!(
        validate_explicit_use(intent, context),
        Err(StartupPolicyError::StaleAuthority)
    );
}

#[test]
fn tab_opening_cannot_authorize_another_tab_or_running_session() {
    let (intent, mut context) = facts(StartupSource::TabOpening {
        tab: TabId::new(ServerId::new([1; 16]), 2).unwrap(),
    });
    assert_eq!(
        validate_explicit_use(intent, context),
        Err(StartupPolicyError::ScopeMismatch)
    );
    let (intent, _) = facts(StartupSource::ExplicitSelection);
    context.runtime.phase = SessionPhase::Running {
        generation: context.runtime.session_generation,
    };
    assert_eq!(
        validate_explicit_use(intent, context),
        Err(StartupPolicyError::AlreadyStarted)
    );
}

#[test]
fn end_all_barrier_and_lower_geometry_limit_reject_before_reservation() {
    let (intent, mut context) = facts(StartupSource::StartSession);
    context.admissions_open = false;
    assert_eq!(
        validate_explicit_use(intent, context),
        Err(StartupPolicyError::AdmissionsClosed)
    );
    context.admissions_open = true;
    let grid = MeasuredGrid::new(
        40,
        100,
        CellPixels::new(9, 18).unwrap(),
        SUPPORTED_GRID_LIMITS,
    )
    .unwrap();
    assert_eq!(
        validate_measurement(
            intent,
            MeasuredStartupMeasurement {
                intent: intent.id(),
                fence: intent.fence(),
                grid
            },
            context,
            GridLimits::new(40, 99, 3960).unwrap()
        ),
        Err(StartupPolicyError::InvalidGeometry)
    );
}
