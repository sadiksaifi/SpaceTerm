//! Server-backed Tab Views, driven through replica publications and a recording provider.

use std::cell::RefCell;
use std::rc::Rc;

use gpui::{Entity, TestAppContext, VisualTestContext};
use spaceterm_core::organization::{PaneCreation, SplitAxis as CoreAxis, SplitEdge};

use super::server::testing::{
    TestPanes, TestRuntime, frame, local_session_factory, operation, publication, request_id,
    view_revision,
};
use super::{
    LayoutNode, ServerEditRefused, ServerPaneFactory, ServerTabManagerEvent, SplitKey, TabView,
    TerminalPaneEvent,
};
use crate::domain::server_view_testing::{A, B, Org, Replica, pane, tab, ws};
use crate::domain::{PaneSize, SplitAxis};
use crate::ui::pane_lifecycle::PaneConstruction;
use crate::ui::server_desktop_contracts::{
    ArrangementEdit, Command, DesktopClientError, DesktopRequest, Outcome, PaneAction, PaneId,
    ReplicaStatus, SelectionSource, TabId, ViewEdit,
};

struct Harness {
    runtime: TestRuntime,
    panes: Rc<TestPanes>,
    view: Entity<TabView>,
    events: Rc<RefCell<Vec<ServerTabManagerEvent>>>,
    refused: Rc<RefCell<Vec<DesktopClientError>>>,
}

impl Harness {
    fn events(&self) -> Vec<ServerTabManagerEvent> {
        self.events.borrow().clone()
    }

    fn refused(&self) -> Vec<DesktopClientError> {
        self.refused.borrow().clone()
    }

    fn presented(&self, cx: &mut VisualTestContext) -> Vec<PaneId> {
        self.view.read_with(cx, |view, _| {
            view.presented_server_panes()
                .into_iter()
                .map(|(pane, _)| pane)
                .collect()
        })
    }

    fn focused(&self, cx: &mut VisualTestContext) -> PaneId {
        self.view
            .read_with(cx, |view, _| view.focused_server_pane())
            .expect("a server Tab View has a Focused Pane")
    }

    fn natively_focused(&self, cx: &mut VisualTestContext) -> bool {
        cx.update(|window, cx| self.view.read(cx).focused_terminal_is_focused(window, cx))
    }

    fn emit(&self, pane: PaneId, event: TerminalPaneEvent, cx: &mut VisualTestContext) {
        let terminal = self
            .view
            .read_with(cx, |view, _| view.server_terminal(pane))
            .expect("the Pane is presented");
        terminal.update(cx, |_, cx| cx.emit(event));
        cx.run_until_parked();
    }

    /// Applies `replica`'s current publication of `tab` with `status`.
    fn apply(
        &self,
        replica: &Replica,
        status: ReplicaStatus,
        tab_local: u64,
        cx: &mut VisualTestContext,
    ) -> Result<(), DesktopClientError> {
        self.apply_at(replica, status, tab_local, 0, cx)
    }

    fn receipt(&self, request: usize, revision: usize, cx: &mut VisualTestContext) {
        self.view.update(cx, |view, cx| {
            view.personal_edit_applied(request_id(request), view_revision(revision), cx)
        });
        cx.run_until_parked();
    }

    fn apply_at(
        &self,
        replica: &Replica,
        status: ReplicaStatus,
        tab_local: u64,
        revision: usize,
        cx: &mut VisualTestContext,
    ) -> Result<(), DesktopClientError> {
        let mut frame = frame(replica, status);
        frame.view_revision = view_revision(revision);
        let record = replica.tab_record(tab_local);
        let arrangement = replica.arrangement(1, tab_local);
        let result = self.view.update(cx, |view, cx| {
            view.apply_tab(&frame, &record, &arrangement, cx)
        });
        cx.run_until_parked();
        result
    }
}

/// Workspace 1 with Tab 1, whose Pane 1 is split into Pane 2.
fn two_panes(byte: u8) -> Replica {
    let mut org = Org::new(byte);
    org.add_workspace(1, 1, 1)
        .split(1, 2, 1, CoreAxis::Horizontal, SplitEdge::After);
    Replica::attached(org, 1)
}

fn one_pane(byte: u8) -> Replica {
    let mut org = Org::new(byte);
    org.add_workspace(1, 1, 1);
    Replica::attached(org, 1)
}

fn server_view<'a>(
    replica: &Replica,
    status: ReplicaStatus,
    cx: &'a mut TestAppContext,
) -> (Harness, &'a mut VisualTestContext) {
    cx.update(crate::ui::init)
        .expect("UI initialization should succeed");
    let runtime = TestRuntime::new(cx);
    let panes = Rc::new(TestPanes::default());
    let mut publication = publication(replica);
    publication.status = status;
    let target = TabId::new(replica.org.server(), 1).unwrap();
    let (view, cx) = cx.add_window_view({
        let runtime = runtime.runtime.clone();
        let factory: Rc<dyn ServerPaneFactory> = panes.clone();
        move |window, cx| {
            TabView::new_server_with(&publication, target, runtime, factory, window, cx)
                .expect("every Pane is presented")
        }
    });
    let events = Rc::new(RefCell::new(Vec::new()));
    let refused = Rc::new(RefCell::new(Vec::new()));
    cx.update(|window, cx| {
        let recorded = Rc::clone(&events);
        cx.subscribe(&view, move |_, event: &ServerTabManagerEvent, _| {
            recorded.borrow_mut().push(*event);
        })
        .detach();
        let recorded = Rc::clone(&refused);
        cx.subscribe(&view, move |_, event: &ServerEditRefused, _| {
            recorded.borrow_mut().push(event.0);
        })
        .detach();
        window.activate_window();
        view.update(cx, |view, cx| view.focus(window, cx));
    });
    cx.run_until_parked();
    (
        Harness {
            runtime,
            panes,
            view,
            events,
            refused,
        },
        cx,
    )
}

fn root_split(view: &TabView) -> (SplitKey, f32) {
    match view.panes.layout().node() {
        LayoutNode::Split {
            split_id, ratio, ..
        } => (split_id, ratio),
        LayoutNode::Leaf(_) => panic!("the Tab has a Split"),
    }
}

fn shared_commands(requests: &[DesktopRequest]) -> Vec<DesktopRequest> {
    requests
        .iter()
        .filter(|request| !matches!(request, DesktopRequest::PersonalEdit(_)))
        .cloned()
        .collect()
}

#[gpui::test]
fn desktop_server_tab_view_production_construction_fails_closed(cx: &mut TestAppContext) {
    cx.update(crate::ui::init)
        .expect("UI initialization should succeed");
    let runtime = TestRuntime::new(cx);
    let replica = one_pane(A);
    let frame = frame(&replica, ReplicaStatus::Ready);
    let refusal = Rc::new(RefCell::new(None));
    let recorded = Rc::clone(&refusal);
    let runtime_entity = runtime.runtime.clone();
    let (_view, cx) = cx.add_window_view(move |window, cx| {
        let result = TabView::new_server(
            &frame,
            tab(A, 1),
            runtime_entity,
            PaneConstruction::testing(),
            window,
            cx,
        );
        *recorded.borrow_mut() = result.err();
        TabView::new(
            crate::domain::TabId::new(1),
            local_session_factory(),
            window,
            cx,
        )
    });
    cx.run_until_parked();

    assert_eq!(
        *refusal.borrow(),
        Some(DesktopClientError::UnsupportedCapability)
    );
    assert!(runtime.begun().is_empty());
}

#[gpui::test]
fn desktop_server_apply_replica_never_acquires_or_starts(cx: &mut TestAppContext) {
    let mut replica = two_panes(A);
    let (harness, cx) = server_view(&replica, ReplicaStatus::Ready, cx);
    let focused = harness.focused(cx);
    let entities = harness
        .view
        .read_with(cx, |view, _| view.presented_server_panes());
    assert_eq!(harness.presented(cx), vec![pane(A, 1), pane(A, 2)]);

    // Another client splits Pane 2.
    replica
        .org
        .split(1, 3, 2, CoreAxis::Vertical, SplitEdge::After)
        .publish();
    replica.reconcile_ready(&[]);
    assert_eq!(harness.apply(&replica, ReplicaStatus::Ready, 1, cx), Ok(()));

    assert_eq!(
        harness.presented(cx),
        vec![pane(A, 1), pane(A, 2), pane(A, 3)]
    );
    assert_eq!(harness.panes.created().last(), Some(&pane(A, 3)));
    let survivors = harness
        .view
        .read_with(cx, |view, _| view.presented_server_panes());
    assert_eq!(&survivors[..2], entities.as_slice());
    assert_eq!(harness.focused(cx), focused);
    assert!(harness.natively_focused(cx));
    assert!(harness.runtime.begun().is_empty());
    assert!(harness.events().is_empty());
}

#[gpui::test]
fn desktop_server_focus_restoration_is_not_explicit_selection(cx: &mut TestAppContext) {
    let replica = two_panes(A);
    let (harness, cx) = server_view(&replica, ReplicaStatus::Ready, cx);
    let first = harness.focused(cx);
    let other = if first == pane(A, 1) {
        pane(A, 2)
    } else {
        pane(A, 1)
    };

    // Native focus reaching a Pane moves personal focus only.
    harness.emit(other, TerminalPaneEvent::FocusRequested, cx);
    assert_eq!(harness.focused(cx), other);
    assert_eq!(
        harness.runtime.begun(),
        vec![DesktopRequest::PersonalEdit(ViewEdit::Arrange {
            workspace: ws(A, 1),
            tab: tab(A, 1),
            edit: ArrangementEdit::Focus(other),
        })]
    );

    // Restoring native focus sends nothing.
    cx.update(|window, cx| harness.view.update(cx, |view, cx| view.focus(window, cx)));
    cx.run_until_parked();
    assert!(harness.natively_focused(cx));
    assert_eq!(harness.runtime.begun().len(), 1);
    assert!(harness.events().is_empty());
}

#[gpui::test]
fn desktop_server_pointer_keyboard_and_ax_press_select_explicitly(cx: &mut TestAppContext) {
    let replica = two_panes(A);
    let (harness, cx) = server_view(&replica, ReplicaStatus::Ready, cx);

    harness.emit(
        pane(A, 2),
        TerminalPaneEvent::SelectionRequested(SelectionSource::Pointer),
        cx,
    );
    harness.emit(
        pane(A, 1),
        TerminalPaneEvent::SelectionRequested(SelectionSource::AccessibilityPress),
        cx,
    );
    assert_eq!(harness.focused(cx), pane(A, 1));
    cx.simulate_keystrokes("cmd-]");
    cx.run_until_parked();

    assert_eq!(
        harness.events(),
        vec![
            ServerTabManagerEvent::SelectPane {
                pane: pane(A, 2),
                source: SelectionSource::Pointer,
            },
            ServerTabManagerEvent::SelectPane {
                pane: pane(A, 1),
                source: SelectionSource::AccessibilityPress,
            },
            ServerTabManagerEvent::SelectPane {
                pane: pane(A, 2),
                source: SelectionSource::Command,
            },
        ]
    );
    assert_eq!(harness.focused(cx), pane(A, 2));
    assert!(harness.natively_focused(cx));
    assert!(shared_commands(&harness.runtime.begun()).is_empty());
}

#[gpui::test]
fn desktop_server_pane_actions_forward_with_their_qualified_pane(cx: &mut TestAppContext) {
    let replica = two_panes(A);
    let (harness, cx) = server_view(&replica, ReplicaStatus::Ready, cx);

    harness.emit(
        pane(A, 2),
        TerminalPaneEvent::ActionRequested(PaneAction::StartSession(SelectionSource::Pointer)),
        cx,
    );

    assert_eq!(
        harness.events(),
        vec![ServerTabManagerEvent::PaneAction {
            pane: pane(A, 2),
            action: PaneAction::StartSession(SelectionSource::Pointer),
        }]
    );
    assert!(harness.runtime.begun().is_empty());
}

#[gpui::test]
fn desktop_server_remote_split_while_zoomed_keeps_zoom_and_focus(cx: &mut TestAppContext) {
    let mut replica = two_panes(A);
    let (harness, cx) = server_view(&replica, ReplicaStatus::Ready, cx);
    let focused = harness.focused(cx);
    cx.update(|window, cx| {
        harness
            .view
            .update(cx, |view, cx| view.toggle_zoom(window, cx))
    });
    cx.run_until_parked();
    let zoom = harness
        .view
        .read_with(cx, |view, _| view.server_arrangement().zoom());
    assert_eq!(
        harness.runtime.begun(),
        vec![DesktopRequest::PersonalEdit(ViewEdit::Arrange {
            workspace: ws(A, 1),
            tab: tab(A, 1),
            edit: ArrangementEdit::ToggleZoom,
        })]
    );

    // The provider applies the edit, then another client splits the unfocused Pane.
    replica
        .personal
        .arrange(ws(A, 1), tab(A, 1), ArrangementEdit::ToggleZoom)
        .unwrap();
    replica
        .org
        .split(1, 3, 2, CoreAxis::Vertical, SplitEdge::After)
        .publish();
    replica.reconcile_ready(&[]);
    assert_eq!(harness.apply(&replica, ReplicaStatus::Ready, 1, cx), Ok(()));

    harness.view.read_with(cx, |view, _| {
        assert_eq!(view.server_arrangement().zoom(), zoom);
        assert!(view.server_arrangement().contains_pane(pane(A, 3)));
    });
    assert_eq!(harness.focused(cx), focused);
    assert!(harness.natively_focused(cx));
    assert_eq!(harness.runtime.begun().len(), 1);
    assert!(harness.events().is_empty());
}

/// Splits Pane 1 and delivers the outcome and the publication in the given order.
fn split_with_order(outcome_first: bool, cx: &mut TestAppContext) {
    let mut replica = one_pane(A);
    let (harness, cx) = server_view(&replica, ReplicaStatus::Ready, cx);
    cx.update(|window, cx| {
        harness.view.update(cx, |view, cx| {
            view.split_focused(SplitAxis::Horizontal, window, cx)
        });
    });
    cx.run_until_parked();
    assert_eq!(
        harness.runtime.begun(),
        vec![DesktopRequest::Shared {
            server: replica.org.server(),
            command: Command::CreatePane {
                tab: tab(A, 1),
                source: pane(A, 1),
                axis: CoreAxis::Horizontal,
                edge: SplitEdge::After,
            },
        }]
    );
    let created = operation(replica.org.server(), 1);
    let publish = |replica: &mut Replica, cx: &mut VisualTestContext| {
        replica
            .org
            .split(1, 2, 1, CoreAxis::Horizontal, SplitEdge::After)
            .publish();
        let accepted = replica
            .org
            .accepted(2, 1, CoreAxis::Horizontal, SplitEdge::After);
        replica.reconcile_ready(&[accepted]);
        assert_eq!(harness.apply(replica, ReplicaStatus::Ready, 1, cx), Ok(()));
    };

    if outcome_first {
        harness.runtime.provider.finish(0, Ok(Some(created)));
        cx.run_until_parked();
        harness.runtime.deliver(
            replica.org.server(),
            created,
            Outcome::CreatedPane { pane: pane(A, 2) },
            cx,
        );
        assert_eq!(harness.focused(cx), pane(A, 1));
        publish(&mut replica, cx);
    } else {
        publish(&mut replica, cx);
        assert_eq!(harness.focused(cx), pane(A, 1));
        harness.runtime.provider.finish(0, Ok(Some(created)));
        cx.run_until_parked();
        harness.runtime.deliver(
            replica.org.server(),
            created,
            Outcome::CreatedPane { pane: pane(A, 2) },
            cx,
        );
    }

    assert_eq!(harness.focused(cx), pane(A, 2));
    assert!(harness.natively_focused(cx));
    assert_eq!(
        harness.runtime.begun()[1..],
        [DesktopRequest::PersonalEdit(ViewEdit::Arrange {
            workspace: ws(A, 1),
            tab: tab(A, 1),
            edit: ArrangementEdit::Focus(pane(A, 2)),
        })]
    );
    assert!(harness.events().is_empty());
}

#[gpui::test]
fn desktop_server_split_outcome_before_delta_focuses_the_new_pane_once(cx: &mut TestAppContext) {
    split_with_order(true, cx);
}

#[gpui::test]
fn desktop_server_split_delta_before_outcome_focuses_the_new_pane_once(cx: &mut TestAppContext) {
    split_with_order(false, cx);
}

#[gpui::test]
fn desktop_server_shared_split_is_refused_while_offline(cx: &mut TestAppContext) {
    let replica = one_pane(A);
    let (harness, cx) = server_view(&replica, ReplicaStatus::RetainedOffline, cx);

    cx.update(|window, cx| {
        harness.view.update(cx, |view, cx| {
            view.split_focused(SplitAxis::Vertical, window, cx)
        });
    });
    cx.run_until_parked();

    assert_eq!(harness.refused(), vec![DesktopClientError::Offline]);
    assert!(harness.runtime.begun().is_empty());
    assert_eq!(harness.presented(cx), vec![pane(A, 1)]);
}

#[gpui::test]
fn desktop_server_refused_personal_edit_restores_the_publication(cx: &mut TestAppContext) {
    let replica = two_panes(A);
    let (harness, cx) = server_view(&replica, ReplicaStatus::Ready, cx);
    let published = replica.arrangement(1, 1);

    // The provider admits the edit and fails it later.
    cx.update(|window, cx| {
        harness
            .view
            .update(cx, |view, cx| view.toggle_zoom(window, cx))
    });
    assert_ne!(
        harness
            .view
            .read_with(cx, |view, _| view.server_arrangement().clone()),
        published
    );
    harness
        .runtime
        .provider
        .finish(0, Err(DesktopClientError::StaleContext));
    cx.run_until_parked();
    assert_eq!(
        harness
            .view
            .read_with(cx, |view, _| view.server_arrangement().clone()),
        published
    );

    // The provider refuses the next edit before admitting it.
    harness
        .runtime
        .provider
        .refuse_with(DesktopClientError::Offline);
    cx.update(|window, cx| {
        harness
            .view
            .update(cx, |view, cx| view.toggle_zoom(window, cx))
    });
    cx.run_until_parked();

    assert_eq!(
        harness
            .view
            .read_with(cx, |view, _| view.server_arrangement().clone()),
        published
    );
    assert_eq!(
        harness.refused(),
        vec![
            DesktopClientError::StaleContext,
            DesktopClientError::Offline
        ]
    );
}

#[gpui::test]
fn desktop_server_exit_event_waits_for_authoritative_deletion(cx: &mut TestAppContext) {
    let mut replica = two_panes(A);
    let (harness, cx) = server_view(&replica, ReplicaStatus::Ready, cx);

    harness.emit(pane(A, 2), TerminalPaneEvent::Exited, cx);
    assert_eq!(harness.presented(cx), vec![pane(A, 1), pane(A, 2)]);
    assert!(harness.runtime.begun().is_empty());

    replica.org.delete_pane(2).publish();
    replica.reconcile_ready(&[]);
    assert_eq!(harness.apply(&replica, ReplicaStatus::Ready, 1, cx), Ok(()));

    assert_eq!(harness.presented(cx), vec![pane(A, 1)]);
    assert_eq!(harness.focused(cx), pane(A, 1));
    assert!(harness.runtime.begun().is_empty());
    assert!(harness.events().is_empty());
}

#[gpui::test]
fn desktop_server_failed_session_keeps_its_pane(cx: &mut TestAppContext) {
    use spaceterm_core::organization::{FailureClass, OrganizationSnapshot, SessionPhase};
    let mut replica = two_panes(A);
    let (harness, cx) = server_view(&replica, ReplicaStatus::Ready, cx);
    let entities = harness
        .view
        .read_with(cx, |view, _| view.presented_server_panes());
    replica.org.publish();
    let mut failed = frame(&replica, ReplicaStatus::Ready);
    let mut runtime = failed.organization.runtime().to_vec();
    runtime
        .iter_mut()
        .find(|record| record.pane == pane(A, 2))
        .unwrap()
        .phase = SessionPhase::Failed {
        class: FailureClass::RuntimeFault,
    };
    failed.organization = std::sync::Arc::new(
        OrganizationSnapshot::new(
            failed.organization.epoch(),
            failed.organization.state().clone(),
            runtime,
        )
        .unwrap(),
    );
    let record = replica.tab_record(1);
    let arrangement = replica.arrangement(1, 1);
    harness
        .view
        .update(cx, |view, cx| {
            view.apply_tab(&failed, &record, &arrangement, cx)
        })
        .unwrap();
    cx.run_until_parked();

    assert_eq!(
        harness
            .view
            .read_with(cx, |view, _| view.presented_server_panes()),
        entities
    );
    assert!(harness.runtime.begun().is_empty());
    assert!(harness.events().is_empty());
}

#[gpui::test]
fn desktop_server_deleted_split_source_uses_normalized_creation_topology(cx: &mut TestAppContext) {
    let mut org = Org::new(A);
    org.add_workspace(1, 1, 1)
        .split(1, 2, 1, CoreAxis::Horizontal, SplitEdge::After)
        .split(1, 3, 2, CoreAxis::Vertical, SplitEdge::After);
    let mut replica = Replica::attached(org, 1);
    let (harness, cx) = server_view(&replica, ReplicaStatus::Ready, cx);

    // The server deletes Pane 2 and relinks Pane 3 to Pane 2's own source.
    replica
        .org
        .relink(
            3,
            PaneCreation::SplitOf {
                source: pane(A, 1),
                axis: CoreAxis::Horizontal,
                edge: SplitEdge::After,
            },
        )
        .delete_pane(2)
        .publish();
    replica.reconcile_ready(&[]);
    assert_eq!(harness.apply(&replica, ReplicaStatus::Ready, 1, cx), Ok(()));

    assert_eq!(harness.presented(cx), vec![pane(A, 1), pane(A, 3)]);
    assert_eq!(
        harness
            .view
            .read_with(cx, |view, _| view.server_arrangement().clone()),
        replica.arrangement(1, 1)
    );
    assert!(harness.runtime.begun().is_empty());
}

#[gpui::test]
fn desktop_server_equal_local_ids_on_two_servers_do_not_alias(cx: &mut TestAppContext) {
    let replica = two_panes(A);
    let other = two_panes(B);
    let (harness, cx) = server_view(&replica, ReplicaStatus::Ready, cx);
    assert_eq!(pane(A, 1).local(), pane(B, 1).local());
    assert_ne!(pane(A, 1), pane(B, 1));

    // A publication of another server never applies, even with equal local identities.
    let frame = frame(&other, ReplicaStatus::Ready);
    let record = other.tab_record(1);
    let arrangement = other.arrangement(1, 1);
    let result = harness.view.update(cx, |view, cx| {
        view.apply_tab(&frame, &record, &arrangement, cx)
    });
    assert_eq!(result, Err(DesktopClientError::WrongServer));

    harness.emit(
        pane(A, 1),
        TerminalPaneEvent::SelectionRequested(SelectionSource::Pointer),
        cx,
    );
    assert_eq!(
        harness.events(),
        vec![ServerTabManagerEvent::SelectPane {
            pane: pane(A, 1),
            source: SelectionSource::Pointer,
        }]
    );
    assert_eq!(harness.presented(cx), vec![pane(A, 1), pane(A, 2)]);
    harness.view.read_with(cx, |view, _| {
        assert_eq!(view.server_tab_id(), Some(tab(A, 1)));
        assert_eq!(view.local_tab_id(), None);
    });
}

#[gpui::test]
fn desktop_server_split_resize_changes_only_the_personal_arrangement(cx: &mut TestAppContext) {
    let replica = two_panes(A);
    let (harness, cx) = server_view(&replica, ReplicaStatus::Ready, cx);
    let root = replica.tab_record(1).root_pane;
    let available = PaneSize::new(1200.0, 800.0).unwrap();
    let (split, ratio) = harness.view.read_with(cx, |view, _| root_split(view));

    // A live resize applies here and sends nothing until the interaction ends.
    let live = harness.view.update(cx, |view, cx| {
        view.set_split_ratio(split, available, 0.3, false, cx)
    });
    assert!(live.is_some_and(|accepted| (accepted - ratio).abs() > f32::EPSILON));
    assert!(harness.runtime.begun().is_empty());
    harness
        .view
        .update(cx, |view, cx| view.commit_live_edit(cx));
    cx.run_until_parked();

    let begun = harness.runtime.begun();
    assert!(
        matches!(
            begun.as_slice(),
            [DesktopRequest::PersonalEdit(ViewEdit::Arrange {
                edit: ArrangementEdit::Resize { .. },
                ..
            })]
        ),
        "one personal resize: {begun:?}"
    );
    harness.view.read_with(cx, |view, _| {
        assert_eq!(view.server_root_pane(), Some(root));
        assert_ne!(root_split(view).1, ratio);
    });
    assert!(shared_commands(&begun).is_empty());
}

#[gpui::test]
fn desktop_server_reconcile_during_resize_cancels_the_deleted_split(cx: &mut TestAppContext) {
    let mut replica = two_panes(A);
    let (harness, cx) = server_view(&replica, ReplicaStatus::Ready, cx);
    let available = PaneSize::new(1200.0, 800.0).unwrap();
    let (split, _) = harness.view.read_with(cx, |view, _| root_split(view));
    harness.view.update(cx, |view, cx| {
        view.resizing_split_id = Some(split);
        view.set_split_ratio(split, available, 0.3, false, cx);
    });

    replica.org.delete_pane(2).publish();
    replica.reconcile_ready(&[]);
    assert_eq!(harness.apply(&replica, ReplicaStatus::Ready, 1, cx), Ok(()));
    harness
        .view
        .update(cx, |view, cx| view.commit_live_edit(cx));
    cx.run_until_parked();

    assert_eq!(
        harness.view.read_with(cx, |view, _| view.resizing_split_id),
        None
    );
    assert!(harness.runtime.begun().is_empty());
    assert_eq!(harness.presented(cx), vec![pane(A, 1)]);
}

#[gpui::test]
fn desktop_server_failed_pane_presentation_keeps_its_place_until_a_retry(cx: &mut TestAppContext) {
    let mut replica = one_pane(A);
    let (harness, cx) = server_view(&replica, ReplicaStatus::Ready, cx);
    harness
        .panes
        .fail_with(Some(DesktopClientError::UnsupportedCapability));

    replica
        .org
        .split(1, 2, 1, CoreAxis::Horizontal, SplitEdge::After)
        .publish();
    replica.reconcile_ready(&[]);
    assert_eq!(harness.apply(&replica, ReplicaStatus::Ready, 1, cx), Ok(()));
    assert_eq!(harness.presented(cx), vec![pane(A, 1)]);
    assert!(harness.view.read_with(cx, |view, _| {
        view.server_arrangement().contains_pane(pane(A, 2))
    }));

    harness.panes.fail_with(None);
    assert_eq!(harness.apply(&replica, ReplicaStatus::Ready, 1, cx), Ok(()));
    assert_eq!(harness.presented(cx), vec![pane(A, 1), pane(A, 2)]);
    assert!(harness.runtime.begun().is_empty());
}

#[gpui::test]
fn desktop_server_pending_focus_survives_an_older_personal_publication(cx: &mut TestAppContext) {
    let mut replica = two_panes(A);
    let (harness, cx) = server_view(&replica, ReplicaStatus::Ready, cx);
    cx.simulate_keystrokes("cmd-]");
    cx.run_until_parked();
    assert_eq!(harness.focused(cx), pane(A, 2));
    replica
        .org
        .split(1, 3, 2, CoreAxis::Vertical, SplitEdge::After)
        .publish();
    replica.reconcile_ready(&[]);
    harness
        .apply(&replica, ReplicaStatus::Ready, 1, cx)
        .unwrap();
    assert_eq!(harness.focused(cx), pane(A, 2));
    assert!(harness.natively_focused(cx));
    assert_eq!(harness.runtime.begun().len(), 1);
}

#[gpui::test]
fn desktop_server_refused_focus_restores_native_focus_without_selecting(cx: &mut TestAppContext) {
    let replica = two_panes(A);
    let (harness, cx) = server_view(&replica, ReplicaStatus::Ready, cx);
    cx.simulate_keystrokes("cmd-]");
    cx.run_until_parked();
    assert_eq!(harness.focused(cx), pane(A, 2));
    assert!(harness.natively_focused(cx));
    harness
        .runtime
        .provider
        .finish(0, Err(DesktopClientError::StaleContext));
    cx.run_until_parked();
    assert_eq!(harness.focused(cx), pane(A, 1));
    assert!(harness.natively_focused(cx));
    assert_eq!(harness.events().len(), 1, "rollback is not a selection");
    assert_eq!(harness.runtime.begun().len(), 1);
}

#[gpui::test]
fn desktop_server_refused_arrangement_preserves_other_pending_edits(cx: &mut TestAppContext) {
    let replica = two_panes(A);
    let (harness, cx) = server_view(&replica, ReplicaStatus::Ready, cx);
    cx.simulate_keystrokes("cmd-]");
    cx.update(|window, cx| {
        harness
            .view
            .update(cx, |view, cx| view.toggle_zoom(window, cx))
    });
    cx.run_until_parked();
    harness
        .runtime
        .provider
        .finish(1, Err(DesktopClientError::StaleContext));
    cx.run_until_parked();
    assert_eq!(harness.focused(cx), pane(A, 2));
    assert!(harness.natively_focused(cx));
    assert_eq!(
        harness
            .view
            .read_with(cx, |view, _| view.server_arrangement().zoom()),
        spaceterm_client::view::ZoomState::Restored
    );

    // A synchronous refusal must also keep the earlier admitted focus edit.
    harness
        .runtime
        .provider
        .refuse_with(DesktopClientError::Offline);
    cx.update(|window, cx| {
        harness
            .view
            .update(cx, |view, cx| view.toggle_zoom(window, cx))
    });
    cx.run_until_parked();
    assert_eq!(harness.focused(cx), pane(A, 2));
    assert!(harness.natively_focused(cx));
    assert_eq!(
        harness.refused(),
        vec![
            DesktopClientError::StaleContext,
            DesktopClientError::Offline
        ]
    );
}

#[gpui::test]
fn desktop_server_pending_zoom_is_not_toggled_again_when_publication_reflects_it(
    cx: &mut TestAppContext,
) {
    let mut replica = two_panes(A);
    let (harness, cx) = server_view(&replica, ReplicaStatus::Ready, cx);
    cx.update(|window, cx| {
        harness
            .view
            .update(cx, |view, cx| view.toggle_zoom(window, cx))
    });
    replica
        .personal
        .arrange(ws(A, 1), tab(A, 1), ArrangementEdit::ToggleZoom)
        .unwrap();
    harness
        .apply(&replica, ReplicaStatus::Ready, 1, cx)
        .unwrap();
    assert_eq!(
        harness
            .view
            .read_with(cx, |view, _| view.server_arrangement().zoom()),
        spaceterm_client::view::ZoomState::Zoomed(pane(A, 1))
    );
}

#[gpui::test]
fn desktop_server_missing_focused_pane_takes_native_focus_when_it_materializes(
    cx: &mut TestAppContext,
) {
    let mut replica = one_pane(A);
    let (harness, cx) = server_view(&replica, ReplicaStatus::Ready, cx);
    harness
        .panes
        .fail_with(Some(DesktopClientError::UnsupportedCapability));
    replica
        .org
        .split(1, 2, 1, CoreAxis::Horizontal, SplitEdge::After)
        .publish();
    replica.reconcile_ready(&[]);
    harness
        .apply(&replica, ReplicaStatus::Ready, 1, cx)
        .unwrap();
    replica
        .org
        .set_root_pane(1, 2)
        .relink(2, PaneCreation::Initial)
        .delete_pane(1)
        .publish();
    replica.reconcile_ready(&[]);
    harness
        .apply(&replica, ReplicaStatus::Ready, 1, cx)
        .unwrap();
    assert_eq!(harness.focused(cx), pane(A, 2));
    assert!(harness.presented(cx).is_empty());
    harness.panes.fail_with(None);
    harness
        .apply(&replica, ReplicaStatus::Ready, 1, cx)
        .unwrap();
    assert_eq!(harness.presented(cx), vec![pane(A, 2)]);
    assert!(harness.natively_focused(cx));
    assert!(harness.events().is_empty());
    assert!(harness.runtime.begun().is_empty());
}

#[gpui::test]
fn desktop_server_committed_caption_click_and_ax_press_select_explicitly(cx: &mut TestAppContext) {
    use gpui::{Modifiers, accesskit::Action};
    use spaceterm_ui::a11y_testing::{A11yTree, perform};
    let replica = two_panes(A);
    let (harness, cx) = server_view(&replica, ReplicaStatus::Ready, cx);
    harness.view.update(cx, |view, cx| {
        view.pane_captions
            .get_mut(&super::PaneKey::Server(pane(A, 1)))
            .unwrap()
            .name = "First shell".into();
        view.pane_captions
            .get_mut(&super::PaneKey::Server(pane(A, 2)))
            .unwrap()
            .name = "Second shell".into();
        cx.notify();
    });
    cx.run_until_parked();
    let caption = cx.debug_bounds("pane-caption-2-unfocused").unwrap();
    let point = gpui::point(caption.left() + gpui::px(10.0), caption.center().y);
    cx.simulate_mouse_move(point, None, Modifiers::none());
    cx.simulate_click(point, Modifiers::none());
    cx.run_until_parked();
    assert_eq!(
        harness.events(),
        vec![ServerTabManagerEvent::SelectPane {
            pane: pane(A, 2),
            source: SelectionSource::Pointer
        }]
    );
    let tree = A11yTree::read(cx);
    perform(cx, tree.node("Pane Caption, First shell"), Action::Click);
    cx.run_until_parked();
    assert_eq!(
        harness.events(),
        vec![
            ServerTabManagerEvent::SelectPane {
                pane: pane(A, 2),
                source: SelectionSource::Pointer
            },
            ServerTabManagerEvent::SelectPane {
                pane: pane(A, 1),
                source: SelectionSource::AccessibilityPress
            },
        ]
    );
    assert!(harness.natively_focused(cx));
    assert!(shared_commands(&harness.runtime.begun()).is_empty());
}

#[gpui::test]
fn desktop_server_caption_press_that_becomes_a_drag_is_not_explicit_use(cx: &mut TestAppContext) {
    use gpui::{Modifiers, MouseButton, point, px};
    let replica = two_panes(A);
    let (harness, cx) = server_view(&replica, ReplicaStatus::Ready, cx);
    let caption = cx.debug_bounds("pane-caption-2-unfocused").unwrap();
    let from = point(caption.left() + px(10.0), caption.center().y);
    cx.simulate_mouse_move(from, None, Modifiers::none());
    cx.simulate_mouse_down(from, MouseButton::Left, Modifiers::none());
    assert!(harness.events().is_empty(), "pointer-down is not selection");
    let to = from + point(px(20.0), px(20.0));
    cx.simulate_mouse_move(to, Some(MouseButton::Left), Modifiers::none());
    cx.run_until_parked();
    assert!(
        harness
            .view
            .read_with(cx, |view, _| view.pane_drag.is_some())
    );
    cx.simulate_mouse_up(to, MouseButton::Left, Modifiers::none());
    cx.run_until_parked();
    assert!(harness.events().is_empty());
    assert!(shared_commands(&harness.runtime.begun()).is_empty());
}

#[gpui::test]
fn desktop_server_unavailable_caption_disables_shared_controls_but_keeps_zoom(
    cx: &mut TestAppContext,
) {
    use spaceterm_ui::a11y_testing::{A11yTree, perform};
    let replica = two_panes(A);
    let (harness, cx) = server_view(&replica, ReplicaStatus::Ready, cx);
    for (status, refusal) in [
        (ReplicaStatus::RetainedOffline, DesktopClientError::Offline),
        (
            ReplicaStatus::Resynchronizing,
            DesktopClientError::Reconciling,
        ),
    ] {
        harness.apply(&replica, status, 1, cx).unwrap();
        crate::ui::settle_hover(cx);
        let tree = A11yTree::read(cx);
        for label in ["Split Right", "Split Down", "Close Pane"] {
            let control = tree
                .with_role("Button")
                .into_iter()
                .find(|node| node["aria"]["label"] == label && tree.exposed(node))
                .unwrap();
            assert_eq!(control["aria"]["disabled"], true, "{status:?}: {label}");
            perform(cx, control, gpui::accesskit::Action::Click);
        }
        assert!(harness.runtime.begun().is_empty());
        assert!(harness.events().is_empty());
        let zoom = tree
            .with_role("Button")
            .into_iter()
            .find(|node| node["aria"]["label"] == "Zoom Pane" && tree.exposed(node))
            .unwrap();
        assert_ne!(zoom["aria"]["disabled"], true);
        // A stale callback or Shortcut still reports the typed refusal.
        harness.view.update(cx, |view, cx| {
            view.request_close_pane(super::PaneKey::Server(pane(A, 1)), cx)
        });
        cx.run_until_parked();
        assert_eq!(harness.refused().last(), Some(&refusal));
        assert!(harness.events().is_empty());
    }
}

#[gpui::test]
fn desktop_server_releasing_panes_closes_only_their_presentations(cx: &mut TestAppContext) {
    let mut replica = two_panes(A);
    let (harness, cx) = server_view(&replica, ReplicaStatus::Ready, cx);
    let first = harness
        .view
        .read_with(cx, |view, _| view.server_terminal(pane(A, 1)))
        .unwrap();
    let second = harness
        .view
        .read_with(cx, |view, _| view.server_terminal(pane(A, 2)))
        .unwrap();
    assert!(first.read_with(cx, |terminal, _| terminal.restart_state().0));
    assert!(second.read_with(cx, |terminal, _| terminal.restart_state().0));
    replica.org.delete_pane(2).publish();
    replica.reconcile_ready(&[]);
    harness
        .apply(&replica, ReplicaStatus::Ready, 1, cx)
        .unwrap();
    assert!(
        second.read_with(cx, |terminal, _| terminal.restart_state().0),
        "release must not invoke compatibility Session close"
    );
    harness.view.update(cx, |view, cx| view.close_all(cx));
    cx.run_until_parked();
    assert!(
        first.read_with(cx, |terminal, _| terminal.restart_state().0),
        "window close must not invoke compatibility Session close"
    );
    assert!(harness.runtime.begun().is_empty());
    assert!(harness.events().is_empty());
}

#[gpui::test]
fn desktop_server_two_clients_keep_personal_ratios_focus_zoom_and_surviving_entities(
    cx: &mut TestAppContext,
) {
    use spaceterm_client::view::{
        ArrangementMetrics, PaneNodeRef, PaneSize as ViewSize, ZoomState,
    };
    let mut first = two_panes(A);
    let mut second = first.clone();
    for (replica, ratio, focused) in [
        (&mut first, 0.3, pane(A, 1)),
        (&mut second, 0.7, pane(A, 2)),
    ] {
        let arrangement = replica.arrangement(1, 1);
        let PaneNodeRef::Split { id: split_id, .. } = arrangement.tree().node() else {
            panic!("a split");
        };
        replica
            .personal
            .arrange(
                ws(A, 1),
                tab(A, 1),
                ArrangementEdit::Resize {
                    split: split_id,
                    available: ViewSize::new(1200.0, 800.0).unwrap(),
                    metrics: ArrangementMetrics {
                        minimum_leaf: ViewSize::new(100.0, 100.0).unwrap(),
                        divider: 6.0,
                    },
                    ratio,
                },
            )
            .unwrap();
        replica
            .personal
            .arrange(ws(A, 1), tab(A, 1), ArrangementEdit::Focus(focused))
            .unwrap();
        replica
            .personal
            .arrange(ws(A, 1), tab(A, 1), ArrangementEdit::ToggleZoom)
            .unwrap();
    }
    let (one, mut cx_one) = {
        let (harness, visual) = server_view(&first, ReplicaStatus::Ready, cx);
        (harness, visual.clone())
    };
    let (two, mut cx_two) = {
        let (harness, visual) = server_view(&second, ReplicaStatus::Ready, cx);
        (harness, visual.clone())
    };
    let first_entities = one
        .view
        .read_with(&cx_one, |view, _| view.presented_server_panes());
    let second_entities = two
        .view
        .read_with(&cx_two, |view, _| view.presented_server_panes());
    first
        .org
        .split(1, 3, 2, CoreAxis::Vertical, SplitEdge::After)
        .publish();
    for close in [false, true] {
        if close {
            first.org.delete_pane(3).publish();
        }
        second.org = first.org.clone();
        first.reconcile_ready(&[]);
        second.reconcile_ready(&[]);
        one.apply(&first, ReplicaStatus::Ready, 1, &mut cx_one)
            .unwrap();
        two.apply(&second, ReplicaStatus::Ready, 1, &mut cx_two)
            .unwrap();
        for (harness, visual, ratio, focused, entities) in [
            (&one, &mut cx_one, 0.3, pane(A, 1), &first_entities),
            (&two, &mut cx_two, 0.7, pane(A, 2), &second_entities),
        ] {
            harness.view.read_with(visual, |view, _| {
                assert_eq!(root_split(view).1, ratio);
                assert_eq!(view.focused_server_pane(), Some(focused));
                assert_eq!(view.server_arrangement().zoom(), ZoomState::Zoomed(focused));
                let survivors: Vec<_> = view
                    .presented_server_panes()
                    .into_iter()
                    .filter(|(pane, _)| pane.local() <= 2)
                    .collect();
                assert_eq!(&survivors, entities);
            });
            assert!(harness.runtime.begun().is_empty());
            assert!(harness.events().is_empty());
        }
    }
}

#[gpui::test]
fn desktop_server_materialized_focused_pane_respects_a_native_focus_blocker(
    cx: &mut TestAppContext,
) {
    let mut replica = one_pane(A);
    let (harness, cx) = server_view(&replica, ReplicaStatus::Ready, cx);
    harness
        .panes
        .fail_with(Some(DesktopClientError::UnsupportedCapability));
    replica
        .org
        .split(1, 2, 1, CoreAxis::Horizontal, SplitEdge::After)
        .publish();
    replica.reconcile_ready(&[]);
    harness
        .apply(&replica, ReplicaStatus::Ready, 1, cx)
        .unwrap();
    replica
        .org
        .set_root_pane(1, 2)
        .relink(2, PaneCreation::Initial)
        .delete_pane(1)
        .publish();
    replica.reconcile_ready(&[]);
    harness
        .apply(&replica, ReplicaStatus::Ready, 1, cx)
        .unwrap();
    let blocker = cx.update(|window, cx| {
        let blocker = cx.focus_handle();
        window.focus(&blocker, cx);
        harness.view.update(cx, |view, cx| {
            view.set_focus_branch(
                true,
                Some(crate::ui::terminal_focus::TerminalFocusBlocker::Modal),
                cx,
            );
        });
        blocker
    });
    harness.panes.fail_with(None);
    harness
        .apply(&replica, ReplicaStatus::Ready, 1, cx)
        .unwrap();
    assert_eq!(harness.presented(cx), vec![pane(A, 2)]);
    assert!(cx.update(|window, _| blocker.is_focused(window)));
    assert!(!harness.natively_focused(cx));
    assert!(harness.events().is_empty());
    assert!(harness.runtime.begun().is_empty());
}

#[gpui::test]
fn desktop_server_synchronously_refused_native_focus_edit_restores_the_responder(
    cx: &mut TestAppContext,
) {
    let replica = two_panes(A);
    let (harness, cx) = server_view(&replica, ReplicaStatus::Ready, cx);
    harness
        .runtime
        .provider
        .refuse_with(DesktopClientError::Offline);
    let second = harness
        .view
        .read_with(cx, |view, _| view.server_terminal(pane(A, 2)))
        .unwrap();
    cx.update(|window, cx| second.update(cx, |terminal, cx| terminal.focus(window, cx)));
    // The native callback is a personal edit, not explicit use.
    harness.emit(pane(A, 2), TerminalPaneEvent::FocusRequested, cx);
    assert_eq!(harness.focused(cx), pane(A, 1));
    assert!(harness.natively_focused(cx));
    assert!(harness.events().is_empty());
    assert!(harness.runtime.begun().is_empty());
}

#[gpui::test]
fn desktop_server_replaying_pending_history_navigation_keeps_the_focus_cycle(
    cx: &mut TestAppContext,
) {
    let mut replica = two_panes(A);
    let (harness, cx) = server_view(&replica, ReplicaStatus::Ready, cx);
    cx.simulate_keystrokes("cmd-]");
    cx.run_until_parked();
    let history = harness.view.read_with(cx, |view, _| {
        view.server_arrangement().focus_history().to_vec()
    });
    replica
        .org
        .split(1, 3, 2, CoreAxis::Vertical, SplitEdge::After)
        .publish();
    replica.reconcile_ready(&[]);
    harness
        .apply(&replica, ReplicaStatus::Ready, 1, cx)
        .unwrap();
    assert_eq!(harness.focused(cx), pane(A, 2));
    assert_eq!(
        harness.view.read_with(cx, |view, _| view
            .server_arrangement()
            .focus_history()
            .to_vec()),
        history
    );
}

#[gpui::test]
fn desktop_server_pending_zoom_does_not_zoom_a_deleted_panes_replacement(cx: &mut TestAppContext) {
    let mut replica = two_panes(A);
    replica
        .org
        .split(1, 3, 2, CoreAxis::Vertical, SplitEdge::After)
        .publish();
    replica.reconcile_ready(&[]);
    let (harness, cx) = server_view(&replica, ReplicaStatus::Ready, cx);
    cx.update(|window, cx| {
        harness
            .view
            .update(cx, |view, cx| view.toggle_zoom(window, cx))
    });
    replica
        .org
        .set_root_pane(1, 2)
        .relink(2, PaneCreation::Initial)
        .delete_pane(1)
        .publish();
    replica.reconcile_ready(&[]);
    harness
        .apply(&replica, ReplicaStatus::Ready, 1, cx)
        .unwrap();
    assert_eq!(
        harness
            .view
            .read_with(cx, |view, _| view.server_arrangement().zoom()),
        spaceterm_client::view::ZoomState::Restored
    );
    assert!(harness.natively_focused(cx));
    assert_eq!(harness.runtime.begun().len(), 1);
    assert!(harness.events().is_empty());
}

#[gpui::test]
fn desktop_server_caption_drag_cancels_delayed_split_attention(cx: &mut TestAppContext) {
    use gpui::{Modifiers, MouseButton, point, px};
    let mut replica = two_panes(A);
    let (harness, cx) = server_view(&replica, ReplicaStatus::Ready, cx);
    harness.view.update(cx, |view, cx| {
        view.split_server_pane(pane(A, 1), SplitAxis::Vertical, cx);
    });
    let caption = cx.debug_bounds("pane-caption-2-unfocused").unwrap();
    let from = point(caption.left() + px(10.0), caption.center().y);
    cx.simulate_mouse_move(from, None, Modifiers::none());
    cx.simulate_mouse_down(from, MouseButton::Left, Modifiers::none());
    let to = from + point(px(20.0), px(20.0));
    cx.simulate_mouse_move(to, Some(MouseButton::Left), Modifiers::none());
    cx.run_until_parked();
    assert_eq!(harness.focused(cx), pane(A, 2));
    assert!(
        harness
            .view
            .read_with(cx, |view, _| view.pane_drag.is_some())
    );
    assert!(harness.events().is_empty());

    let created = operation(replica.org.server(), 1);
    harness.runtime.provider.finish(0, Ok(Some(created)));
    cx.run_until_parked();
    harness.runtime.deliver(
        replica.org.server(),
        created,
        Outcome::CreatedPane { pane: pane(A, 3) },
        cx,
    );
    replica
        .org
        .split(1, 3, 1, CoreAxis::Vertical, SplitEdge::After)
        .publish();
    replica.reconcile_ready(&[]);
    harness
        .apply(&replica, ReplicaStatus::Ready, 1, cx)
        .unwrap();
    assert_eq!(
        harness.focused(cx),
        pane(A, 2),
        "post-create attention cannot override a caption drag"
    );
    cx.simulate_mouse_up(to, MouseButton::Left, Modifiers::none());
    cx.run_until_parked();
    assert!(harness.events().is_empty());
    assert_eq!(harness.runtime.begun().len(), 2, "creation and focus only");
}

fn admit_moves_and_resize(
    harness: &Harness,
    cx: &mut VisualTestContext,
) -> spaceterm_client::view::SplitId {
    use spaceterm_client::view::{ArrangementMetrics, PaneEdge, PaneSize as ViewSize};
    let metrics = ArrangementMetrics {
        minimum_leaf: ViewSize::new(100.0, 100.0).unwrap(),
        divider: 6.0,
    };
    for target in [pane(A, 2), pane(A, 3)] {
        assert!(harness.view.update(cx, |view, cx| view.arrange(
            ArrangementEdit::Move {
                pane: pane(A, 1),
                target,
                edge: PaneEdge::Bottom,
                target_size: ViewSize::new(1200.0, 800.0).unwrap(),
                metrics,
            },
            cx
        )));
    }
    let split = harness.view.read_with(cx, |view, _| {
        spaceterm_client::view::SplitId::new(view.server_arrangement().split_high_water()).unwrap()
    });
    assert!(harness.view.update(cx, |view, cx| view.arrange(
        ArrangementEdit::Resize {
            split,
            available: ViewSize::new(1200.0, 800.0).unwrap(),
            metrics,
            ratio: 0.3,
        },
        cx
    )));
    split
}

#[gpui::test]
fn desktop_server_published_moves_and_resize_keep_split_ids_and_ratios(cx: &mut TestAppContext) {
    let mut replica = two_panes(A);
    replica
        .org
        .split(1, 3, 2, CoreAxis::Horizontal, SplitEdge::After)
        .publish();
    replica.reconcile_ready(&[]);
    let (harness, cx) = server_view(&replica, ReplicaStatus::Ready, cx);
    let split = admit_moves_and_resize(&harness, cx);
    let accepted = harness
        .view
        .read_with(cx, |view, _| view.server_arrangement().export());
    let entities = harness
        .view
        .read_with(cx, |view, _| view.presented_server_panes());
    for request in harness.runtime.begun() {
        let DesktopRequest::PersonalEdit(ViewEdit::Arrange {
            workspace,
            tab,
            edit,
        }) = request
        else {
            panic!("an arrangement edit")
        };
        replica.personal.arrange(workspace, tab, edit).unwrap();
    }
    harness
        .apply(&replica, ReplicaStatus::Ready, 1, cx)
        .unwrap();
    assert_eq!(
        harness
            .view
            .read_with(cx, |view, _| view.server_arrangement().export()),
        accepted
    );
    assert_eq!(
        harness
            .view
            .read_with(cx, |view, _| view.server_split_ratio(split)),
        Some(0.3)
    );
    assert_eq!(
        harness
            .view
            .read_with(cx, |view, _| view.presented_server_panes()),
        entities
    );
    assert!(harness.events().is_empty());
    assert_eq!(harness.runtime.begun().len(), 3);
}

#[gpui::test]
fn desktop_server_completed_focus_survives_a_failure_before_its_publication(
    cx: &mut TestAppContext,
) {
    let mut replica = two_panes(A);
    let (harness, cx) = server_view(&replica, ReplicaStatus::Ready, cx);
    harness.emit(pane(A, 2), TerminalPaneEvent::FocusRequested, cx);
    cx.update(|window, cx| {
        harness
            .view
            .update(cx, |view, cx| view.toggle_zoom(window, cx))
    });
    harness.runtime.provider.finish(0, Ok(None));
    cx.run_until_parked();
    harness
        .runtime
        .provider
        .finish(1, Err(DesktopClientError::StaleContext));
    cx.run_until_parked();
    assert_eq!(harness.focused(cx), pane(A, 2));
    assert!(harness.natively_focused(cx));
    assert!(harness.events().is_empty());
    assert_eq!(harness.refused(), vec![DesktopClientError::StaleContext]);
    replica
        .personal
        .arrange(ws(A, 1), tab(A, 1), ArrangementEdit::Focus(pane(A, 2)))
        .unwrap();
    harness.receipt(0, 1, cx);
    harness
        .apply_at(&replica, ReplicaStatus::Ready, 1, 1, cx)
        .unwrap();
    // Only its application receipt permits a later authoritative personal change to win.
    replica
        .personal
        .arrange(ws(A, 1), tab(A, 1), ArrangementEdit::Focus(pane(A, 1)))
        .unwrap();
    harness
        .apply_at(&replica, ReplicaStatus::Ready, 1, 2, cx)
        .unwrap();
    assert_eq!(harness.focused(cx), pane(A, 1));
    assert!(harness.events().is_empty());
}

#[gpui::test]
fn desktop_server_structural_edit_publication_watermark_survives_remote_interleaving(
    cx: &mut TestAppContext,
) {
    use spaceterm_client::view::SplitId;
    let mut replica = two_panes(A);
    replica
        .org
        .split(1, 3, 2, CoreAxis::Horizontal, SplitEdge::After)
        .publish();
    replica.reconcile_ready(&[]);
    let (harness, cx) = server_view(&replica, ReplicaStatus::Ready, cx);
    let split = admit_moves_and_resize(&harness, cx);
    let entities = harness
        .view
        .read_with(cx, |view, _| view.presented_server_panes());
    let mut older = replica.clone();
    for request in harness.runtime.begun() {
        let DesktopRequest::PersonalEdit(ViewEdit::Arrange {
            workspace,
            tab,
            edit,
        }) = request
        else {
            panic!("an arrangement edit")
        };
        replica.personal.arrange(workspace, tab, edit).unwrap();
    }
    replica
        .org
        .split(1, 4, 2, CoreAxis::Vertical, SplitEdge::After)
        .publish();
    older.org = replica.org.clone();
    older.reconcile_ready(&[]);
    harness.apply(&older, ReplicaStatus::Ready, 1, cx).unwrap();
    harness.view.read_with(cx, |view, _| {
        let rebased = SplitId::new(view.server_arrangement().split_high_water()).unwrap();
        assert_eq!(
            view.server_split_ratio(rebased),
            Some(0.3),
            "older publications must retain the admitted resize"
        );
    });
    replica.reconcile_ready(&[]);
    let displayed = harness.view.read_with(cx, |view, _| {
        SplitId::new(view.server_arrangement().split_high_water()).unwrap()
    });
    assert_ne!(displayed, split, "the older frame rebases the moved branch");
    assert!(harness.view.update(cx, |view, cx| view.arrange(
        ArrangementEdit::Resize {
            split: displayed,
            available: spaceterm_client::view::PaneSize::new(1200.0, 800.0).unwrap(),
            metrics: spaceterm_client::view::ArrangementMetrics {
                minimum_leaf: spaceterm_client::view::PaneSize::new(100.0, 100.0).unwrap(),
                divider: 6.0,
            },
            ratio: 0.4,
        },
        cx
    )));
    let DesktopRequest::PersonalEdit(ViewEdit::Arrange {
        workspace,
        tab,
        edit,
    }) = harness.runtime.begun()[3]
    else {
        panic!("the newly admitted resize")
    };
    replica.personal.arrange(workspace, tab, edit).unwrap();
    let provider_branch_ratio = split_facts(replica.arrangement(1, 1).tree(), split)
        .unwrap()
        .1;
    assert_eq!(
        provider_branch_ratio, 0.4,
        "resize the branch the person chose"
    );
    harness
        .apply(&replica, ReplicaStatus::Ready, 1, cx)
        .unwrap();
    assert_eq!(
        harness
            .view
            .read_with(cx, |view, _| view.server_arrangement().export()),
        replica.arrangement(1, 1).export()
    );
    assert_eq!(
        harness
            .view
            .read_with(cx, |view, _| view.server_split_ratio(split)),
        Some(0.4)
    );
    let survivors: Vec<_> = harness
        .view
        .read_with(cx, |view, _| view.presented_server_panes())
        .into_iter()
        .filter(|(pane, _)| pane.local() <= 3)
        .collect();
    assert_eq!(survivors, entities);
    assert_eq!(harness.runtime.begun().len(), 4);
    assert!(harness.events().is_empty());
}

#[gpui::test]
fn desktop_server_completed_unpublished_arrangements_are_bounded(cx: &mut TestAppContext) {
    let replica = two_panes(A);
    let (harness, cx) = server_view(&replica, ReplicaStatus::Ready, cx);
    for index in 0..crate::ui::server_client_runtime::MAX_PENDING_UI_REQUESTS {
        assert!(
            harness
                .view
                .update(cx, |view, cx| view.arrange(ArrangementEdit::ToggleZoom, cx))
        );
        harness.runtime.provider.finish(index, Ok(None));
        cx.run_until_parked();
    }
    let accepted = harness
        .view
        .read_with(cx, |view, _| view.server_arrangement().export());
    assert!(
        !harness
            .view
            .update(cx, |view, cx| view.arrange(ArrangementEdit::ToggleZoom, cx))
    );
    cx.run_until_parked();
    assert_eq!(harness.runtime.begun().len(), 128);
    assert_eq!(harness.refused(), vec![DesktopClientError::AdmissionFull]);
    assert_eq!(
        harness
            .view
            .read_with(cx, |view, _| view.server_arrangement().export()),
        accepted
    );
    harness
        .apply(&replica, ReplicaStatus::Ready, 1, cx)
        .unwrap();
    assert!(
        !harness
            .view
            .update(cx, |view, cx| view.arrange(ArrangementEdit::ToggleZoom, cx)),
        "an unchanged frame cannot reclaim retained results"
    );
    for request in 0..128 {
        harness.receipt(request, 1, cx);
    }
    harness
        .apply_at(&replica, ReplicaStatus::Ready, 1, 1, cx)
        .unwrap();
    assert!(
        harness
            .view
            .update(cx, |view, cx| view.arrange(ArrangementEdit::ToggleZoom, cx))
    );
    assert_eq!(harness.runtime.begun().len(), 129);
    assert!(harness.events().is_empty());
}

#[gpui::test]
fn desktop_server_opposite_zoom_toggles_do_not_acknowledge_an_unchanged_publication(
    cx: &mut TestAppContext,
) {
    use spaceterm_client::view::ZoomState;
    let replica = two_panes(A);
    let (harness, cx) = server_view(&replica, ReplicaStatus::Ready, cx);
    for _ in 0..2 {
        assert!(harness.view.update(cx, |view, cx| {
            view.arrange(ArrangementEdit::ToggleZoom, cx)
        }));
    }
    assert_eq!(
        harness
            .view
            .read_with(cx, |view, _| view.server_arrangement().zoom()),
        ZoomState::Restored
    );
    harness
        .apply(&replica, ReplicaStatus::Ready, 1, cx)
        .unwrap();
    harness.runtime.provider.finish(0, Ok(None));
    cx.run_until_parked();
    harness
        .runtime
        .provider
        .finish(1, Err(DesktopClientError::StaleContext));
    cx.run_until_parked();
    assert_eq!(
        harness
            .view
            .read_with(cx, |view, _| view.server_arrangement().zoom()),
        ZoomState::Zoomed(pane(A, 1))
    );
    assert_eq!(harness.refused(), vec![DesktopClientError::StaleContext]);
    assert!(harness.events().is_empty());
}

fn split_facts(
    tree: spaceterm_client::view::PaneTreeRef<'_>,
    split: spaceterm_client::view::SplitId,
) -> Option<(CoreAxis, f32)> {
    match tree.node() {
        spaceterm_client::view::PaneNodeRef::Pane(_) => None,
        spaceterm_client::view::PaneNodeRef::Split {
            id,
            axis,
            ratio,
            first,
            second,
        } => {
            if id == split {
                Some((axis, ratio))
            } else {
                split_facts(first, split).or_else(|| split_facts(second, split))
            }
        }
    }
}

#[gpui::test]
fn desktop_server_rebased_split_axis_and_remote_branch_resize_target_the_provider(
    cx: &mut TestAppContext,
) {
    use spaceterm_client::view::{ArrangementMetrics, PaneSize as ViewSize, SplitId};
    let mut replica = two_panes(A);
    replica
        .org
        .split(1, 3, 2, CoreAxis::Horizontal, SplitEdge::After)
        .publish();
    replica.reconcile_ready(&[]);
    let (harness, cx) = server_view(&replica, ReplicaStatus::Ready, cx);
    let moved = admit_moves_and_resize(&harness, cx);
    let mut older = replica.clone();
    for request in harness.runtime.begun() {
        let DesktopRequest::PersonalEdit(ViewEdit::Arrange {
            workspace,
            tab,
            edit,
        }) = request
        else {
            panic!("an arrangement edit")
        };
        replica.personal.arrange(workspace, tab, edit).unwrap();
    }
    replica
        .org
        .split(1, 4, 2, CoreAxis::Vertical, SplitEdge::After)
        .publish();
    older.org = replica.org.clone();
    older.reconcile_ready(&[]);
    replica.reconcile_ready(&[]);
    let remote = SplitId::new(replica.arrangement(1, 1).split_high_water()).unwrap();
    harness.apply(&older, ReplicaStatus::Ready, 1, cx).unwrap();
    let displayed_moved = harness.view.read_with(cx, |view, _| {
        SplitId::new(view.server_arrangement().split_high_water()).unwrap()
    });
    let displayed_remote = SplitId::new(older.arrangement(1, 1).split_high_water()).unwrap();
    let metrics = ArrangementMetrics {
        minimum_leaf: ViewSize::new(100.0, 100.0).unwrap(),
        divider: 6.0,
    };
    for edit in [
        ArrangementEdit::SetSplitAxis {
            split: displayed_moved,
            axis: CoreAxis::Horizontal,
            available: ViewSize::new(1200.0, 800.0).unwrap(),
            metrics,
        },
        ArrangementEdit::Resize {
            split: displayed_remote,
            available: ViewSize::new(1200.0, 800.0).unwrap(),
            metrics,
            ratio: 0.65,
        },
    ] {
        assert!(harness.view.update(cx, |view, cx| view.arrange(edit, cx)));
        let DesktopRequest::PersonalEdit(ViewEdit::Arrange {
            workspace,
            tab,
            edit,
        }) = *harness.runtime.begun().last().unwrap()
        else {
            panic!("a newly admitted edit")
        };
        replica.personal.arrange(workspace, tab, edit).unwrap();
    }
    assert_eq!(
        split_facts(replica.arrangement(1, 1).tree(), moved)
            .unwrap()
            .0,
        CoreAxis::Horizontal
    );
    assert_eq!(
        split_facts(replica.arrangement(1, 1).tree(), remote)
            .unwrap()
            .1,
        0.65
    );
    harness
        .apply(&replica, ReplicaStatus::Ready, 1, cx)
        .unwrap();
    assert_eq!(
        harness
            .view
            .read_with(cx, |view, _| view.server_arrangement().export()),
        replica.arrangement(1, 1).export()
    );
    assert!(harness.events().is_empty());
}

#[gpui::test]
fn desktop_server_live_divider_rebases_its_binding_before_commit(cx: &mut TestAppContext) {
    use spaceterm_client::view::SplitId;
    let mut replica = two_panes(A);
    replica
        .org
        .split(1, 3, 2, CoreAxis::Horizontal, SplitEdge::After)
        .publish();
    replica.reconcile_ready(&[]);
    let (harness, cx) = server_view(&replica, ReplicaStatus::Ready, cx);
    let split = admit_moves_and_resize(&harness, cx);
    let mut older = replica.clone();
    for request in harness.runtime.begun() {
        let DesktopRequest::PersonalEdit(ViewEdit::Arrange {
            workspace,
            tab,
            edit,
        }) = request
        else {
            panic!("an arrangement edit")
        };
        replica.personal.arrange(workspace, tab, edit).unwrap();
    }
    harness.view.update(cx, |view, cx| {
        view.resizing_split_id = Some(SplitKey::Server(split));
        view.set_split_ratio(
            SplitKey::Server(split),
            PaneSize::new(1200.0, 800.0).unwrap(),
            0.4,
            false,
            cx,
        );
    });
    replica
        .org
        .split(1, 4, 2, CoreAxis::Vertical, SplitEdge::After)
        .publish();
    older.org = replica.org.clone();
    older.reconcile_ready(&[]);
    replica.reconcile_ready(&[]);
    harness.apply(&older, ReplicaStatus::Ready, 1, cx).unwrap();
    let displayed = harness.view.read_with(cx, |view, _| {
        SplitId::new(view.server_arrangement().split_high_water()).unwrap()
    });
    assert_eq!(
        harness.view.read_with(cx, |view, _| view.resizing_split_id),
        Some(SplitKey::Server(displayed))
    );
    harness.view.update(cx, |view, cx| {
        view.set_split_ratio(
            SplitKey::Server(displayed),
            PaneSize::new(1200.0, 800.0).unwrap(),
            0.45,
            false,
            cx,
        );
        view.commit_live_edit(cx);
    });
    let DesktopRequest::PersonalEdit(ViewEdit::Arrange {
        workspace,
        tab,
        edit,
    }) = harness.runtime.begun()[3]
    else {
        panic!("the committed divider edit")
    };
    replica.personal.arrange(workspace, tab, edit).unwrap();
    assert_eq!(
        split_facts(replica.arrangement(1, 1).tree(), split)
            .unwrap()
            .1,
        0.45
    );
    harness
        .apply(&replica, ReplicaStatus::Ready, 1, cx)
        .unwrap();
    assert_eq!(
        harness.view.read_with(cx, |view, _| view.resizing_split_id),
        Some(SplitKey::Server(split))
    );
    assert_eq!(
        harness
            .view
            .read_with(cx, |view, _| view.server_arrangement().export()),
        replica.arrangement(1, 1).export()
    );
    assert!(harness.events().is_empty());
}

#[gpui::test]
fn desktop_server_zoom_prefix_receipt_and_frame_can_precede_completion(cx: &mut TestAppContext) {
    use spaceterm_client::view::ZoomState;
    let mut replica = two_panes(A);
    let original = replica.clone();
    let (harness, cx) = server_view(&replica, ReplicaStatus::Ready, cx);
    for _ in 0..2 {
        assert!(
            harness
                .view
                .update(cx, |view, cx| view.arrange(ArrangementEdit::ToggleZoom, cx))
        );
    }
    harness
        .apply(&original, ReplicaStatus::Ready, 1, cx)
        .unwrap();
    replica
        .personal
        .arrange(ws(A, 1), tab(A, 1), ArrangementEdit::ToggleZoom)
        .unwrap();
    harness
        .apply_at(&replica, ReplicaStatus::Ready, 1, 1, cx)
        .unwrap();
    assert_eq!(
        harness
            .view
            .read_with(cx, |view, _| view.server_arrangement().zoom()),
        ZoomState::Restored,
        "the second toggle still overlays the successful prefix"
    );
    harness.receipt(0, 1, cx);
    harness
        .runtime
        .provider
        .finish(1, Err(DesktopClientError::StaleContext));
    cx.run_until_parked();
    assert_eq!(
        harness
            .view
            .read_with(cx, |view, _| view.server_arrangement().zoom()),
        ZoomState::Zoomed(pane(A, 1))
    );
    harness.runtime.provider.finish(0, Ok(None));
    cx.run_until_parked();
    // A later authoritative change now wins because the successful edit has its receipted frame.
    replica
        .personal
        .arrange(ws(A, 1), tab(A, 1), ArrangementEdit::ToggleZoom)
        .unwrap();
    harness
        .apply_at(&replica, ReplicaStatus::Ready, 1, 2, cx)
        .unwrap();
    assert_eq!(
        harness
            .view
            .read_with(cx, |view, _| view.server_arrangement().zoom()),
        ZoomState::Restored
    );
    assert_eq!(
        harness.apply(&original, ReplicaStatus::Ready, 1, cx),
        Err(DesktopClientError::StaleContext)
    );
    assert!(harness.events().is_empty());
}
