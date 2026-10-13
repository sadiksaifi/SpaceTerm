use super::*;
use crate::{
    domain::server_view_testing::{Org, Replica, ready},
    ui::{
        directory_picker::server_tests::Peer,
        server_client_runtime::testing::RecordingClientProvider,
        server_client_runtime::{
            DesktopClientDependencies, DesktopClientNotifier, DesktopClientProvider,
            notification_channel,
        },
        server_desktop_contracts::{AttachmentGeneration, DesktopServerFrame},
        tab_view::server_testing::{frame, operation},
    },
};
use gpui::{App, IntoElement, ParentElement as _, Render, TestAppContext, VisualTestContext, div};
use spaceterm_client::api::{ConnectedInfo, ConnectionCapabilities};
use spaceterm_core::{
    authority::{ContextGeneration, OrganizationRevision},
    organization::OrganizationSnapshot,
};
use std::cell::{Cell, RefCell};

struct Provider {
    recorded: RecordingClientProvider,
    token: Cell<PaneViewToken>,
}
impl DesktopClientProvider for Provider {
    fn frame(&self, server: spaceterm_core::identity::ServerId) -> Option<Arc<DesktopServerFrame>> {
        self.recorded.frame(server)
    }
    fn pane_view_token(&self, pane: PaneId) -> Result<PaneViewToken, DesktopClientError> {
        let token = self.token.get();
        if pane == token.pane {
            Ok(token)
        } else {
            Err(DesktopClientError::TargetGone)
        }
    }
    fn begin(
        &self,
        request: DesktopRequest,
        cx: &mut App,
    ) -> Result<gpui::Task<Result<Option<OperationId>, DesktopClientError>>, DesktopClientError>
    {
        self.recorded.begin(request, cx)
    }
    fn outcome(&self, id: OperationId) -> Option<Arc<Outcome>> {
        self.recorded.outcome(id)
    }
    fn outcome_revision(&self, id: OperationId) -> Option<OrganizationRevision> {
        self.recorded.outcome_revision(id)
    }
    fn prepare_quit(&self, cx: &mut App) -> gpui::Task<Result<(), DesktopClientError>> {
        self.recorded.prepare_quit(cx)
    }
}
struct Root {
    repair: Entity<ServerPaneRepair>,
}
impl Render for Root {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div().child(self.repair.read(cx).picker())
    }
}
struct Harness {
    repair: Entity<ServerPaneRepair>,
    provider: Rc<Provider>,
    notifier: DesktopClientNotifier,
    events: Rc<RefCell<Vec<PaneRepairEvent>>>,
    peer: Arc<Peer>,
}
fn server() -> spaceterm_core::identity::ServerId {
    spaceterm_core::identity::ServerId::new([7; 16])
}
fn pane() -> PaneId {
    PaneId::new(server(), 2).unwrap()
}
fn dir(value: &str) -> ServerDirectory {
    ServerDirectory::new(server(), value.as_bytes().to_vec()).unwrap()
}
fn initial_frame() -> DesktopServerFrame {
    let mut org = Org::new(7);
    org.add_workspace(1, 1, 1).add_tab(1, 2, 2, Some(1));
    let replica = Replica::attached(org, 1);
    let mut frame = frame(&replica, ReplicaStatus::Ready);
    let snapshot = frame.organization.as_ref();
    let state = snapshot.state().clone();
    let mut runtime = snapshot.runtime().to_vec();
    for fact in &mut runtime {
        fact.control = ControlRecord::Held {
            client: ClientId::new(server(), 1).unwrap(),
            generation: fact.control_generation,
        };
        fact.phase = SessionPhase::DirectoryUnavailable;
    }
    frame.organization =
        Arc::new(OrganizationSnapshot::new(snapshot.epoch(), state, runtime).unwrap());
    let facts = ready(7);
    frame.connection = ConnectionState::Connected {
        info: ConnectedInfo {
            server: server(),
            pairing: facts.pairing,
            client: facts.client,
            epoch: frame.organization.epoch(),
            binding: facts.connection.unwrap(),
            major: 2,
            limits: spaceterm_protocol::limits::NegotiatedLimits::default(),
            capabilities: ConnectionCapabilities::NONE,
            organization_revision: OrganizationRevision::new(1),
            attempt: 1,
        },
    };
    frame
}
fn harness(cx: &mut TestAppContext) -> (Harness, &mut VisualTestContext) {
    cx.update(crate::ui::init).unwrap();
    let provider = Rc::new(Provider {
        recorded: RecordingClientProvider::default(),
        token: Cell::new(PaneViewToken {
            pane: pane(),
            attachment_generation: AttachmentGeneration::FIRST,
            context_generation: ContextGeneration::new(1),
        }),
    });
    provider.recorded.set_frame(Arc::new(initial_frame()));
    let (notifier, notifications) = notification_channel();
    let runtime = cx.new(|cx| {
        DesktopClientRuntime::new(
            DesktopClientDependencies {
                provider: provider.clone(),
                notifications,
            },
            cx,
        )
    });
    let events = Rc::new(RefCell::new(Vec::new()));
    let recorded = events.clone();
    let peer = Peer::new();
    let (root, cx) = cx.add_window_view(|window, cx| {
        let repair = cx.new(|cx| {
            ServerPaneRepair::new(
                runtime,
                pane(),
                SelectionSource::Command,
                "Owning server",
                peer.clone(),
                window,
                cx,
            )
            .unwrap()
        });
        cx.subscribe(&repair, move |_, _, event: &PaneRepairEvent, _| {
            recorded.borrow_mut().push(event.clone())
        })
        .detach();
        Root { repair }
    });
    let repair = root.read_with(cx, |r, _| r.repair.clone());
    (
        Harness {
            repair,
            provider,
            notifier,
            events,
            peer,
        },
        cx,
    )
}
fn starts(h: &Harness) -> Vec<DesktopRequest> {
    h.events
        .borrow()
        .iter()
        .filter_map(|e| {
            if let PaneRepairEvent::StartRequested { request, .. } = e {
                Some(request.as_ref().clone())
            } else {
                None
            }
        })
        .collect()
}
fn choose(h: &Harness, cx: &mut VisualTestContext) {
    cx.update(|window, cx| {
        window.activate_window();
        h.repair.update(cx, |r, cx| {
            assert!(r.open(window, cx));
        });
    });
    cx.run_until_parked();
    cx.simulate_keystrokes("cmd-enter");
    cx.run_until_parked();
}
fn publish_repair(h: &Harness, revision: u64, cx: &mut VisualTestContext) {
    let previous = h.provider.recorded.frame(server()).unwrap();
    let mut state = previous.organization.state().clone();
    state.revision = OrganizationRevision::new(revision);
    let pane = state.panes.iter_mut().find(|p| p.id == pane()).unwrap();
    pane.starting_directory = dir("/server/home");
    pane.confirmed_restoration_directory.value = None;
    pane.confirmed_restoration_directory.version = FieldVersion::new(2);
    let mut runtime = previous.organization.runtime().to_vec();
    runtime
        .iter_mut()
        .find(|p| p.pane == pane.id)
        .unwrap()
        .phase = SessionPhase::NotStarted;
    h.provider.recorded.set_frame(Arc::new(DesktopServerFrame {
        server: server(),
        connection: previous.connection.clone(),
        replica_status: ReplicaStatus::Ready,
        organization: Arc::new(
            OrganizationSnapshot::new(previous.organization.epoch(), state, runtime).unwrap(),
        ),
        personal: previous.personal.clone(),
        view_revision: previous.view_revision,
    }));
    h.notifier.frame_changed(server());
    cx.run_until_parked();
}

#[gpui::test]
fn pane_choose_directory_waits_for_original_durable_outcome_and_replica(cx: &mut TestAppContext) {
    let (h, cx) = harness(cx);
    let before = h.provider.recorded.frame(server()).unwrap();
    choose(&h, cx);
    assert_eq!(
        h.provider.recorded.begun(),
        [DesktopRequest::Shared {
            server: server(),
            command: Command::RepairPaneDirectory {
                pane: pane(),
                expected_version: FieldVersion::new(1),
                value: dir("/server/home"),
            }
        }]
    );
    assert!(starts(&h).is_empty());
    let id = operation(server(), 1);
    h.provider.recorded.finish(0, Ok(Some(id)));
    cx.run_until_parked();
    assert_eq!(
        h.repair.read_with(cx, |r, _| r.original_operation()),
        Some(id)
    );
    assert!(starts(&h).is_empty());
    h.provider.recorded.set_outcome_at(
        id,
        Arc::new(Outcome::Applied),
        OrganizationRevision::new(3),
    );
    publish_repair(&h, 2, cx);
    assert!(starts(&h).is_empty());
    publish_repair(&h, 3, cx);
    assert_eq!(
        starts(&h),
        [DesktopRequest::StartSession {
            pane: pane(),
            source: SelectionSource::Command
        }]
    );
    h.notifier.frame_changed(server());
    cx.run_until_parked();
    assert_eq!(starts(&h).len(), 1);
    let after = h.provider.recorded.frame(server()).unwrap();
    assert_eq!(
        before.organization.state().workspaces,
        after.organization.state().workspaces
    );
    assert_eq!(
        before.organization.state().tabs,
        after.organization.state().tabs
    );
    assert_eq!(
        before.organization.state().worktrees,
        after.organization.state().worktrees
    );
    assert_eq!(
        before.organization.state().panes[0],
        after.organization.state().panes[0]
    );
    assert_eq!(
        h.repair.read_with(cx, |r, _| r.status()),
        PaneRepairStatus::Completed
    );
}

#[gpui::test]
fn pane_retry_keeps_the_original_directory_and_reports_still_unavailable(cx: &mut TestAppContext) {
    use spaceterm_core::server_directories::{DirectoryQueryAction, DirectoryValidationClass};
    let (h, cx) = harness(cx);
    let before = h.provider.recorded.frame(server()).unwrap();
    *h.peer.class.lock().unwrap() = DirectoryValidationClass::Missing;
    for _ in 0..2 {
        cx.update(|window, cx| h.repair.update(cx, |r, cx| r.retry(window, cx)));
        cx.run_until_parked();
        assert_eq!(
            h.repair.read_with(cx, |r, _| r.status().message()),
            "Still unavailable."
        );
        assert!(starts(&h).is_empty());
    }
    assert_eq!(h.peer.queries.lock().unwrap().len(), 2);
    assert!(h.peer.queries.lock().unwrap().iter().all(|q|matches!(&q.parts().action,
        DirectoryQueryAction::Validate { directory,purpose:DirectoryPurpose::SessionLaunch } if directory==&dir("/work"))));
    assert!(h.provider.recorded.begun().is_empty());
    assert_eq!(
        before.organization.state(),
        h.provider
            .recorded
            .frame(server())
            .unwrap()
            .organization
            .state()
    );
    *h.peer.class.lock().unwrap() = DirectoryValidationClass::AvailableDirectory;
    cx.update(|window, cx| h.repair.update(cx, |r, cx| r.retry(window, cx)));
    cx.run_until_parked();
    assert_eq!(
        starts(&h),
        [DesktopRequest::Retry {
            pane: pane(),
            source: SelectionSource::Command
        }]
    );
    assert!(h.provider.recorded.begun().is_empty());
}

#[gpui::test]
fn cancelled_pane_repair_does_not_start_after_commit(cx: &mut TestAppContext) {
    let (h, cx) = harness(cx);
    choose(&h, cx);
    cx.update(|window, cx| h.repair.update(cx, |r, cx| r.cancel(window, cx)));
    let id = operation(server(), 1);
    h.provider.recorded.set_outcome_at(
        id,
        Arc::new(Outcome::Applied),
        OrganizationRevision::new(2),
    );
    h.provider.recorded.finish(0, Ok(Some(id)));
    cx.run_until_parked();
    publish_repair(&h, 2, cx);
    assert!(starts(&h).is_empty());
    assert_eq!(
        h.repair.read_with(cx, |r, _| r.status()),
        PaneRepairStatus::Cancelled
    );
}

#[gpui::test]
fn stale_attachment_context_cancels_pane_repair_startup(cx: &mut TestAppContext) {
    let (h, cx) = harness(cx);
    choose(&h, cx);
    let token = h.provider.token.get();
    h.provider.token.set(PaneViewToken {
        attachment_generation: AttachmentGeneration::new(2).unwrap(),
        ..token
    });
    let id = operation(server(), 1);
    h.provider.recorded.set_outcome_at(
        id,
        Arc::new(Outcome::Applied),
        OrganizationRevision::new(2),
    );
    h.provider.recorded.finish(0, Ok(Some(id)));
    cx.run_until_parked();
    publish_repair(&h, 2, cx);
    assert!(starts(&h).is_empty());
    assert_eq!(
        h.repair.read_with(cx, |r, _| r.status()),
        PaneRepairStatus::Cancelled
    );
}

#[gpui::test]
fn unresolved_pane_repair_cannot_issue_replacement_or_startup(cx: &mut TestAppContext) {
    let (h, cx) = harness(cx);
    choose(&h, cx);
    h.provider
        .recorded
        .finish(0, Err(DesktopClientError::Cancelled));
    cx.run_until_parked();
    cx.update(|window, cx| h.repair.update(cx, |r, cx| r.retry(window, cx)));
    assert_eq!(h.provider.recorded.begun().len(), 1);
    assert!(starts(&h).is_empty());
    assert_eq!(
        h.repair.read_with(cx, |r, _| r.status()),
        PaneRepairStatus::Unresolved
    );
}

#[gpui::test]
fn outcome_before_ui_receipt_uses_the_same_retained_operation(cx: &mut TestAppContext) {
    let (h, cx) = harness(cx);
    choose(&h, cx);
    let id = operation(server(), 1);
    h.provider.recorded.set_outcome_at(
        id,
        Arc::new(Outcome::Applied),
        OrganizationRevision::new(2),
    );
    publish_repair(&h, 2, cx);
    assert!(starts(&h).is_empty());
    h.provider.recorded.finish(0, Ok(Some(id)));
    cx.run_until_parked();
    assert_eq!(starts(&h).len(), 1);
    assert_eq!(h.provider.recorded.begun().len(), 1);
}

#[gpui::test]
fn pane_repair_rejects_wrong_server_without_shared_effects(cx: &mut TestAppContext) {
    let (h, cx) = harness(cx);
    let wrong = ServerDirectory::new(
        spaceterm_core::identity::ServerId::new([8; 16]),
        b"/wrong".to_vec(),
    )
    .unwrap();
    cx.update(|window, cx| h.repair.update(cx, |r, cx| r.choose(wrong, window, cx)));
    assert!(h.provider.recorded.begun().is_empty());
    assert!(starts(&h).is_empty());
}

#[gpui::test]
fn refused_pane_repair_announces_still_unavailable_and_restores_query_focus(
    cx: &mut TestAppContext,
) {
    use spaceterm_ui::a11y_testing::A11yTree;
    let (h, cx) = harness(cx);
    choose(&h, cx);
    let id = operation(server(), 1);
    h.provider.recorded.set_outcome_at(
        id,
        Arc::new(Outcome::Failed(OperationFailure::DirectoryUnavailable)),
        OrganizationRevision::new(1),
    );
    h.provider.recorded.finish(0, Ok(Some(id)));
    cx.run_until_parked();
    assert_eq!(
        h.repair.read_with(cx, |r, _| r.status()),
        PaneRepairStatus::StillUnavailable
    );
    let tree = A11yTree::read(cx);
    assert!(cx.debug_bounds("command-palette-empty-title").is_some());
    let status = tree.with_role("Status");
    assert_eq!(status.len(), 1);
    assert_eq!(status[0]["aria"]["label"], "Still unavailable.");
    assert!(tree.exposed(status[0]));
    let id = status[0]["accesskit_id"].clone();
    let picker = h.repair.read_with(cx, |r, _| r.picker());
    picker.update(cx, |_, cx| cx.notify());
    cx.run_until_parked();
    let tree = A11yTree::read(cx);
    assert_eq!(tree.with_role("Status")[0]["accesskit_id"], id);
    assert_eq!(tree.focused().unwrap()["aria"]["role"], "TextInput");
    assert!(starts(&h).is_empty());
    assert_eq!(
        h.repair.read_with(cx, |r, _| r.status()),
        PaneRepairStatus::StillUnavailable
    );
}

#[gpui::test]
fn retry_reconnected_before_completion_cancels_deliberate_start(cx: &mut TestAppContext) {
    let (h, cx) = harness(cx);
    let (sender, gate) = async_channel::bounded(1);
    *h.peer.gate.lock().unwrap() = Some(gate);
    cx.update(|window, cx| h.repair.update(cx, |r, cx| r.retry(window, cx)));
    cx.run_until_parked();
    h.peer.state.lock().unwrap().generation =
        spaceterm_core::authority::ConnectionGeneration::new(2);
    sender.try_send(()).unwrap();
    cx.run_until_parked();
    assert!(starts(&h).is_empty());
    assert_eq!(
        h.repair.read_with(cx, |r, _| r.status()),
        PaneRepairStatus::Cancelled
    );
}

#[gpui::test]
fn pane_repair_receipt_from_another_operation_principal_never_starts(cx: &mut TestAppContext) {
    use spaceterm_core::{
        identity::{OperationStreamId, OperationStreamKey},
        operations::OperationSequence,
    };
    let (h, cx) = harness(cx);
    choose(&h, cx);
    let stream = OperationStreamId::new(
        OperationStreamKey::new(server(), 2).unwrap(),
        PairingId::new(server(), 2).unwrap(),
        ClientId::new(server(), 2).unwrap(),
    )
    .unwrap();
    let id = OperationId::new(stream, OperationSequence::new(1).unwrap());
    h.provider.recorded.set_outcome_at(
        id,
        Arc::new(Outcome::Applied),
        OrganizationRevision::new(2),
    );
    h.provider.recorded.finish(0, Ok(Some(id)));
    cx.run_until_parked();
    publish_repair(&h, 2, cx);
    assert!(starts(&h).is_empty());
    assert_eq!(
        h.repair.read_with(cx, |r, _| r.status()),
        PaneRepairStatus::Unresolved
    );
}
