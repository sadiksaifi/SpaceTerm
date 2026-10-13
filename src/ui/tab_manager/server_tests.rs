//! Server-backed Tab Managers, driven through replica publications and a recording provider.

use std::cell::RefCell;
use std::rc::Rc;

use gpui::{
    Entity, Modifiers, MouseButton, Pixels, Point, TestAppContext, VisualTestContext, point, px,
};
use spaceterm_core::operations::TabCreationSource;
use spaceterm_core::organization::{SplitAxis as CoreAxis, SplitEdge};

use super::{ServerEditRefused, ServerTabManagerEvent, TabKey, TabManager};
use crate::domain::server_view_testing::{A, B, Org, Replica, pane, tab, worktree, ws};
use crate::platform::window_movement::RecordingOperatingSystemWindowDragPlatform;
use crate::ui::pane_lifecycle::PaneConstruction;
use crate::ui::server_desktop_contracts::{
    Command, DesktopClientError, DesktopRequest, Outcome, PaneId, ReplicaStatus, SelectionSource,
    TabId, TabStep as ViewTabStep, ViewEdit, WorkspaceId,
};
use crate::ui::tab_view::ServerPaneFactory;
use crate::ui::tab_view::server_testing::{
    TestPanes, TestRuntime, frame, local_session_factory, operation, publication, request_id,
    view_revision,
};
use crate::ui::{ActivateTab1, CreateTab, MoveTabRight, NextTab, PreviousTab};

struct Harness {
    runtime: TestRuntime,
    panes: Rc<TestPanes>,
    manager: Entity<TabManager>,
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

    fn order(&self, cx: &mut VisualTestContext) -> Vec<TabId> {
        self.manager
            .read_with(cx, |manager, _| manager.server_tab_order())
    }

    fn visible(&self, cx: &mut VisualTestContext) -> Vec<TabId> {
        self.manager
            .read_with(cx, |manager, _| manager.visible_server_tabs())
    }

    fn active(&self, cx: &mut VisualTestContext) -> Option<TabId> {
        self.manager
            .read_with(cx, |manager, _| manager.active_server_tab())
    }

    fn natively_focused(&self, cx: &mut VisualTestContext) -> bool {
        cx.update(|window, cx| {
            self.manager
                .read(cx)
                .focused_terminal_is_focused(window, cx)
        })
    }

    fn presented_panes(&self, tab: TabId, cx: &mut VisualTestContext) -> Vec<PaneId> {
        let view = self
            .manager
            .read_with(cx, |manager, _| manager.server_tab(tab))
            .expect("the Tab is presented");
        view.read_with(cx, |view, _| {
            view.presented_server_panes()
                .into_iter()
                .map(|(pane, _)| pane)
                .collect()
        })
    }

    /// Applies `replica`'s current publication of Workspace 1.
    fn apply(
        &self,
        replica: &Replica,
        cx: &mut VisualTestContext,
    ) -> Result<(), DesktopClientError> {
        self.apply_at(replica, 0, cx)
    }

    fn receipt(&self, request: usize, revision: usize, cx: &mut VisualTestContext) {
        self.manager.update(cx, |manager, cx| {
            manager.personal_edit_applied(request_id(request), view_revision(revision), cx)
        });
        cx.run_until_parked();
    }

    fn apply_at(
        &self,
        replica: &Replica,
        revision: usize,
        cx: &mut VisualTestContext,
    ) -> Result<(), DesktopClientError> {
        let mut frame = frame(replica, ReplicaStatus::Ready);
        frame.view_revision = view_revision(revision);
        let record = replica.workspace_record(1);
        let result = self.manager.update(cx, |manager, cx| {
            manager.apply_workspace(&frame, &record, replica.view(1), cx)
        });
        cx.run_until_parked();
        result
    }
}

/// Workspace 1 with Root Tab 1 and Tabs 2 and 3, each with one Pane of the same number.
fn three_tabs(byte: u8) -> Replica {
    let mut org = Org::new(byte);
    org.add_workspace(1, 1, 1)
        .add_tab(1, 2, 2, None)
        .add_tab(1, 3, 3, None);
    Replica::attached(org, 1)
}

fn server_manager<'a>(
    replica: &Replica,
    cx: &'a mut TestAppContext,
) -> (Harness, &'a mut VisualTestContext) {
    cx.update(crate::ui::init)
        .expect("UI initialization should succeed");
    let runtime = TestRuntime::new(cx);
    let panes = Rc::new(TestPanes::default());
    let publication = publication(replica);
    let workspace = replica.workspace_id(1);
    let (manager, cx) = cx.add_window_view({
        let runtime = runtime.runtime.clone();
        let factory: Rc<dyn ServerPaneFactory> = panes.clone();
        move |window, cx| {
            TabManager::new_server_with(
                &publication,
                workspace,
                runtime,
                factory,
                PaneConstruction::testing(),
                Rc::new(RecordingOperatingSystemWindowDragPlatform::default()),
                window,
                cx,
            )
            .expect("every Tab is presented")
        }
    });
    let events = Rc::new(RefCell::new(Vec::new()));
    let refused = Rc::new(RefCell::new(Vec::new()));
    cx.update(|window, cx| {
        let recorded = Rc::clone(&events);
        cx.subscribe(&manager, move |_, event: &ServerTabManagerEvent, _| {
            recorded.borrow_mut().push(*event);
        })
        .detach();
        let recorded = Rc::clone(&refused);
        cx.subscribe(&manager, move |_, event: &ServerEditRefused, _| {
            recorded.borrow_mut().push(event.0);
        })
        .detach();
        window.activate_window();
        manager.update(cx, |manager, cx| manager.focus(window, cx));
    });
    cx.run_until_parked();
    (
        Harness {
            runtime,
            panes,
            manager,
            events,
            refused,
        },
        cx,
    )
}

fn tab_item(tab: TabId, cx: &mut VisualTestContext) -> gpui::Bounds<Pixels> {
    let local = tab.local();
    cx.debug_bounds(Box::leak(
        format!("tab-item-{local}-active").into_boxed_str(),
    ))
    .or_else(|| {
        cx.debug_bounds(Box::leak(
            format!("tab-item-{local}-inactive").into_boxed_str(),
        ))
    })
    .unwrap_or_else(|| panic!("Tab {local} was not rendered"))
}

fn click_at(position: Point<Pixels>, cx: &mut VisualTestContext) {
    cx.simulate_mouse_move(position, None, Modifiers::none());
    cx.simulate_click(position, Modifiers::none());
    cx.run_until_parked();
}

fn begin_drag(from: Point<Pixels>, path: &[Point<Pixels>], cx: &mut VisualTestContext) {
    cx.simulate_mouse_move(from, None, Modifiers::none());
    cx.simulate_mouse_down(from, MouseButton::Left, Modifiers::none());
    for position in path {
        cx.simulate_mouse_move(*position, Some(MouseButton::Left), Modifiers::none());
        cx.run_until_parked();
    }
}

fn activation(tab: TabId) -> DesktopRequest {
    DesktopRequest::PersonalEdit(ViewEdit::ActivateTab {
        workspace: tab_workspace(tab),
        tab,
    })
}

fn tab_workspace(tab: TabId) -> WorkspaceId {
    WorkspaceId::new(tab.server(), 1).unwrap()
}

fn shared_commands(requests: &[DesktopRequest]) -> Vec<DesktopRequest> {
    requests
        .iter()
        .filter(|request| !matches!(request, DesktopRequest::PersonalEdit(_)))
        .cloned()
        .collect()
}

#[gpui::test]
fn desktop_server_tab_manager_production_construction_fails_closed(cx: &mut TestAppContext) {
    cx.update(crate::ui::init)
        .expect("UI initialization should succeed");
    let runtime = TestRuntime::new(cx);
    let replica = three_tabs(A);
    let frame = frame(&replica, ReplicaStatus::Ready);
    let refusal = Rc::new(RefCell::new(None));
    let recorded = Rc::clone(&refusal);
    let runtime_entity = runtime.runtime.clone();
    let (_manager, cx) = cx.add_window_view(move |window, cx| {
        let result = TabManager::new_server(
            &frame,
            ws(A, 1),
            runtime_entity,
            PaneConstruction::testing(),
            Rc::new(RecordingOperatingSystemWindowDragPlatform::default()),
            window,
            cx,
        );
        *recorded.borrow_mut() = result.err();
        TabManager::new(local_session_factory(), window, cx)
    });
    cx.run_until_parked();

    assert_eq!(
        *refusal.borrow(),
        Some(DesktopClientError::UnsupportedCapability)
    );
    assert!(runtime.begun().is_empty());
}

#[gpui::test]
fn desktop_server_apply_workspace_never_opens_or_selects(cx: &mut TestAppContext) {
    let mut replica = three_tabs(A);
    let (harness, cx) = server_manager(&replica, cx);
    assert_eq!(harness.order(cx), vec![tab(A, 1), tab(A, 2), tab(A, 3)]);
    assert_eq!(harness.active(cx), Some(tab(A, 1)));

    // Another client appends a Tab. It is presented without becoming active.
    replica.org.add_tab(1, 4, 4, None).publish();
    replica.reconcile_ready(&[]);
    assert_eq!(harness.apply(&replica, cx), Ok(()));
    assert_eq!(
        harness.order(cx),
        vec![tab(A, 1), tab(A, 2), tab(A, 3), tab(A, 4)]
    );
    assert_eq!(harness.active(cx), Some(tab(A, 1)));
    assert!(harness.natively_focused(cx));

    // Another client closes the Active Tab. A neighbour becomes active and takes native focus,
    // which restores focus without opening the Tab.
    replica.org.set_root_tab(1, 2).delete_tab(1).publish();
    replica.reconcile_ready(&[]);
    assert_eq!(harness.apply(&replica, cx), Ok(()));
    let active = harness.active(cx).expect("a neighbour becomes active");
    assert_ne!(active, tab(A, 1));
    assert_eq!(
        harness
            .manager
            .read_with(cx, |manager, _| manager.server_root_tab()),
        Some(tab(A, 2))
    );
    assert!(harness.natively_focused(cx));
    assert!(harness.runtime.begun().is_empty());
    assert!(harness.events().is_empty());
    assert!(harness.panes.created().contains(&pane(A, 4)));
}

#[gpui::test]
fn desktop_server_restore_focus_is_not_explicit_selection(cx: &mut TestAppContext) {
    let replica = three_tabs(A);
    let (harness, cx) = server_manager(&replica, cx);

    cx.update(|window, cx| window.blur(cx));
    assert!(!harness.natively_focused(cx));
    cx.update(|window, cx| {
        harness
            .manager
            .update(cx, |manager, cx| manager.restore_focus(window, cx));
    });
    cx.run_until_parked();

    assert!(harness.natively_focused(cx));
    assert!(harness.runtime.begun().is_empty());
    assert!(harness.events().is_empty());
}

#[gpui::test]
fn desktop_server_pointer_keyboard_and_ax_press_open_tabs_explicitly(cx: &mut TestAppContext) {
    let replica = three_tabs(A);
    let (harness, cx) = server_manager(&replica, cx);

    let second = tab_item(tab(A, 2), cx).center();
    click_at(second, cx);
    assert_eq!(harness.active(cx), Some(tab(A, 2)));
    assert!(harness.natively_focused(cx));
    cx.update(|window, cx| {
        harness.manager.update(cx, |manager, cx| {
            manager.press_server_tab_for_accessibility(tab(A, 3), window, cx);
        });
    });
    cx.run_until_parked();
    cx.dispatch_action(ActivateTab1);
    cx.run_until_parked();
    cx.dispatch_action(PreviousTab);
    cx.run_until_parked();

    assert_eq!(
        harness.events(),
        vec![
            ServerTabManagerEvent::OpenTab {
                tab: tab(A, 2),
                source: SelectionSource::Pointer,
            },
            ServerTabManagerEvent::OpenTab {
                tab: tab(A, 3),
                source: SelectionSource::AccessibilityPress,
            },
            ServerTabManagerEvent::OpenTab {
                tab: tab(A, 1),
                source: SelectionSource::Command,
            },
            ServerTabManagerEvent::OpenTab {
                tab: tab(A, 3),
                source: SelectionSource::Command,
            },
        ]
    );
    assert_eq!(
        harness.runtime.begun(),
        vec![
            activation(tab(A, 2)),
            activation(tab(A, 3)),
            activation(tab(A, 1)),
            activation(tab(A, 3)),
        ]
    );
    assert_eq!(harness.active(cx), Some(tab(A, 3)));
    assert!(harness.natively_focused(cx));
}

#[gpui::test]
fn desktop_server_tab_open_names_only_the_tab_not_each_pane(cx: &mut TestAppContext) {
    let mut org = Org::new(A);
    org.add_workspace(1, 1, 1).add_tab(1, 2, 2, None).split(
        2,
        3,
        2,
        CoreAxis::Horizontal,
        SplitEdge::After,
    );
    let replica = Replica::attached(org, 1);
    let (harness, cx) = server_manager(&replica, cx);

    let second = tab_item(tab(A, 2), cx).center();
    click_at(second, cx);

    // The Workspace Manager opens the Tab, which acquires its Focused Pane only.
    assert_eq!(
        harness.events(),
        vec![ServerTabManagerEvent::OpenTab {
            tab: tab(A, 2),
            source: SelectionSource::Pointer,
        }]
    );
    assert_eq!(harness.runtime.begun(), vec![activation(tab(A, 2))]);
    assert_eq!(
        harness.presented_panes(tab(A, 2), cx),
        vec![pane(A, 2), pane(A, 3)]
    );
}

#[gpui::test]
fn desktop_server_personal_reorder_leaves_roots_unchanged(cx: &mut TestAppContext) {
    let mut replica = three_tabs(A);
    let (harness, cx) = server_manager(&replica, cx);
    let root = replica.workspace_record(1).root_tab;

    let third = tab_item(tab(A, 3), cx);
    let first = tab_item(tab(A, 1), cx);
    let release = point(first.left() + px(2.0), first.center().y);
    begin_drag(
        third.center(),
        &[third.center() - point(px(8.0), px(0.0)), release],
        cx,
    );
    cx.simulate_mouse_up(release, MouseButton::Left, Modifiers::none());
    cx.run_until_parked();

    assert_eq!(harness.order(cx), vec![tab(A, 3), tab(A, 1), tab(A, 2)]);
    assert_eq!(
        harness.runtime.begun(),
        vec![DesktopRequest::PersonalEdit(ViewEdit::MoveTab {
            workspace: ws(A, 1),
            tab: tab(A, 3),
            position: 0,
        })]
    );
    assert!(harness.events().is_empty());

    // The provider applies the edit. The server's Root Tab never changes.
    let snapshot = replica.org.snapshot();
    replica
        .personal
        .move_tab(ws(A, 1), tab(A, 3), 0, &snapshot)
        .unwrap();
    assert_eq!(harness.apply(&replica, cx), Ok(()));
    assert_eq!(harness.order(cx), vec![tab(A, 3), tab(A, 1), tab(A, 2)]);
    assert_eq!(replica.workspace_record(1).root_tab, root);
    assert_eq!(
        harness
            .manager
            .read_with(cx, |manager, _| manager.server_root_tab()),
        Some(root)
    );
    assert!(shared_commands(&harness.runtime.begun()).is_empty());
}

#[gpui::test]
fn desktop_server_tab_strip_renders_personal_order(cx: &mut TestAppContext) {
    let mut replica = three_tabs(A);
    let snapshot = replica.org.snapshot();
    replica
        .personal
        .move_tab(ws(A, 1), tab(A, 2), 2, &snapshot)
        .unwrap();
    let (harness, cx) = server_manager(&replica, cx);

    let lefts: Vec<_> = harness
        .visible(cx)
        .into_iter()
        .map(|tab| tab_item(tab, cx).left())
        .collect();

    assert_eq!(harness.visible(cx), vec![tab(A, 1), tab(A, 3), tab(A, 2)]);
    assert!(lefts.windows(2).all(|pair| pair[0] < pair[1]), "{lefts:?}");
}

#[gpui::test]
fn desktop_server_move_tab_commands_change_only_personal_order(cx: &mut TestAppContext) {
    let replica = three_tabs(A);
    let (harness, cx) = server_manager(&replica, cx);
    let root = replica.workspace_record(1).root_tab;

    cx.dispatch_action(MoveTabRight);
    cx.run_until_parked();

    assert_eq!(harness.order(cx), vec![tab(A, 2), tab(A, 1), tab(A, 3)]);
    assert_eq!(harness.active(cx), Some(tab(A, 1)));
    assert_eq!(
        harness.runtime.begun(),
        vec![DesktopRequest::PersonalEdit(ViewEdit::StepTab {
            workspace: ws(A, 1),
            tab: tab(A, 1),
            step: ViewTabStep::Next,
        })]
    );
    assert_eq!(
        harness
            .manager
            .read_with(cx, |manager, _| manager.server_root_tab()),
        Some(root)
    );
    assert!(harness.events().is_empty());
}

#[gpui::test]
fn desktop_server_refused_personal_edit_restores_the_publication(cx: &mut TestAppContext) {
    let replica = three_tabs(A);
    let (harness, cx) = server_manager(&replica, cx);

    // The provider admits the activation and fails it later.
    let second = tab_item(tab(A, 2), cx).center();
    click_at(second, cx);
    assert_eq!(harness.active(cx), Some(tab(A, 2)));
    harness
        .runtime
        .provider
        .finish(0, Err(DesktopClientError::StaleContext));
    cx.run_until_parked();
    assert_eq!(harness.active(cx), Some(tab(A, 1)));
    assert!(harness.natively_focused(cx));

    // The provider refuses the next edit before admitting it.
    harness
        .runtime
        .provider
        .refuse_with(DesktopClientError::Offline);
    harness.manager.update(cx, |manager, cx| {
        manager.drop_server_tab_at(tab(A, 3), 0, cx);
    });
    cx.run_until_parked();

    assert_eq!(harness.order(cx), vec![tab(A, 1), tab(A, 2), tab(A, 3)]);
    assert_eq!(
        harness.refused(),
        vec![
            DesktopClientError::StaleContext,
            DesktopClientError::Offline
        ]
    );
}

/// Creates a Tab and delivers the outcome and the publication in the given order.
fn create_with_order(outcome_first: bool, cx: &mut TestAppContext) {
    let mut replica = three_tabs(A);
    let (harness, cx) = server_manager(&replica, cx);
    cx.dispatch_action(CreateTab);
    cx.run_until_parked();
    assert_eq!(
        harness.runtime.begun(),
        vec![DesktopRequest::Shared {
            server: replica.org.server(),
            command: Command::CreateTab {
                workspace: ws(A, 1),
                source: TabCreationSource::Pane(pane(A, 1)),
            },
        }]
    );
    let created = operation(replica.org.server(), 1);
    let outcome = Outcome::CreatedTab {
        tab: tab(A, 4),
        pane: pane(A, 4),
    };
    let publish = |replica: &mut Replica, cx: &mut VisualTestContext| {
        replica.org.add_tab(1, 4, 4, None).publish();
        replica.reconcile_ready(&[]);
        assert_eq!(harness.apply(replica, cx), Ok(()));
    };

    if outcome_first {
        harness.runtime.provider.finish(0, Ok(Some(created)));
        cx.run_until_parked();
        harness
            .runtime
            .deliver(replica.org.server(), created, outcome, cx);
        assert_eq!(harness.active(cx), Some(tab(A, 1)));
        publish(&mut replica, cx);
    } else {
        publish(&mut replica, cx);
        assert_eq!(harness.active(cx), Some(tab(A, 1)));
        harness.runtime.provider.finish(0, Ok(Some(created)));
        cx.run_until_parked();
        harness
            .runtime
            .deliver(replica.org.server(), created, outcome, cx);
    }

    assert_eq!(harness.active(cx), Some(tab(A, 4)));
    assert!(harness.natively_focused(cx));
    assert_eq!(harness.runtime.begun()[1..], [activation(tab(A, 4))]);
    assert!(harness.events().is_empty());
}

#[gpui::test]
fn desktop_server_create_outcome_before_delta_activates_the_new_tab_once(cx: &mut TestAppContext) {
    create_with_order(true, cx);
}

#[gpui::test]
fn desktop_server_create_delta_before_outcome_activates_the_new_tab_once(cx: &mut TestAppContext) {
    create_with_order(false, cx);
}

#[gpui::test]
fn desktop_server_explicit_navigation_cancels_a_pending_creation(cx: &mut TestAppContext) {
    let mut replica = three_tabs(A);
    let (harness, cx) = server_manager(&replica, cx);
    cx.dispatch_action(CreateTab);
    cx.run_until_parked();
    let second = tab_item(tab(A, 2), cx).center();
    click_at(second, cx);

    let created = operation(replica.org.server(), 1);
    harness.runtime.provider.finish(0, Ok(Some(created)));
    cx.run_until_parked();
    harness.runtime.deliver(
        replica.org.server(),
        created,
        Outcome::CreatedTab {
            tab: tab(A, 4),
            pane: pane(A, 4),
        },
        cx,
    );
    replica.org.add_tab(1, 4, 4, None).publish();
    replica.reconcile_ready(&[]);
    assert_eq!(harness.apply(&replica, cx), Ok(()));

    // The pending navigation stays over a publication that predates it. Creation takes no focus.
    assert_eq!(harness.active(cx), Some(tab(A, 2)));
    assert_eq!(harness.runtime.begun().len(), 2);
}

#[gpui::test]
fn desktop_server_shared_create_is_refused_while_offline(cx: &mut TestAppContext) {
    let replica = three_tabs(A);
    cx.update(crate::ui::init)
        .expect("UI initialization should succeed");
    let runtime = TestRuntime::new(cx);
    let panes = Rc::new(TestPanes::default());
    let mut offline = publication(&replica);
    offline.status = ReplicaStatus::RetainedOffline;
    let refused = Rc::new(RefCell::new(Vec::new()));
    let recorded = Rc::clone(&refused);
    let (manager, cx) = cx.add_window_view({
        let runtime = runtime.runtime.clone();
        let factory: Rc<dyn ServerPaneFactory> = panes;
        move |window, cx| {
            let manager = TabManager::new_server_with(
                &offline,
                ws(A, 1),
                runtime,
                factory,
                PaneConstruction::testing(),
                Rc::new(RecordingOperatingSystemWindowDragPlatform::default()),
                window,
                cx,
            )
            .expect("every Tab is presented");
            cx.subscribe_self(move |_, event: &ServerEditRefused, _| {
                recorded.borrow_mut().push(event.0);
            })
            .detach();
            manager
        }
    });
    cx.update(|window, cx| {
        window.activate_window();
        manager.update(cx, |manager, cx| manager.focus(window, cx));
    });
    cx.run_until_parked();
    cx.dispatch_action(CreateTab);
    cx.run_until_parked();
    let second = tab_item(tab(A, 2), cx).center();
    click_at(second, cx);

    // Navigation and personal order stay available offline.
    assert_eq!(
        manager.read_with(cx, |manager, _| manager.active_server_tab()),
        Some(tab(A, 2))
    );
    assert_eq!(*refused.borrow(), vec![DesktopClientError::Offline]);
    assert_eq!(runtime.begun(), vec![activation(tab(A, 2))]);
}

#[gpui::test]
fn desktop_server_worktree_navigation_filters_personal_tab_order(cx: &mut TestAppContext) {
    let mut org = Org::new(A);
    org.add_workspace(1, 1, 1)
        .add_tab(1, 2, 2, Some(1))
        .add_tab(1, 3, 3, Some(1))
        .add_tab(1, 4, 4, Some(2));
    let replica = Replica::attached(org, 1);
    let (harness, cx) = server_manager(&replica, cx);
    assert_eq!(harness.visible(cx), vec![tab(A, 1)]);

    cx.update(|window, cx| {
        harness.manager.update(cx, |manager, cx| {
            manager.open_server_worktree(
                Some(worktree(A, 1)),
                Some(SelectionSource::Pointer),
                true,
                window,
                cx,
            );
        });
    });
    cx.run_until_parked();
    assert_eq!(harness.visible(cx), vec![tab(A, 2), tab(A, 3)]);
    let entered = harness.active(cx).expect("the Worktree has an Active Tab");
    assert_eq!(
        harness
            .manager
            .read_with(cx, |manager, _| manager.active_server_worktree()),
        Some(worktree(A, 1))
    );

    // Tab navigation wraps within the Worktree.
    let mut visited = Vec::new();
    for _ in 0..2 {
        cx.dispatch_action(NextTab);
        cx.run_until_parked();
        visited.push(harness.active(cx).unwrap());
    }
    let other = if entered == tab(A, 2) {
        tab(A, 3)
    } else {
        tab(A, 2)
    };
    assert_eq!(visited, vec![other, entered]);
    assert_eq!(harness.visible(cx), vec![tab(A, 2), tab(A, 3)]);

    // A Worktree without Tabs asks the server for one.
    cx.update(|window, cx| {
        harness.manager.update(cx, |manager, cx| {
            manager.open_server_worktree(
                Some(worktree(A, 9)),
                Some(SelectionSource::Pointer),
                true,
                window,
                cx,
            );
        });
    });
    cx.run_until_parked();
    assert_eq!(
        shared_commands(&harness.runtime.begun()),
        vec![DesktopRequest::Shared {
            server: replica.org.server(),
            command: Command::CreateTab {
                workspace: ws(A, 1),
                source: TabCreationSource::Worktree(worktree(A, 9)),
            },
        }]
    );
    assert_eq!(harness.visible(cx), vec![tab(A, 2), tab(A, 3)]);
}

#[gpui::test]
fn desktop_server_inactive_tab_reconciles_membership(cx: &mut TestAppContext) {
    let mut replica = three_tabs(A);
    let (harness, cx) = server_manager(&replica, cx);

    replica
        .org
        .split(2, 5, 2, CoreAxis::Vertical, SplitEdge::After)
        .publish();
    replica.reconcile_ready(&[]);
    assert_eq!(harness.apply(&replica, cx), Ok(()));

    assert_eq!(
        harness.presented_panes(tab(A, 2), cx),
        vec![pane(A, 2), pane(A, 5)]
    );
    assert_eq!(harness.active(cx), Some(tab(A, 1)));
    assert!(harness.natively_focused(cx));
    assert!(harness.runtime.begun().is_empty());
    assert!(harness.events().is_empty());
}

#[gpui::test]
fn desktop_server_close_tab_waits_for_authoritative_deletion(cx: &mut TestAppContext) {
    let mut replica = three_tabs(A);
    let (harness, cx) = server_manager(&replica, cx);

    harness.manager.update(cx, |manager, cx| {
        manager.request_close_server_tab(tab(A, 2), cx);
    });
    cx.run_until_parked();
    assert_eq!(
        harness.events(),
        vec![ServerTabManagerEvent::CloseTab { tab: tab(A, 2) }]
    );
    assert_eq!(harness.order(cx), vec![tab(A, 1), tab(A, 2), tab(A, 3)]);

    replica.org.delete_tab(2).publish();
    replica.reconcile_ready(&[]);
    assert_eq!(harness.apply(&replica, cx), Ok(()));
    assert_eq!(harness.order(cx), vec![tab(A, 1), tab(A, 3)]);
    assert!(harness.runtime.begun().is_empty());
}

#[gpui::test]
fn desktop_server_reconcile_during_drag_cancels_deleted_target(cx: &mut TestAppContext) {
    let mut replica = three_tabs(A);
    let (harness, cx) = server_manager(&replica, cx);
    let third = tab_item(tab(A, 3), cx);
    let first = tab_item(tab(A, 1), cx);
    let over = point(first.left() + px(2.0), first.center().y);
    begin_drag(
        third.center(),
        &[third.center() - point(px(8.0), px(0.0)), over],
        cx,
    );
    assert_eq!(
        harness
            .manager
            .read_with(cx, |manager, _| manager.tab_reorder.dragged()),
        Some(TabKey::Server(tab(A, 3)))
    );

    replica.org.delete_tab(3).publish();
    replica.reconcile_ready(&[]);
    assert_eq!(harness.apply(&replica, cx), Ok(()));
    cx.simulate_mouse_up(over, MouseButton::Left, Modifiers::none());
    cx.run_until_parked();

    assert_eq!(
        harness
            .manager
            .read_with(cx, |manager, _| manager.tab_reorder.dragged()),
        None
    );
    assert_eq!(harness.order(cx), vec![tab(A, 1), tab(A, 2)]);
    assert!(harness.runtime.begun().is_empty());
}

#[gpui::test]
fn desktop_server_equal_local_ids_on_two_servers_do_not_alias(cx: &mut TestAppContext) {
    let replica = three_tabs(A);
    let mut other = three_tabs(B);
    other.org.delete_tab(3).publish();
    other.reconcile_ready(&[]);
    let (harness, cx) = server_manager(&replica, cx);
    assert_eq!(tab(A, 3).local(), tab(B, 3).local());

    let frame = frame(&other, ReplicaStatus::Ready);
    let record = other.workspace_record(1);
    let result = harness.manager.update(cx, |manager, cx| {
        manager.apply_workspace(&frame, &record, other.view(1), cx)
    });

    assert_eq!(result, Err(DesktopClientError::WrongServer));
    assert_eq!(harness.order(cx), vec![tab(A, 1), tab(A, 2), tab(A, 3)]);
    let second = tab_item(tab(A, 2), cx).center();
    click_at(second, cx);
    assert_eq!(
        harness.events(),
        vec![ServerTabManagerEvent::OpenTab {
            tab: tab(A, 2),
            source: SelectionSource::Pointer,
        }]
    );
}

#[gpui::test]
fn desktop_server_tab_pane_events_reach_the_workspace_manager(cx: &mut TestAppContext) {
    let replica = three_tabs(A);
    let (harness, cx) = server_manager(&replica, cx);
    let view = harness
        .manager
        .read_with(cx, |manager, _| manager.server_tab(tab(A, 1)))
        .unwrap();
    let terminal = view
        .read_with(cx, |view, _| view.server_terminal(pane(A, 1)))
        .unwrap();

    terminal.update(cx, |_, cx| {
        cx.emit(crate::ui::TerminalPaneEvent::SelectionRequested(
            SelectionSource::Pointer,
        ));
    });
    cx.run_until_parked();

    assert_eq!(
        harness.events(),
        vec![ServerTabManagerEvent::SelectPane {
            pane: pane(A, 1),
            source: SelectionSource::Pointer,
        }]
    );
}

fn accessible_tabs(cx: &mut VisualTestContext) -> Vec<serde_json::Value> {
    spaceterm_ui::a11y_testing::A11yTree::read(cx)
        .in_order()
        .into_iter()
        .filter(|node| node["aria"]["role"] == "Tab")
        .cloned()
        .collect()
}

#[gpui::test]
fn desktop_server_tab_strip_reports_personal_order_to_accessibility(cx: &mut TestAppContext) {
    let mut replica = three_tabs(A);
    let (harness, cx) = server_manager(&replica, cx);
    let original = accessible_tabs(cx);
    let snapshot = replica.org.snapshot();
    replica
        .personal
        .move_tab(ws(A, 1), tab(A, 3), 0, &snapshot)
        .unwrap();
    harness.apply(&replica, cx).unwrap();

    let tabs = accessible_tabs(cx);
    assert_eq!(harness.visible(cx), vec![tab(A, 3), tab(A, 1), tab(A, 2)]);
    for (index, original_index) in [2, 0, 1].into_iter().enumerate() {
        assert_eq!(
            tabs[index]["accesskit_id"],
            original[original_index]["accesskit_id"]
        );
        assert_eq!(tabs[index]["aria"]["position_in_set"], index + 1);
        assert_eq!(tabs[index]["aria"]["size_of_set"], 3);
    }
}

#[gpui::test]
fn desktop_server_remote_tab_append_is_not_announced(cx: &mut TestAppContext) {
    let mut replica = three_tabs(A);
    let snapshot = replica.org.snapshot();
    replica
        .personal
        .move_tab(ws(A, 1), tab(A, 3), 0, &snapshot)
        .unwrap();
    let (harness, cx) = server_manager(&replica, cx);
    let before = accessible_tabs(cx);
    let announcements = |cx: &mut VisualTestContext| {
        spaceterm_ui::a11y_testing::A11yTree::read(cx)
            .in_order()
            .into_iter()
            .filter(|node| matches!(node["aria"]["role"].as_str(), Some("Status" | "Alert")))
            .map(|node| node["aria"].to_string())
            .collect::<std::collections::BTreeSet<_>>()
    };
    let notices = announcements(cx);
    let active = harness.active(cx);
    replica.org.add_tab(1, 4, 4, None).publish();
    replica.reconcile_ready(&[]);
    harness.apply(&replica, cx).unwrap();

    let tabs = accessible_tabs(cx);
    assert_eq!(tabs.len(), 4);
    for (index, original) in before.iter().enumerate() {
        assert_eq!(tabs[index]["accesskit_id"], original["accesskit_id"]);
        assert_eq!(tabs[index]["aria"]["position_in_set"], index + 1);
        assert_eq!(tabs[index]["aria"]["size_of_set"], 4);
        let mut semantics = tabs[index]["aria"].clone();
        semantics["size_of_set"] = original["aria"]["size_of_set"].clone();
        assert_eq!(semantics, original["aria"]);
    }
    assert_eq!(announcements(cx), notices);
    assert_eq!(harness.active(cx), active);
    assert!(harness.natively_focused(cx));
    assert!(harness.events().is_empty());
    assert!(harness.runtime.begun().is_empty());
}

#[gpui::test]
fn desktop_server_pending_activation_survives_an_older_personal_publication(
    cx: &mut TestAppContext,
) {
    let mut replica = three_tabs(A);
    let (harness, cx) = server_manager(&replica, cx);
    click_at(tab_item(tab(A, 2), cx).center(), cx);
    replica.org.add_tab(1, 4, 4, None).publish();
    replica.reconcile_ready(&[]);
    harness.apply(&replica, cx).unwrap();
    assert_eq!(harness.active(cx), Some(tab(A, 2)));
    assert!(harness.natively_focused(cx));
    assert_eq!(harness.runtime.begun(), vec![activation(tab(A, 2))]);
}

#[gpui::test]
fn desktop_server_refused_activation_preserves_a_pending_reorder(cx: &mut TestAppContext) {
    let replica = three_tabs(A);
    let (harness, cx) = server_manager(&replica, cx);
    harness.manager.update(cx, |manager, cx| {
        manager.drop_server_tab_at(tab(A, 3), 0, cx)
    });
    click_at(tab_item(tab(A, 2), cx).center(), cx);
    harness
        .runtime
        .provider
        .finish(1, Err(DesktopClientError::StaleContext));
    cx.run_until_parked();
    assert_eq!(harness.active(cx), Some(tab(A, 1)));
    assert_eq!(harness.order(cx), vec![tab(A, 3), tab(A, 1), tab(A, 2)]);
    assert!(harness.natively_focused(cx));
}

#[gpui::test]
fn desktop_server_deleted_inactive_tab_chip_restores_native_focus(cx: &mut TestAppContext) {
    use spaceterm_ui::a11y_testing::perform;
    let mut replica = three_tabs(A);
    let (harness, cx) = server_manager(&replica, cx);
    let tabs = accessible_tabs(cx);
    perform(cx, &tabs[1], gpui::accesskit::Action::Focus);
    assert!(!harness.natively_focused(cx));
    replica.org.delete_tab(2).publish();
    replica.reconcile_ready(&[]);
    harness.apply(&replica, cx).unwrap();
    assert_eq!(harness.active(cx), Some(tab(A, 1)));
    assert!(harness.natively_focused(cx));
    assert!(harness.runtime.begun().is_empty());
    assert!(harness.events().is_empty());
}

#[gpui::test]
fn desktop_server_worktree_navigation_without_use_cancels_creation_attention(
    cx: &mut TestAppContext,
) {
    let mut org = Org::new(A);
    org.add_workspace(1, 1, 1).add_tab(1, 2, 2, Some(1));
    let mut replica = Replica::attached(org, 1);
    let (harness, cx) = server_manager(&replica, cx);
    cx.dispatch_action(CreateTab);
    cx.update(|window, cx| {
        harness.manager.update(cx, |manager, cx| {
            manager.open_server_worktree(Some(worktree(A, 1)), None, true, window, cx);
        })
    });
    cx.run_until_parked();
    assert_eq!(harness.active(cx), Some(tab(A, 2)));
    let created = operation(replica.org.server(), 1);
    harness.runtime.provider.finish(0, Ok(Some(created)));
    cx.run_until_parked();
    harness.runtime.deliver(
        replica.org.server(),
        created,
        Outcome::CreatedTab {
            tab: tab(A, 3),
            pane: pane(A, 3),
        },
        cx,
    );
    replica.org.add_tab(1, 3, 3, None).publish();
    replica.reconcile_ready(&[]);
    harness.apply(&replica, cx).unwrap();
    assert_eq!(harness.active(cx), Some(tab(A, 2)));
    assert_eq!(harness.runtime.begun().len(), 2);
    assert!(harness.events().is_empty());
}

#[gpui::test]
fn desktop_server_switching_tabs_cancels_pending_split_attention(cx: &mut TestAppContext) {
    let mut replica = three_tabs(A);
    let (harness, cx) = server_manager(&replica, cx);
    let first = harness
        .manager
        .read_with(cx, |manager, _| manager.server_tab(tab(A, 1)))
        .unwrap();
    cx.update(|window, cx| {
        first.update(cx, |view, cx| {
            view.split_focused(crate::domain::SplitAxis::Horizontal, window, cx)
        })
    });
    cx.dispatch_action(NextTab);
    cx.run_until_parked();
    let created = operation(replica.org.server(), 1);
    harness.runtime.provider.finish(0, Ok(Some(created)));
    cx.run_until_parked();
    harness.runtime.deliver(
        replica.org.server(),
        created,
        Outcome::CreatedPane { pane: pane(A, 4) },
        cx,
    );
    replica
        .org
        .split(1, 4, 1, CoreAxis::Horizontal, SplitEdge::After)
        .publish();
    replica.reconcile_ready(&[]);
    harness.apply(&replica, cx).unwrap();
    assert_eq!(
        first.read_with(cx, |view, _| view.focused_server_pane()),
        Some(pane(A, 1))
    );
    assert_eq!(harness.active(cx), Some(tab(A, 2)));
    assert_eq!(harness.runtime.begun().len(), 2, "no delayed focus edit");
}

#[gpui::test]
fn desktop_server_unavailable_tab_menu_disables_close_but_keeps_reorder(cx: &mut TestAppContext) {
    use spaceterm_ui::a11y_testing::{A11yTree, perform};
    let replica = three_tabs(A);
    let (harness, cx) = server_manager(&replica, cx);
    for (status, refusal) in [
        (ReplicaStatus::RetainedOffline, DesktopClientError::Offline),
        (
            ReplicaStatus::Resynchronizing,
            DesktopClientError::Reconciling,
        ),
    ] {
        let frame = frame(&replica, status);
        let record = replica.workspace_record(1);
        harness
            .manager
            .update(cx, |manager, cx| {
                manager.apply_workspace(&frame, &record, replica.view(1), cx)
            })
            .unwrap();
        let tabs = accessible_tabs(cx);
        perform(cx, &tabs[0], gpui::accesskit::Action::ShowContextMenu);
        let tree = A11yTree::read(cx);
        let menu = tree.with_role("Menu")[0];
        let rows = tree.children(menu);
        for label in ["New Tab", "Split Right", "Split Down", "Close Tab"] {
            let row = rows
                .iter()
                .find(|row| row["aria"]["label"] == label)
                .unwrap();
            assert_eq!(row["aria"]["disabled"], true, "{status:?}: {label}");
            perform(cx, row, gpui::accesskit::Action::Click);
        }
        let move_right = rows
            .iter()
            .find(|row| row["aria"]["label"] == "Move Tab Right")
            .unwrap();
        assert_ne!(move_right["aria"]["disabled"], true);
        cx.simulate_keystrokes("escape");
        assert!(harness.runtime.begun().is_empty());
        assert!(harness.events().is_empty());
        harness.manager.update(cx, |manager, cx| {
            manager.request_close_server_tab(tab(A, 1), cx)
        });
        cx.run_until_parked();
        assert_eq!(harness.refused().last(), Some(&refusal));
        assert!(harness.events().is_empty());
    }
}

fn missing_middle_tab(cx: &mut TestAppContext) -> (Harness, &mut VisualTestContext) {
    let mut org = Org::new(A);
    org.add_workspace(1, 1, 1).add_tab(1, 3, 3, None);
    let mut replica = Replica::attached(org, 1);
    let (harness, cx) = server_manager(&replica, cx);
    harness
        .panes
        .fail_with(Some(DesktopClientError::UnsupportedCapability));
    replica.org.add_tab(1, 2, 2, None).publish();
    replica.reconcile_ready(&[]);
    let snapshot = replica.org.snapshot();
    replica
        .personal
        .move_tab(ws(A, 1), tab(A, 2), 1, &snapshot)
        .unwrap();
    harness.apply(&replica, cx).unwrap();
    assert_eq!(harness.visible(cx), vec![tab(A, 1), tab(A, 3)]);
    (harness, cx)
}

#[gpui::test]
fn desktop_server_tab_drag_maps_visible_positions_past_a_missing_view(cx: &mut TestAppContext) {
    let (harness, cx) = missing_middle_tab(cx);
    let first = tab_item(tab(A, 1), cx);
    let third = tab_item(tab(A, 3), cx);
    let to = point(third.right() - px(2.0), third.center().y);
    begin_drag(
        first.center(),
        &[first.center() + point(px(8.0), px(0.0)), to],
        cx,
    );
    cx.simulate_mouse_up(to, MouseButton::Left, Modifiers::none());
    cx.run_until_parked();
    assert_eq!(harness.visible(cx), vec![tab(A, 3), tab(A, 1)]);
    assert_eq!(
        harness.runtime.begun(),
        vec![DesktopRequest::PersonalEdit(ViewEdit::MoveTab {
            workspace: ws(A, 1),
            tab: tab(A, 1),
            position: 2,
        })]
    );
    assert!(harness.events().is_empty());
}

#[gpui::test]
fn desktop_server_adjacent_navigation_skips_a_missing_tab_view(cx: &mut TestAppContext) {
    let (harness, cx) = missing_middle_tab(cx);
    cx.dispatch_action(NextTab);
    cx.run_until_parked();
    assert_eq!(harness.active(cx), Some(tab(A, 3)));
    cx.dispatch_action(PreviousTab);
    cx.run_until_parked();
    assert_eq!(harness.active(cx), Some(tab(A, 1)));
    assert_eq!(
        harness.runtime.begun(),
        vec![activation(tab(A, 3)), activation(tab(A, 1))]
    );
}

#[gpui::test]
fn desktop_server_move_tab_right_skips_a_missing_tab_view(cx: &mut TestAppContext) {
    let (harness, cx) = missing_middle_tab(cx);
    cx.dispatch_action(MoveTabRight);
    cx.run_until_parked();
    assert_eq!(harness.visible(cx), vec![tab(A, 3), tab(A, 1)]);
    assert_eq!(harness.active(cx), Some(tab(A, 1)));
    assert_eq!(
        harness.runtime.begun(),
        vec![DesktopRequest::PersonalEdit(ViewEdit::MoveTab {
            workspace: ws(A, 1),
            tab: tab(A, 1),
            position: 2,
        })]
    );
    assert!(harness.events().is_empty());
}

#[gpui::test]
fn desktop_server_deleting_a_focused_chip_inside_a_window_update_restores_focus(
    cx: &mut TestAppContext,
) {
    use spaceterm_ui::a11y_testing::perform;
    let mut replica = three_tabs(A);
    let (harness, cx) = server_manager(&replica, cx);
    let tabs = accessible_tabs(cx);
    perform(cx, &tabs[1], gpui::accesskit::Action::Focus);
    replica.org.delete_tab(2).publish();
    replica.reconcile_ready(&[]);
    let frame = frame(&replica, ReplicaStatus::Ready);
    let record = replica.workspace_record(1);
    cx.update(|_, cx| {
        harness.manager.update(cx, |manager, cx| {
            manager
                .apply_workspace(&frame, &record, replica.view(1), cx)
                .unwrap();
        })
    });
    cx.run_until_parked();
    assert!(harness.natively_focused(cx));
    assert!(harness.runtime.begun().is_empty());
}

#[gpui::test]
fn desktop_server_pending_step_is_not_repeated_after_it_is_published(cx: &mut TestAppContext) {
    let mut replica = three_tabs(A);
    let (harness, cx) = server_manager(&replica, cx);
    cx.dispatch_action(MoveTabRight);
    cx.run_until_parked();
    let snapshot = replica.org.snapshot();
    replica
        .personal
        .step_tab(ws(A, 1), tab(A, 1), ViewTabStep::Next, &snapshot)
        .unwrap();
    harness.apply(&replica, cx).unwrap();
    assert_eq!(harness.order(cx), vec![tab(A, 2), tab(A, 1), tab(A, 3)]);
    assert_eq!(harness.runtime.begun().len(), 1);
}

#[gpui::test]
fn desktop_server_materialized_active_tab_respects_a_native_focus_blocker(cx: &mut TestAppContext) {
    let mut org = Org::new(A);
    org.add_workspace(1, 1, 1);
    let mut replica = Replica::attached(org, 1);
    let (harness, cx) = server_manager(&replica, cx);
    harness
        .panes
        .fail_with(Some(DesktopClientError::UnsupportedCapability));
    replica.org.add_tab(1, 2, 2, None).publish();
    replica.reconcile_ready(&[]);
    harness.apply(&replica, cx).unwrap();
    replica.org.set_root_tab(1, 2).delete_tab(1).publish();
    replica.reconcile_ready(&[]);
    harness.apply(&replica, cx).unwrap();
    assert_eq!(harness.active(cx), Some(tab(A, 2)));
    assert!(harness.order(cx).is_empty());

    let modal = cx.update(|window, cx| {
        let modal = cx.focus_handle();
        window.focus(&modal, cx);
        harness.manager.update(cx, |manager, cx| {
            manager.set_parent_focus_blocker(
                Some(crate::ui::terminal_focus::TerminalFocusBlocker::Modal),
                cx,
            );
        });
        modal
    });
    harness.panes.fail_with(None);
    harness.apply(&replica, cx).unwrap();
    assert_eq!(harness.order(cx), vec![tab(A, 2)]);
    assert!(cx.update(|window, _| modal.is_focused(window)));
    assert!(!harness.natively_focused(cx));
    assert!(harness.runtime.begun().is_empty());
    assert!(harness.events().is_empty());

    // Once the modal closes, restoration reaches the newly presented Tab without opening it.
    cx.update(|window, cx| {
        harness.manager.update(cx, |manager, cx| {
            manager.set_parent_focus_blocker(None, cx);
            manager.restore_focus(window, cx);
        });
    });
    assert!(harness.natively_focused(cx));
    assert!(harness.events().is_empty());
}

#[gpui::test]
fn desktop_server_worktree_navigation_skips_its_missing_preferred_tab(cx: &mut TestAppContext) {
    let mut org = Org::new(A);
    org.add_workspace(1, 1, 1).add_tab(1, 3, 3, Some(1));
    let mut replica = Replica::attached(org, 1);
    let (harness, cx) = server_manager(&replica, cx);
    harness
        .panes
        .fail_with(Some(DesktopClientError::UnsupportedCapability));
    replica.org.add_tab(1, 2, 2, Some(1)).publish();
    replica.reconcile_ready(&[]);
    replica
        .personal
        .move_tab(ws(A, 1), tab(A, 2), 0, &replica.org.snapshot())
        .unwrap();
    harness.apply(&replica, cx).unwrap();
    assert_eq!(
        replica.view(1).tab_order(),
        &[tab(A, 1), tab(A, 2), tab(A, 3)]
    );
    let opened = cx.update(|window, cx| {
        harness.manager.update(cx, |manager, cx| {
            manager.open_server_worktree(Some(worktree(A, 1)), None, true, window, cx)
        })
    });
    cx.run_until_parked();
    assert!(opened, "a presented Tab in this Worktree is usable");
    assert_eq!(harness.active(cx), Some(tab(A, 3)));
    assert_eq!(harness.visible(cx), vec![tab(A, 3)]);
    assert!(harness.natively_focused(cx));
    assert_eq!(harness.runtime.begun(), vec![activation(tab(A, 3))]);
    assert!(harness.events().is_empty());
    assert_eq!(
        replica.view(1).tab_order(),
        &[tab(A, 1), tab(A, 2), tab(A, 3)]
    );
}

#[gpui::test]
fn desktop_server_completed_reorder_survives_a_failure_before_its_publication(
    cx: &mut TestAppContext,
) {
    let mut replica = three_tabs(A);
    let (harness, cx) = server_manager(&replica, cx);
    harness.manager.update(cx, |manager, cx| {
        manager.drop_server_tab_at(tab(A, 3), 0, cx)
    });
    cx.update(|window, cx| {
        harness.manager.update(cx, |manager, cx| {
            manager.open_server_worktree(None, None, true, window, cx);
            manager.open_server_tab(tab(A, 2), None, window, cx);
        })
    });
    harness.runtime.provider.finish(0, Ok(None));
    cx.run_until_parked();
    harness
        .runtime
        .provider
        .finish(1, Err(DesktopClientError::StaleContext));
    cx.run_until_parked();
    assert_eq!(harness.order(cx), vec![tab(A, 3), tab(A, 1), tab(A, 2)]);
    assert_eq!(harness.active(cx), Some(tab(A, 1)));
    assert!(harness.natively_focused(cx));
    assert!(harness.events().is_empty());
    assert_eq!(harness.refused(), vec![DesktopClientError::StaleContext]);
    let snapshot = replica.org.snapshot();
    replica
        .personal
        .move_tab(ws(A, 1), tab(A, 3), 0, &snapshot)
        .unwrap();
    harness.receipt(0, 1, cx);
    harness.apply_at(&replica, 1, cx).unwrap();
    replica
        .personal
        .move_tab(ws(A, 1), tab(A, 3), 2, &snapshot)
        .unwrap();
    harness.apply_at(&replica, 2, cx).unwrap();
    assert_eq!(harness.order(cx), vec![tab(A, 1), tab(A, 2), tab(A, 3)]);
}

#[gpui::test]
fn desktop_server_completed_unpublished_tab_edits_are_bounded(cx: &mut TestAppContext) {
    let replica = three_tabs(A);
    let (harness, cx) = server_manager(&replica, cx);
    for index in 0..crate::ui::server_client_runtime::MAX_PENDING_UI_REQUESTS {
        harness.manager.update(cx, |manager, cx| {
            manager.drop_server_tab_at(tab(A, 3), if index % 2 == 0 { 0 } else { 2 }, cx)
        });
        harness.runtime.provider.finish(index, Ok(None));
        cx.run_until_parked();
    }
    let accepted = harness.order(cx);
    harness.manager.update(cx, |manager, cx| {
        manager.drop_server_tab_at(tab(A, 3), 0, cx)
    });
    cx.run_until_parked();
    assert_eq!(harness.runtime.begun().len(), 128);
    assert_eq!(harness.refused(), vec![DesktopClientError::AdmissionFull]);
    assert_eq!(harness.order(cx), accepted);
    harness.apply(&replica, cx).unwrap();
    harness.manager.update(cx, |manager, cx| {
        manager.drop_server_tab_at(tab(A, 3), 0, cx)
    });
    assert_eq!(
        harness.runtime.begun().len(),
        128,
        "an unchanged frame cannot reclaim retained results"
    );
    for request in 0..128 {
        harness.receipt(request, 1, cx);
    }
    harness.apply_at(&replica, 1, cx).unwrap();
    harness.manager.update(cx, |manager, cx| {
        manager.drop_server_tab_at(tab(A, 3), 0, cx)
    });
    assert_eq!(harness.runtime.begun().len(), 129);
    assert!(harness.events().is_empty());
}

#[gpui::test]
fn desktop_server_worktree_with_only_missing_presentations_does_not_create_replacement_work(
    cx: &mut TestAppContext,
) {
    let mut org = Org::new(A);
    org.add_workspace(1, 1, 1);
    let mut replica = Replica::attached(org, 1);
    let (harness, cx) = server_manager(&replica, cx);
    harness
        .panes
        .fail_with(Some(DesktopClientError::UnsupportedCapability));
    replica.org.add_tab(1, 2, 2, Some(1)).publish();
    replica.reconcile_ready(&[]);
    harness.apply(&replica, cx).unwrap();
    let opened = cx.update(|window, cx| {
        harness.manager.update(cx, |manager, cx| {
            manager.open_server_worktree(Some(worktree(A, 1)), None, true, window, cx)
        })
    });
    cx.run_until_parked();
    assert!(!opened);
    assert_eq!(harness.active(cx), Some(tab(A, 1)));
    assert!(harness.runtime.begun().is_empty());
    assert!(harness.events().is_empty());
}

#[gpui::test]
fn desktop_server_opposite_reorders_do_not_acknowledge_an_unchanged_publication(
    cx: &mut TestAppContext,
) {
    let replica = three_tabs(A);
    let (harness, cx) = server_manager(&replica, cx);
    for position in [0, 2] {
        harness.manager.update(cx, |manager, cx| {
            manager.drop_server_tab_at(tab(A, 3), position, cx)
        });
    }
    assert_eq!(harness.order(cx), vec![tab(A, 1), tab(A, 2), tab(A, 3)]);
    // This unrelated frame contains neither edit, although its order matches their combined result.
    harness.apply(&replica, cx).unwrap();
    harness.runtime.provider.finish(0, Ok(None));
    cx.run_until_parked();
    harness
        .runtime
        .provider
        .finish(1, Err(DesktopClientError::StaleContext));
    cx.run_until_parked();
    assert_eq!(harness.order(cx), vec![tab(A, 3), tab(A, 1), tab(A, 2)]);
    assert_eq!(harness.refused(), vec![DesktopClientError::StaleContext]);
    assert!(harness.events().is_empty());
}

#[gpui::test]
fn desktop_server_application_receipts_forward_to_panes_and_wait_for_their_frame(
    cx: &mut TestAppContext,
) {
    use crate::ui::server_desktop_contracts::ArrangementEdit;
    let mut replica = three_tabs(A);
    replica
        .org
        .split(1, 4, 1, CoreAxis::Horizontal, SplitEdge::After)
        .publish();
    replica.reconcile_ready(&[]);
    let original = replica.clone();
    let (harness, cx) = server_manager(&replica, cx);
    harness.manager.update(cx, |manager, cx| {
        manager.drop_server_tab_at(tab(A, 3), 0, cx)
    });
    let view = harness
        .manager
        .read_with(cx, |manager, _| manager.server_tab(tab(A, 1)))
        .unwrap();
    let terminal = view
        .read_with(cx, |view, _| view.server_terminal(pane(A, 4)))
        .unwrap();
    terminal.update(cx, |_, cx| {
        cx.emit(crate::ui::TerminalPaneEvent::FocusRequested)
    });
    cx.run_until_parked();
    harness.receipt(0, 1, cx);
    harness.receipt(1, 1, cx);
    for index in 0..2 {
        harness.runtime.provider.finish(index, Ok(None));
    }
    cx.run_until_parked();
    harness.apply(&original, cx).unwrap();
    assert_eq!(harness.order(cx), vec![tab(A, 3), tab(A, 1), tab(A, 2)]);
    assert_eq!(
        view.read_with(cx, |view, _| view.focused_server_pane()),
        Some(pane(A, 4))
    );
    let snapshot = replica.org.snapshot();
    replica
        .personal
        .move_tab(ws(A, 1), tab(A, 3), 0, &snapshot)
        .unwrap();
    replica
        .personal
        .arrange(ws(A, 1), tab(A, 1), ArrangementEdit::Focus(pane(A, 4)))
        .unwrap();
    harness.apply_at(&replica, 1, cx).unwrap();
    replica
        .personal
        .move_tab(ws(A, 1), tab(A, 3), 2, &snapshot)
        .unwrap();
    replica
        .personal
        .arrange(ws(A, 1), tab(A, 1), ArrangementEdit::Focus(pane(A, 1)))
        .unwrap();
    harness.apply_at(&replica, 2, cx).unwrap();
    assert_eq!(harness.order(cx), vec![tab(A, 1), tab(A, 2), tab(A, 3)]);
    assert_eq!(
        view.read_with(cx, |view, _| view.focused_server_pane()),
        Some(pane(A, 1))
    );
    assert_eq!(
        harness.apply(&original, cx),
        Err(DesktopClientError::StaleContext)
    );
    assert!(harness.natively_focused(cx));
    assert!(harness.events().is_empty());
    assert!(harness.refused().is_empty());
}

#[gpui::test]
fn desktop_server_frame_before_receipt_cannot_acknowledge_an_opposite_reorder(
    cx: &mut TestAppContext,
) {
    let mut replica = three_tabs(A);
    let (harness, cx) = server_manager(&replica, cx);
    for position in [0, 2] {
        harness.manager.update(cx, |manager, cx| {
            manager.drop_server_tab_at(tab(A, 3), position, cx)
        });
    }
    let snapshot = replica.org.snapshot();
    replica
        .personal
        .move_tab(ws(A, 1), tab(A, 3), 0, &snapshot)
        .unwrap();
    harness.apply_at(&replica, 1, cx).unwrap();
    // The prefix is in the frame, but both tasks remain pending when its receipt arrives.
    harness.receipt(0, 1, cx);
    harness
        .runtime
        .provider
        .finish(1, Err(DesktopClientError::StaleContext));
    cx.run_until_parked();
    assert_eq!(harness.order(cx), vec![tab(A, 3), tab(A, 1), tab(A, 2)]);
    harness.runtime.provider.finish(0, Ok(None));
    cx.run_until_parked();
    replica
        .personal
        .move_tab(ws(A, 1), tab(A, 3), 2, &snapshot)
        .unwrap();
    harness.apply_at(&replica, 2, cx).unwrap();
    assert_eq!(harness.order(cx), vec![tab(A, 1), tab(A, 2), tab(A, 3)]);
    assert!(harness.events().is_empty());
}
