//! The desktop client runtime and its shared contracts, driven through a recording provider.

use std::cell::RefCell;
use std::collections::BTreeSet;
use std::rc::Rc;
use std::sync::Arc;

use gpui::{AppContext as _, Entity, TestAppContext};
use spaceterm_client::failure::{ClientFailure, ClientFailureClass};
use spaceterm_client::view::ExplicitUseSource;
use spaceterm_core::authority::FieldVersion;
use spaceterm_core::geometry::{CellPixels, SUPPORTED_GRID_LIMITS};
use spaceterm_core::identity::{OperationStreamId, OperationStreamKey, PairingId};
use spaceterm_core::operations::{FieldConflict, OperationSequence};
use spaceterm_core::organization::{NameMode, NameText, WorkspaceName};

use super::testing::RecordingClientProvider;
use super::{
    DesktopClientDependencies, DesktopClientEvent, DesktopClientNotifier, DesktopClientRuntime,
    MAX_PENDING_UI_REQUESTS, MAX_QUEUED_OUTCOMES, OperationOutcomeNotice, notification_channel,
};
use crate::ui::server_desktop_contracts::{
    AttachmentGeneration, ClientId, Command, ContextGeneration, DesktopClientError, DesktopRequest,
    GridSize, LayoutRevision, MeasuredGrid, OperationId, Outcome, PaneAction, PaneActionRoute,
    PaneId, PaneMeasurement, PaneViewToken, SelectionSource, ServerId, TabId, UiRequestId,
    UseTicket, WorkspaceId,
};

struct Harness {
    runtime: Entity<DesktopClientRuntime>,
    provider: Rc<RecordingClientProvider>,
    notifier: DesktopClientNotifier,
    events: Rc<RefCell<Vec<DesktopClientEvent>>>,
}

fn runtime(cx: &mut TestAppContext) -> Harness {
    let provider = Rc::new(RecordingClientProvider::default());
    let (notifier, notifications) = notification_channel();
    let dependencies = DesktopClientDependencies {
        provider: provider.clone(),
        notifications,
    };
    let runtime = cx.new(|cx| DesktopClientRuntime::new(dependencies, cx));
    let events = Rc::new(RefCell::new(Vec::new()));
    let recorded = Rc::clone(&events);
    cx.update(|cx| {
        cx.subscribe(&runtime, move |_, event: &DesktopClientEvent, _| {
            recorded.borrow_mut().push(event.clone());
        })
        .detach();
    });
    Harness {
        runtime,
        provider,
        notifier,
        events,
    }
}

fn server() -> ServerId {
    ServerId::new([7; 16])
}

fn operation(sequence: u64) -> OperationId {
    let stream = OperationStreamId::new(
        OperationStreamKey::new(server(), 1).unwrap(),
        PairingId::new(server(), 1).unwrap(),
        ClientId::new(server(), 1).unwrap(),
    )
    .unwrap();
    OperationId::new(stream, OperationSequence::new(sequence).unwrap())
}

fn outcome_notice(sequence: u64, outcome: Outcome) -> OperationOutcomeNotice {
    OperationOutcomeNotice {
        server: server(),
        operation: operation(sequence),
        outcome: Arc::new(outcome),
    }
}

fn retry(cx: &mut TestAppContext, harness: &Harness) -> Result<UiRequestId, DesktopClientError> {
    harness.runtime.update(cx, |runtime, cx| {
        runtime.request(DesktopRequest::RetryConnection(server()), cx)
    })
}

fn frames(events: &[DesktopClientEvent]) -> Vec<ServerId> {
    events
        .iter()
        .filter_map(|event| match event {
            DesktopClientEvent::FrameChanged(server) => Some(*server),
            _ => None,
        })
        .collect()
}

#[gpui::test]
fn desktop_server_pending_request_bound_rejects_129_before_effects(cx: &mut TestAppContext) {
    let harness = runtime(cx);
    assert_eq!(MAX_PENDING_UI_REQUESTS, 128);
    let admitted = (0..MAX_PENDING_UI_REQUESTS)
        .map(|_| retry(cx, &harness).expect("the first 128 requests are admitted"))
        .collect::<BTreeSet<_>>();
    assert_eq!(admitted.len(), 128, "each request has its own identity");

    assert_eq!(retry(cx, &harness), Err(DesktopClientError::AdmissionFull));
    assert_eq!(harness.provider.begun().len(), 128, "the refusal had no effect");

    harness.provider.finish(0, Ok(None));
    cx.run_until_parked();
    let next = retry(cx, &harness).expect("a finished request frees its slot");
    assert!(!admitted.contains(&next));
    assert_eq!(harness.provider.begun().len(), 129);
}

#[gpui::test]
fn desktop_server_request_results_are_reported_by_local_identity(cx: &mut TestAppContext) {
    let harness = runtime(cx);
    let completed = retry(cx, &harness).unwrap();
    let failed = retry(cx, &harness).unwrap();

    harness.provider.finish(1, Err(DesktopClientError::StaleContext));
    harness.provider.finish(0, Ok(None));
    cx.run_until_parked();

    let events = harness.events.borrow();
    assert_eq!(events.len(), 2);
    assert!(events.contains(&DesktopClientEvent::RequestCompleted {
        request: completed,
        operation: None,
    }));
    assert!(events.contains(&DesktopClientEvent::RequestFailed {
        request: failed,
        error: DesktopClientError::StaleContext,
    }));
}

#[gpui::test]
fn desktop_server_provider_refusal_returns_before_any_effect(cx: &mut TestAppContext) {
    let harness = runtime(cx);
    harness.provider.refuse_with(DesktopClientError::Offline);

    assert_eq!(retry(cx, &harness), Err(DesktopClientError::Offline));
    cx.run_until_parked();

    assert!(harness.provider.begun().is_empty());
    assert!(harness.events.borrow().is_empty());
}

#[gpui::test]
fn desktop_server_frame_changes_reach_subscribers(cx: &mut TestAppContext) {
    let harness = runtime(cx);
    harness.notifier.frame_changed(server());
    cx.run_until_parked();
    assert_eq!(frames(&harness.events.borrow()), vec![server()]);

    harness.notifier.frame_changed(server());
    cx.run_until_parked();
    assert_eq!(
        frames(&harness.events.borrow()),
        vec![server(), server()],
        "a change after a delivered wakeup wakes the runtime again"
    );
}

#[gpui::test]
fn desktop_server_frame_wakeups_coalesce_and_every_outcome_arrives(cx: &mut TestAppContext) {
    let harness = runtime(cx);
    let other = ServerId::new([8; 16]);
    for index in 0..1_000 {
        harness
            .notifier
            .frame_changed(if index % 2 == 0 { server() } else { other });
    }
    let total = 3 * MAX_QUEUED_OUTCOMES as u64;
    let notifier = harness.notifier.clone();
    let sent = cx.foreground_executor().spawn(async move {
        for sequence in 1..=total {
            notifier
                .operation_outcome(outcome_notice(sequence, Outcome::Applied))
                .await?;
        }
        Ok::<_, DesktopClientError>(())
    });
    cx.run_until_parked();
    assert_eq!(cx.foreground_executor().block_test(sent), Ok(()));

    let events = harness.events.borrow();
    assert_eq!(
        frames(&events),
        vec![server(), other],
        "1,000 changes before the runtime ran wake it once and name each server once"
    );
    let delivered = events
        .iter()
        .filter_map(|event| match event {
            DesktopClientEvent::OperationOutcome { operation, .. } => Some(*operation),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        delivered,
        (1..=total).map(operation).collect::<Vec<_>>(),
        "outcomes past the queue bound wait, and none is dropped, merged or reordered"
    );
}

#[gpui::test]
fn desktop_server_outcome_delivery_reports_a_dropped_runtime(cx: &mut TestAppContext) {
    let harness = runtime(cx);
    let notifier = harness.notifier.clone();
    drop(harness);
    // Flushing effects releases the dropped runtime and its notification tasks.
    cx.update(|_| {});
    cx.run_until_parked();

    let delivery = cx.foreground_executor().spawn(async move {
        notifier
            .operation_outcome(outcome_notice(1, Outcome::Applied))
            .await
    });
    assert_eq!(
        cx.foreground_executor().block_test(delivery),
        Err(DesktopClientError::Cancelled)
    );
}

#[gpui::test]
fn desktop_server_operation_outcomes_reach_subscribers_by_operation(cx: &mut TestAppContext) {
    let harness = runtime(cx);
    let notice = outcome_notice(4, Outcome::Applied);
    let delivery = cx.foreground_executor().spawn({
        let notifier = harness.notifier.clone();
        let notice = notice.clone();
        async move { notifier.operation_outcome(notice).await }
    });
    cx.run_until_parked();
    assert_eq!(cx.foreground_executor().block_test(delivery), Ok(()));

    assert_eq!(
        *harness.events.borrow(),
        vec![DesktopClientEvent::OperationOutcome {
            server: notice.server,
            operation: notice.operation,
            outcome: notice.outcome,
        }]
    );
}

#[gpui::test]
fn desktop_server_runtime_reads_frames_and_tokens_from_its_provider(cx: &mut TestAppContext) {
    let harness = runtime(cx);
    let pane = PaneId::new(server(), 3).unwrap();

    harness.runtime.read_with(cx, |runtime, _| {
        assert!(runtime.frame(server()).is_none());
        assert_eq!(
            runtime.pane_view_token(pane),
            Err(DesktopClientError::TargetGone)
        );
    });
}

#[gpui::test]
fn desktop_server_quit_preparation_leaves_pending_requests_running(cx: &mut TestAppContext) {
    let harness = runtime(cx);
    let pending = retry(cx, &harness).unwrap();

    let quit = harness
        .runtime
        .update(cx, |runtime, cx| runtime.prepare_client_quit(cx));
    assert_eq!(cx.foreground_executor().block_test(quit), Ok(()));
    cx.run_until_parked();
    assert_eq!(harness.provider.quits(), 1);
    assert!(
        harness.events.borrow().is_empty(),
        "quitting finishes no request"
    );

    harness.provider.finish(0, Ok(Some(operation(9))));
    cx.run_until_parked();

    assert_eq!(
        *harness.events.borrow(),
        vec![DesktopClientEvent::RequestCompleted {
            request: pending,
            operation: Some(operation(9)),
        }],
        "the request admitted before quit still reports its own completion"
    );
}

#[gpui::test]
fn desktop_server_requests_keep_their_explicit_use_source(cx: &mut TestAppContext) {
    let harness = runtime(cx);
    let pane = PaneId::new(server(), 3).unwrap();
    let tab = TabId::new(server(), 2).unwrap();

    for source in [
        SelectionSource::Pointer,
        SelectionSource::Command,
        SelectionSource::AccessibilityPress,
    ] {
        for (request, explicit_use) in [
            (
                DesktopRequest::SelectPane { pane, source },
                Some(ExplicitUseSource::PaneSelection(source)),
            ),
            (
                DesktopRequest::OpenTab { tab, source },
                Some(ExplicitUseSource::TabOpening(source)),
            ),
            (
                DesktopRequest::StartSession { pane, source },
                Some(ExplicitUseSource::StartSession(source)),
            ),
            (
                DesktopRequest::Retry { pane, source },
                Some(ExplicitUseSource::Retry(source)),
            ),
            // Take Over asks for control. It begins no explicit use.
            (DesktopRequest::TakeOver { pane, source }, None),
        ] {
            assert_eq!(request.selection_source(), Some(source), "{request:?}");
            assert_eq!(request.explicit_use(), explicit_use, "{request:?}");

            let begun = harness.provider.begun().len();
            harness
                .runtime
                .update(cx, |runtime, cx| runtime.request(request.clone(), cx))
                .unwrap();
            assert_eq!(
                harness.provider.begun().get(begun),
                Some(&request),
                "the provider receives the request with its source"
            );
        }
    }

    let connection = DesktopRequest::RetryConnection(server());
    assert_eq!(connection.selection_source(), None);
    assert_eq!(connection.explicit_use(), None);
}

#[gpui::test]
fn desktop_server_pane_actions_reach_their_owner_with_their_source(cx: &mut TestAppContext) {
    let harness = runtime(cx);
    let other = ServerId::new([8; 16]);
    let pane = PaneId::new(other, 3).unwrap();

    for source in [
        SelectionSource::Pointer,
        SelectionSource::Command,
        SelectionSource::AccessibilityPress,
    ] {
        for (action, request) in [
            (
                PaneAction::StartSession(source),
                DesktopRequest::StartSession { pane, source },
            ),
            (
                PaneAction::Retry(source),
                DesktopRequest::Retry { pane, source },
            ),
            (
                PaneAction::TakeOver(source),
                DesktopRequest::TakeOver { pane, source },
            ),
        ] {
            assert_eq!(
                action.route(pane),
                PaneActionRoute::Request(request.clone()),
                "{action:?}"
            );
            let begun = harness.provider.begun().len();
            harness
                .runtime
                .update(cx, |runtime, cx| runtime.request(request.clone(), cx))
                .unwrap();
            assert_eq!(
                harness.provider.begun().get(begun),
                Some(&request),
                "the provider receives the Pane's request with its source"
            );
        }
    }

    // A connection retry targets the Pane's own server and begins no use, even from a frozen
    // screen whose button reads "Retry".
    let PaneActionRoute::Request(connection) = PaneAction::RetryConnection.route(pane) else {
        panic!("a connection retry is a runtime request");
    };
    assert_eq!(connection, DesktopRequest::RetryConnection(other));
    assert_eq!(connection.explicit_use(), None);

    // Choosing a directory and opening Settings send nothing to the runtime.
    assert_eq!(
        PaneAction::ChooseDirectory.route(pane),
        PaneActionRoute::ChooseDirectory(pane)
    );
    assert_eq!(
        PaneAction::OpenServerSettings.route(pane),
        PaneActionRoute::ServerSettings(other)
    );
}

#[test]
fn desktop_server_client_failures_map_to_typed_desktop_classes() {
    use ClientFailureClass as Client;
    use DesktopClientError as Desktop;

    let mapped = [
        (Client::Unavailable, Desktop::Offline),
        (Client::AuthorizationDenied, Desktop::AccessDenied),
        (Client::PinMismatch, Desktop::IdentityUnverified),
        (Client::Incompatible, Desktop::Incompatible),
        (Client::MissingCapability, Desktop::UnsupportedCapability),
        (Client::InvalidPeer, Desktop::Offline),
        (Client::UnsafeEndpoint, Desktop::Offline),
        (Client::StaleAuthority, Desktop::StaleContext),
        (Client::TargetNotFound, Desktop::TargetGone),
        (Client::ResourceLimit, Desktop::LimitReached),
        (Client::StorageUnavailable, Desktop::LocalStorageUnavailable),
        (Client::UnsafeStorage, Desktop::LocalStorageUnavailable),
        (Client::InvalidStorage, Desktop::LocalStorageUnavailable),
        (Client::UnknownSchema, Desktop::LocalStorageUnavailable),
        (Client::RecoveryRequired, Desktop::LocalStorageUnavailable),
        (Client::CounterExhausted, Desktop::CounterExhausted),
        (Client::Cancelled, Desktop::Cancelled),
    ];
    for (class, expected) in mapped {
        assert_eq!(
            Desktop::from(ClientFailure::new(class)),
            expected,
            "{class:?}"
        );
    }

    // Only the runtime's own 128-request bound says earlier requests are still finishing.
    assert!(
        mapped
            .iter()
            .all(|(_, desktop)| *desktop != Desktop::AdmissionFull)
    );
    // A cancelled request is not evidence that a reachable server is offline.
    assert_ne!(
        Desktop::from(ClientFailure::new(Client::Cancelled)),
        Desktop::Offline
    );
    // An identity or authorization refusal is never one of the classes that show no Alert.
    for class in [Client::PinMismatch, Client::AuthorizationDenied] {
        assert!(!matches!(
            Desktop::from(ClientFailure::new(class)),
            Desktop::TargetGone
                | Desktop::WrongServer
                | Desktop::StaleContext
                | Desktop::InvalidGeometry
                | Desktop::Cancelled
        ));
    }
    // A refusal that waiting cannot fix never reuses the class that invites a retry.
    assert_ne!(
        Desktop::from(ClientFailure::new(Client::AuthorizationDenied)),
        Desktop::AuthorizationUnavailable
    );
}

#[test]
fn desktop_server_request_debug_names_only_the_request_kind() {
    let pane = PaneId::new(server(), 3).unwrap();
    let source = SelectionSource::Pointer;
    for (request, expected) in [
        (
            DesktopRequest::RetryConnection(server()),
            "DesktopRequest::RetryConnection",
        ),
        (
            DesktopRequest::StartSession { pane, source },
            "DesktopRequest::StartSession",
        ),
        (
            DesktopRequest::Retry { pane, source },
            "DesktopRequest::Retry",
        ),
        (
            DesktopRequest::TakeOver { pane, source },
            "DesktopRequest::TakeOver",
        ),
        (
            DesktopRequest::Shared {
                server: server(),
                command: Command::AssessEndAllSessions { server: server() },
            },
            "DesktopRequest::Shared",
        ),
    ] {
        assert_eq!(format!("{request:?}"), expected);
    }
}

#[test]
fn desktop_server_event_debug_names_outcome_kinds_and_identities_only() {
    let name = "Quarterly payroll";
    let conflict = Outcome::Conflict(FieldConflict::WorkspaceName {
        workspace: WorkspaceId::new(server(), 5).unwrap(),
        current_value: WorkspaceName {
            mode: NameMode::Custom,
            display: NameText::new(name.to_owned()).unwrap(),
        },
        current_version: FieldVersion::new(3),
    });
    let notice = outcome_notice(4, conflict);
    let event = DesktopClientEvent::OperationOutcome {
        server: notice.server,
        operation: notice.operation,
        outcome: Arc::clone(&notice.outcome),
    };

    let identities = format!(
        "server: {:?}, operation: {:?}, outcome: Conflict }}",
        server(),
        operation(4)
    );
    assert_eq!(
        format!("{event:?}"),
        format!("DesktopClientEvent::OperationOutcome {{ {identities}")
    );
    assert_eq!(
        format!("{notice:?}"),
        format!("OperationOutcomeNotice {{ {identities}")
    );
    for text in [format!("{event:?}"), format!("{notice:?}")] {
        assert!(!text.contains(name));
        assert!(!text.contains("WorkspaceName"));
    }

    let failed = DesktopClientEvent::RequestFailed {
        request: UiRequestId::FIRST,
        error: DesktopClientError::LimitReached,
    };
    assert_eq!(
        format!("{failed:?}"),
        "DesktopClientEvent::RequestFailed { request: UiRequestId(1), error: LimitReached }"
    );
}

fn measurement(grid: MeasuredGrid) -> PaneMeasurement {
    PaneMeasurement {
        token: PaneViewToken {
            pane: PaneId::new(server(), 3).unwrap(),
            attachment_generation: AttachmentGeneration::FIRST,
            context_generation: ContextGeneration::new(5),
        },
        ticket: UseTicket::FIRST,
        layout: LayoutRevision::new(2),
        grid,
    }
}

#[test]
fn desktop_server_measurement_keeps_its_ticket_context_layout_and_full_grid() {
    let cell = CellPixels::new(9, 18).unwrap();
    let grid = MeasuredGrid::new(24, 80, cell, SUPPORTED_GRID_LIMITS).unwrap();

    let view = measurement(grid).view_measurement().unwrap();

    assert_eq!(
        (view.ticket, view.context, view.layout, view.grid),
        (
            UseTicket::FIRST,
            ContextGeneration::new(5),
            LayoutRevision::new(2),
            GridSize::new(24, 80, cell, SUPPORTED_GRID_LIMITS).unwrap()
        )
    );
}

#[test]
fn desktop_server_measurement_keeps_cells_whose_pty_width_overflows() {
    // 1024 columns of 64 device pixels is 65,536 pixels, one past the PTY's u16 width.
    let grid = MeasuredGrid::new(
        24,
        1024,
        CellPixels::new(64, 36).unwrap(),
        SUPPORTED_GRID_LIMITS,
    )
    .unwrap();
    let measured = measurement(grid);

    assert_eq!(measured.grid, grid, "the measurement keeps cells and metrics");
    assert_eq!((measured.grid.columns(), measured.grid.rows()), (1024, 24));
    assert_eq!(measured.grid.pty_pixels(), (0, 864));
    assert_eq!(
        measured.view_measurement(),
        Err(DesktopClientError::InvalidGeometry),
        "legacy client-view geometry refuses it instead of truncating it"
    );
}
