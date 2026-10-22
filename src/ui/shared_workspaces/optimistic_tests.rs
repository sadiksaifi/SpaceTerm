//! Deterministic delivery around the production server mutation engine and shared desktop path.
//! The double delays transport delivery; State::apply owns validation, identities and membership.
use super::*;
use crate::terminal::shared_workspace::SharedWorkspaceSnapshot;
use spaceterm_server::hierarchy::{RetainedOutcome, State};
use std::cell::RefCell;

const DEVICE: Id = [21; 32];
const OTHER_DEVICE: Id = [22; 32];

struct Delivery {
    server: State,
    snapshot: SharedWorkspaceSnapshot,
    original: Option<(u64, Mutation)>,
    dispatched: Vec<Mutation>,
}
struct DelayedServer(RefCell<Delivery>);

impl DelayedServer {
    fn new() -> Rc<Self> {
        let mut server = State::empty();
        server.begin_boot(BOOT);
        server
            .ensure_first_workspace(DEVICE, std::path::Path::new("/fixture/home"))
            .unwrap();
        let inventory = live_inventory(&server);
        server.inventory.close.clone_from(&inventory.close);
        Rc::new(Self(RefCell::new(Delivery {
            server,
            snapshot: SharedWorkspaceSnapshot {
                end_all_sessions: true,
                worktree_writes: false,
                inventory: Some(inventory),
                connected: true,
                pending: false,
                pending_operation: None,
                rejected: None,
                outcome: None,
                error: None,
            },
            original: None,
            dispatched: Vec::new(),
        })))
    }
    fn inventory(&self) -> Inventory {
        self.0.borrow().snapshot.inventory.clone().unwrap()
    }
    fn dispatched(&self) -> Vec<Mutation> {
        self.0.borrow().dispatched.clone()
    }
    fn connected(&self, connected: bool) {
        self.0.borrow_mut().snapshot.connected = connected;
    }

    fn finish(&self, acknowledge: bool, valid_directory: bool) -> RetainedOutcome {
        let mut delivery = self.0.borrow_mut();
        let (sequence, mutation) = delivery
            .original
            .clone()
            .expect("an Original Operation was dispatched");
        // Repeating this same identity resolves an admitted operation; it creates no replacement.
        delivery
            .server
            .apply(
                DEVICE,
                sequence,
                mutation,
                std::path::Path::new("/fixture/home"),
                |_| valid_directory,
            )
            .unwrap();
        let result = delivery.server.outcome(DEVICE, sequence).unwrap().clone();
        let inventory = live_inventory(&delivery.server);
        delivery.server.inventory.close.clone_from(&inventory.close);
        delivery.snapshot.inventory = Some(inventory);
        delivery.snapshot.outcome = Some(result.clone());
        if acknowledge {
            delivery.server.acknowledge(DEVICE, sequence).unwrap();
            delivery.snapshot.pending = false;
            delivery.snapshot.pending_operation = None;
            delivery.original = None;
        }
        result
    }

    fn rename_elsewhere(&self, workspace: Id, name: &str) {
        let mut delivery = self.0.borrow_mut();
        let sequence = delivery.server.next_sequence(OTHER_DEVICE);
        let version = delivery
            .server
            .inventory
            .workspaces
            .iter()
            .find(|w| w.id == workspace)
            .unwrap()
            .name_version;
        delivery
            .server
            .apply(
                OTHER_DEVICE,
                sequence,
                Mutation::RenameWorkspace {
                    workspace,
                    version,
                    name: name.into(),
                },
                std::path::Path::new("/fixture/home"),
                |_| true,
            )
            .unwrap();
        delivery.server.acknowledge(OTHER_DEVICE, sequence).unwrap();
        let inventory = live_inventory(&delivery.server);
        delivery.server.inventory.close.clone_from(&inventory.close);
        delivery.snapshot.inventory = Some(inventory);
    }
}

fn live_inventory(server: &State) -> Inventory {
    let mut inventory = server.inventory.clone();
    inventory.close = Some(CloseInventory {
        boot: BOOT,
        sessions: inventory
            .workspaces
            .iter()
            .flat_map(|w| &w.tabs)
            .flat_map(|t| &t.panes)
            .map(|pane| ConfirmedPane {
                id: pane.id,
                session: None,
            })
            .collect(),
    });
    inventory
}

impl SharedWorkspaceEdits for DelayedServer {
    fn snapshot(&self) -> SharedWorkspaceSnapshot {
        self.0.borrow().snapshot.clone()
    }
    fn availability(&self) -> (bool, bool) {
        let delivery = self.0.borrow();
        (delivery.snapshot.connected, delivery.snapshot.pending)
    }
    fn submit(&self, mutation: Mutation) -> bool {
        let mut delivery = self.0.borrow_mut();
        if !delivery.snapshot.connected || delivery.snapshot.pending {
            return false;
        }
        let sequence = delivery.server.next_sequence(DEVICE);
        delivery.original = Some((sequence, mutation.clone()));
        delivery.dispatched.push(mutation.clone());
        delivery.snapshot.pending = true;
        delivery.snapshot.pending_operation = Some(mutation);
        delivery.snapshot.outcome = None;
        delivery.snapshot.rejected = None;
        true
    }
}

fn sessions() -> (
    SharedPaneSessions,
    Rc<ScriptedPanes>,
    WorkspaceTerminalSessionFactory,
) {
    let factory: Rc<dyn TerminalSessionFactory> =
        Rc::new(crate::terminal::testing::TestTerminalSessionFactory::new(
            crate::terminal::testing::TestTerminalSessionRecords::default(),
        ));
    let factory = WorkspaceTerminalSessionFactory::new_local(
        factory,
        crate::terminal::testing::test_local_directory(PathBuf::from("/tmp/spaceterm-optimistic")),
    );
    let panes = ScriptedPanes::new(factory.clone());
    let sessions = SharedPaneSessions::with_opener(
        Rc::clone(&panes) as Rc<dyn SharedPaneOpener>,
        &PaneConstruction::testing(),
    );
    (sessions, panes, factory)
}

fn model(server: &Rc<DelayedServer>) -> (SharedEditSource, SharedWorkspaces) {
    let source = SharedEditSource::new(Rc::clone(server) as Rc<dyn SharedWorkspaceEdits>);
    let (sessions, _, _) = sessions();
    let mut model = SharedWorkspaces::new(source.clone(), sessions);
    let inventory = model.poll().inventory.unwrap();
    model.register(
        first_workspace_id(),
        SharedWorkspaceFacts::from_server(&inventory.workspaces[0]),
    );
    (source, model)
}

#[test]
fn optimistic_rapid_identical_actions_keep_distinct_server_identities_and_results() {
    let server = DelayedServer::new();
    let (source, mut model) = model(&server);
    let workspace = server.inventory().workspaces[0].id;
    let mutation = Mutation::CreateTab {
        workspace,
        source: None,
    };
    let first = source.submit_waiting(mutation.clone()).unwrap();
    let second = source.submit_waiting(mutation).unwrap();
    assert_eq!(server.dispatched().len(), 1);
    let projected = model.poll().inventory.unwrap();
    assert_eq!(
        projected.workspaces[0].tabs.len(),
        3,
        "both interactions have a frame before either answer"
    );
    assert!(first.try_recv().is_err());
    assert!(second.try_recv().is_err());

    let accepted_first = server.finish(true, true);
    model.poll();
    assert_eq!(first.try_recv().unwrap().unwrap(), accepted_first);
    assert!(
        second.try_recv().is_err(),
        "identical mutations still have separate receivers"
    );
    assert_eq!(server.dispatched().len(), 2);
    let accepted_second = server.finish(true, true);
    model.poll();
    assert_eq!(second.try_recv().unwrap().unwrap(), accepted_second);
    assert_ne!(accepted_first.outcome, accepted_second.outcome);
    assert_eq!(server.inventory().workspaces[0].tabs.len(), 3);
}

#[test]
fn optimistic_split_of_a_pending_tab_dispatches_only_after_its_real_parent_exists() {
    let server = DelayedServer::new();
    let (source, mut model) = model(&server);
    let workspace = server.inventory().workspaces[0].id;
    assert_eq!(
        source.submit(Mutation::CreateTab {
            workspace,
            source: None
        }),
        SharedSubmission::Submitted
    );
    let projected = source.presentation_inventory().unwrap();
    let pending = projected.workspaces[0].tabs.last().unwrap();
    let provisional_tab = pending.id;
    let provisional_pane = pending.root_pane;
    assert_eq!(
        source.submit(Mutation::SplitPane {
            workspace,
            tab: provisional_tab,
            source: provisional_pane,
            axis: Axis::Down
        }),
        SharedSubmission::Submitted
    );
    assert_eq!(
        source.presentation_inventory().unwrap().workspaces[0]
            .tabs
            .last()
            .unwrap()
            .panes
            .len(),
        2
    );
    assert_eq!(
        server.dispatched().len(),
        1,
        "no provisional identity can cross dispatch"
    );

    let created = server.finish(true, true);
    model.poll();
    let Outcome::Accepted { target } = created.outcome else {
        panic!("the server must accept the Tab");
    };
    let real = server.inventory().workspaces[0]
        .tabs
        .iter()
        .find(|t| t.id == target)
        .unwrap()
        .clone();
    assert_eq!(
        server.dispatched()[1],
        Mutation::SplitPane {
            workspace,
            tab: real.id,
            source: real.root_pane,
            axis: Axis::Down
        }
    );
    assert_ne!(real.id, provisional_tab);
    server.finish(true, true);
    model.poll();
    assert_eq!(
        server.inventory().workspaces[0]
            .tabs
            .last()
            .unwrap()
            .panes
            .len(),
        2
    );
}

#[test]
fn optimistic_refusal_removes_dependents_and_preserves_unrelated_later_work() {
    let server = DelayedServer::new();
    let (source, mut model) = model(&server);
    let workspace = server.inventory().workspaces[0].id;
    source.submit(Mutation::CreateTab {
        workspace,
        source: None,
    });
    let projected = source.presentation_inventory().unwrap();
    let tab = projected.workspaces[0].tabs.last().unwrap();
    let dependent = source
        .submit_waiting(Mutation::SplitPane {
            workspace,
            tab: tab.id,
            source: tab.root_pane,
            axis: Axis::Right,
        })
        .unwrap();
    source.submit(Mutation::RenameWorkspace {
        workspace,
        version: 0,
        name: "Later work".into(),
    });
    assert_eq!(server.finish(true, false).outcome, Outcome::Invalid);
    let rollback = model.poll().inventory.unwrap();
    assert_eq!(rollback.workspaces[0].tabs.len(), 1);
    assert_eq!(rollback.workspaces[0].name, "Later work");
    assert!(
        dependent.try_recv().unwrap().is_err(),
        "the refused parent cannot silently drop its split"
    );
    assert_eq!(server.dispatched().len(), 2);
    assert!(
        matches!(&server.dispatched()[1], Mutation::RenameWorkspace { name, .. } if name == "Later work")
    );
    server.finish(true, true);
    model.poll();
    assert_eq!(server.inventory().workspaces[0].name, "Later work");
}

#[test]
fn optimistic_conflict_preserves_its_snapshot_and_draft_beside_a_later_pin() {
    let server = DelayedServer::new();
    let (source, mut model) = model(&server);
    let workspace = server.inventory().workspaces[0].id;
    source.submit(Mutation::RenameWorkspace {
        workspace,
        version: 0,
        name: "My draft".into(),
    });
    source.submit(Mutation::PinWorkspace {
        workspace,
        version: 0,
        directory: Some("/fixture/later".into()),
    });
    let pending = model.poll().inventory.unwrap();
    assert_eq!(pending.workspaces[0].name, "My draft");
    assert_eq!(
        pending.workspaces[0].pinned_directory,
        Some("/fixture/later".into())
    );
    server.rename_elsewhere(workspace, "Their name");
    assert_eq!(server.finish(true, true).outcome, Outcome::Conflict);
    let restored = model.poll().inventory.unwrap();
    assert_eq!(restored.workspaces[0].name, "Their name");
    assert_eq!(
        restored.workspaces[0].pinned_directory,
        Some("/fixture/later".into())
    );
    let conflict = model.take_conflict().unwrap();
    assert_eq!(conflict.authoritative.as_ref(), "Their name");
    assert_eq!(
        conflict.draft,
        SharedWorkspaceDraft::Name("My draft".into())
    );
}

#[test]
fn optimistic_lost_ack_binds_committed_membership_and_keeps_the_original() {
    let server = DelayedServer::new();
    let (source, mut model) = model(&server);
    let workspace = server.inventory().workspaces[0].id;
    source.submit(Mutation::CreateTab {
        workspace,
        source: None,
    });
    model.poll();
    let committed = server.finish(false, true);
    let binding = model.poll();
    assert!(
        binding.accepted.is_none(),
        "an acknowledgement cannot reopen an earlier creation"
    );
    assert_eq!(
        binding.inventory.unwrap().workspaces[0].tabs.len(),
        2,
        "never render the real and provisional Tab together"
    );
    assert!(
        server.0.borrow().original.is_some(),
        "the validated outcome has not retired its Original Operation"
    );
    source.submit(Mutation::CreateTab {
        workspace,
        source: None,
    });
    assert_eq!(
        server.dispatched().len(),
        1,
        "the transport still owns the unacknowledged original"
    );
    server.connected(false);
    model.poll();
    assert!(!source.has_pending(), "unsent work is not an offline queue");
    server.connected(true);
    assert_eq!(
        server.finish(true, true),
        committed,
        "resolution uses the same operation identity"
    );
    model.poll();
    assert_eq!(server.dispatched().len(), 1);
    assert_eq!(server.inventory().workspaces[0].tabs.len(), 2);
}

#[test]
fn optimistic_queue_is_bounded_and_offline_submissions_never_enter_it() {
    let server = DelayedServer::new();
    let (source, _) = model(&server);
    let workspace = server.inventory().workspaces[0].id;
    for _ in 0..32 {
        assert_eq!(
            source.submit(Mutation::CreateTab {
                workspace,
                source: None
            }),
            SharedSubmission::Submitted
        );
    }
    assert_eq!(
        source.submit(Mutation::CreateTab {
            workspace,
            source: None
        }),
        SharedSubmission::Refused(SharedRefusalReason::Pending)
    );
    assert_eq!(server.dispatched().len(), 1);
    server.connected(false);
    assert_eq!(
        source.submit(Mutation::CreateTab {
            workspace,
            source: None
        }),
        SharedSubmission::Refused(SharedRefusalReason::Offline)
    );
    assert_eq!(
        source.presentation_inventory().unwrap().workspaces[0]
            .tabs
            .len(),
        2,
        "only the dispatched Original Operation survives loss"
    );
}

struct WindowHarness {
    manager: Entity<TabManager>,
    server: Rc<DelayedServer>,
    source: SharedEditSource,
    panes: Rc<ScriptedPanes>,
    model: RefCell<SharedWorkspaces>,
}
fn window_harness(cx: &mut TestAppContext) -> (WindowHarness, &mut VisualTestContext) {
    cx.update(crate::ui::init).unwrap();
    let server = DelayedServer::new();
    let inventory = server.inventory();
    let workspace = &inventory.workspaces[0];
    let root = &workspace.tabs[0];
    let source = SharedEditSource::new(Rc::clone(&server) as Rc<dyn SharedWorkspaceEdits>);
    let (sessions, panes, factory) = sessions();
    let (_, state) = panes.open(root.root_pane).unwrap();
    let mut model = SharedWorkspaces::new(source.clone(), sessions.clone());
    model.poll();
    model.register(
        first_workspace_id(),
        SharedWorkspaceFacts::from_server(workspace),
    );
    let (manager, cx) = cx.add_window_view(|window, cx| TabManager::new(factory, window, cx));
    cx.update(|window, cx| {
        window.activate_window();
        manager.update(cx, |manager, cx| {
            manager.adopt_shared(
                SharedTabs::new(source.clone(), sessions, workspace.id),
                root.id,
                root.root_pane,
                state,
                window,
                cx,
            );
            manager.focus(window, cx);
        });
    });
    cx.run_until_parked();
    (
        WindowHarness {
            manager,
            server,
            source,
            panes,
            model: RefCell::new(model),
        },
        cx,
    )
}
fn project_window(harness: &WindowHarness, cx: &mut VisualTestContext) {
    harness.model.borrow_mut().poll();
    let projected = harness.source.presentation_inventory().unwrap();
    cx.update(|window, cx| {
        harness.manager.update(cx, |manager, cx| {
            manager.reconcile_shared(
                &projected.workspaces[0],
                super::super::development::SharedRemoval::Published,
                window,
                cx,
            );
        });
        window.refresh();
    });
    cx.run_until_parked();
}
fn local_view(
    harness: &WindowHarness,
    cx: &mut VisualTestContext,
) -> (crate::domain::TabId, crate::domain::PaneId) {
    harness
        .manager
        .read_with(cx, |manager, cx| manager.active_terminal_identity(cx))
}

#[gpui::test]
fn optimistic_pending_tab_split_and_later_navigation_keep_the_frame_and_keyboard(
    cx: &mut TestAppContext,
) {
    let (harness, cx) = window_harness(cx);
    let original = local_view(&harness, cx);
    click("create-tab-button", cx);
    let provisional = local_view(&harness, cx);
    assert_ne!(provisional.0, original.0);
    click("pane-split-down-1", cx);
    assert_eq!(
        harness.manager.read_with(cx, |manager, cx| manager
            .active_tab_view()
            .read(cx)
            .pane_count()),
        2
    );
    assert_eq!(harness.server.dispatched().len(), 1);
    assert_eq!(
        harness.panes.opened().len(),
        1,
        "provisional Panes subscribe to no server identity or input authority"
    );
    cx.dispatch_action(crate::ui::PreviousTab);
    cx.run_until_parked();
    // A later Tab command owns the selection before either server completion arrives.
    let navigated = local_view(&harness, cx);
    assert_eq!(navigated.0, original.0);
    let focus = cx.update(|window, cx| window.focused(cx));
    harness.server.finish(true, true);
    project_window(&harness, cx);
    assert_eq!(local_view(&harness, cx), navigated);
    assert_eq!(cx.update(|window, cx| window.focused(cx)), focus);
    harness.server.finish(true, true);
    project_window(&harness, cx);
    assert_eq!(local_view(&harness, cx), navigated);
    assert_eq!(cx.update(|window, cx| window.focused(cx)), focus);
    assert!(cx.debug_bounds("shared-tab-unavailable-ok").is_none());
}

#[gpui::test]
fn optimistic_close_hides_before_a_doomed_pane_status_and_restores_the_same_view_on_refusal(
    cx: &mut TestAppContext,
) {
    let (harness, cx) = window_harness(cx);
    click("pane-split-down-1", cx);
    harness.server.finish(true, true);
    project_window(&harness, cx);
    let closed_view = local_view(&harness, cx);
    let inventory = harness.server.inventory();
    let workspace = &inventory.workspaces[0];
    let tab = &workspace.tabs[0];
    let closing = tab.panes.last().unwrap().id;
    let target = spaceterm_server::hierarchy::CloseTarget::Pane {
        workspace: workspace.id,
        tab: tab.id,
        pane: closing,
    };
    let approval = harness.model.borrow().close_approval(target).unwrap();
    assert_eq!(
        harness.model.borrow_mut().close(
            approval,
            crate::close_confirmation::CloseTarget::Pane {
                workspace_id: first_workspace_id(),
                tab_id: closed_view.0,
                pane_id: closed_view.1,
            }
        ),
        SharedSubmission::Submitted
    );
    project_window(&harness, cx);
    assert_eq!(
        harness.manager.read_with(cx, |manager, cx| manager
            .active_tab_view()
            .read(cx)
            .pane_count()),
        1,
        "the first post-consent frame has already removed the Pane"
    );
    harness
        .panes
        .pane(closing)
        .set(DevelopmentPaneStatus::TakeoverReconnecting);
    cx.executor()
        .advance_clock(crate::ui::shared_workspaces::POLL_INTERVAL * 3);
    cx.run_until_parked();
    assert!(cx.debug_bounds("shared-pane-takeover-2").is_none());
    assert!(cx.debug_bounds("shared-pane-notice-2").is_none());
    // A changed server confirmation definitely refuses the original close.
    harness.server.0.borrow_mut().server.inventory.close = Some(CloseInventory {
        boot: [99; 32],
        sessions: vec![],
    });
    assert_eq!(
        harness.server.finish(true, true).outcome,
        Outcome::StaleConfirmation
    );
    let focus = cx.update(|window, cx| window.focused(cx));
    project_window(&harness, cx);
    assert_eq!(
        harness.manager.read_with(cx, |manager, cx| manager
            .active_tab_view()
            .read(cx)
            .pane_count()),
        2
    );
    assert_eq!(
        cx.update(|window, cx| window.focused(cx)),
        focus,
        "rollback never steals the survivor's keyboard"
    );
    // The restored Pane retains its original local identity and layout, not a replacement Pane.
    assert!(
        cx.debug_bounds(format!("pane-surface-{}", closed_view.1.get()).leak())
            .is_some()
    );
}

#[test]
fn optimistic_workspace_close_keeps_its_host_through_a_lost_ack() {
    let server = DelayedServer::new();
    let (_, mut model) = model(&server);
    let workspace = server.inventory().workspaces[0].id;
    let target = spaceterm_server::hierarchy::CloseTarget::Workspace { workspace };
    let approval = model.close_approval(target).unwrap();
    assert_eq!(
        model.close(
            approval,
            crate::close_confirmation::CloseTarget::Workspace(first_workspace_id())
        ),
        SharedSubmission::Submitted
    );
    assert!(model.optimistically_closing(first_workspace_id()));
    assert!(model.poll().inventory.unwrap().workspaces.is_empty());
    server.finish(false, true);
    model.poll();
    assert!(model.latest_inventory().unwrap().workspaces.is_empty());
    assert!(
        model.optimistically_closing(first_workspace_id()),
        "published deletion does not retire an unacknowledged Original Operation"
    );
    server.connected(false);
    model.poll();
    assert!(model.optimistically_closing(first_workspace_id()));
    server.connected(true);
    server.finish(true, true);
    model.poll();
    assert!(!model.optimistically_closing(first_workspace_id()));
}

#[gpui::test]
fn a_committed_original_keeps_resolving_without_a_workspace_window(cx: &mut TestAppContext) {
    let server = DelayedServer::new();
    let (source, mut model) = model(&server);
    let workspace = server.inventory().workspaces[0].id;
    let approval = model
        .close_approval(spaceterm_server::hierarchy::CloseTarget::Workspace { workspace })
        .unwrap();
    model.close(
        approval,
        crate::close_confirmation::CloseTarget::Workspace(first_workspace_id()),
    );
    server.finish(false, true);
    model.poll();
    cx.update(|cx| source.retain_until_settled(cx));
    let retained = Rc::downgrade(&server);
    drop(model);
    drop(source);
    drop(server);
    cx.run_until_parked();
    let original = retained
        .upgrade()
        .expect("the unacknowledged Original Operation stays owned");
    assert_eq!(original.dispatched().len(), 1);
    original.finish(true, true);
    drop(original);
    cx.background_executor
        .advance_clock(crate::ui::shared_workspaces::POLL_INTERVAL * 3);
    cx.run_until_parked();
    assert!(
        retained.upgrade().is_none(),
        "settlement releases the background owner"
    );
}

#[gpui::test]
fn successive_refused_tab_closes_restore_the_original_order(cx: &mut TestAppContext) {
    let (harness, cx) = window_harness(cx);
    for _ in 0..3 {
        click("create-tab-button", cx);
        harness.server.finish(true, true);
        project_window(&harness, cx);
    }
    let original = harness
        .manager
        .read_with(cx, |manager, _| manager.tab_ids());
    let inventory = harness.server.inventory();
    let workspace = &inventory.workspaces[0];
    for index in [1, 2] {
        let approval = harness
            .model
            .borrow()
            .close_approval(spaceterm_server::hierarchy::CloseTarget::Tab {
                workspace: workspace.id,
                tab: workspace.tabs[index].id,
            })
            .unwrap();
        assert_eq!(
            harness.model.borrow_mut().close(
                approval,
                crate::close_confirmation::CloseTarget::Tab {
                    workspace_id: first_workspace_id(),
                    tab_id: original[index],
                },
            ),
            SharedSubmission::Submitted
        );
        project_window(&harness, cx);
    }
    for _ in 0..2 {
        harness.server.0.borrow_mut().server.inventory.close = Some(CloseInventory {
            boot: [99; 32],
            sessions: vec![],
        });
        assert_eq!(
            harness.server.finish(true, true).outcome,
            Outcome::StaleConfirmation
        );
        project_window(&harness, cx);
    }
    assert_eq!(
        harness
            .manager
            .read_with(cx, |manager, _| manager.tab_ids()),
        original
    );
}

#[gpui::test]
fn optimistic_tab_close_ack_keeps_later_find_focus(cx: &mut TestAppContext) {
    let (harness, cx) = window_harness(cx);
    click("create-tab-button", cx);
    harness.server.finish(true, true);
    project_window(&harness, cx);
    let closed = local_view(&harness, cx);
    let inventory = harness.server.inventory();
    let workspace = &inventory.workspaces[0];
    let tab = workspace.tabs.last().unwrap();
    let doomed_pane = tab.root_pane;
    let target = spaceterm_server::hierarchy::CloseTarget::Tab {
        workspace: workspace.id,
        tab: tab.id,
    };
    let approval = harness.model.borrow().close_approval(target).unwrap();
    assert_eq!(
        harness.model.borrow_mut().close(
            approval,
            crate::close_confirmation::CloseTarget::Tab {
                workspace_id: first_workspace_id(),
                tab_id: closed.0,
            }
        ),
        SharedSubmission::Submitted
    );
    project_window(&harness, cx);
    assert_eq!(
        harness
            .manager
            .read_with(cx, |manager, _| manager.tab_ids().len()),
        1
    );
    assert!(
        cx.debug_bounds(format!("tab-identity-{}", closed.0.get()).leak())
            .is_none()
    );

    cx.dispatch_action(crate::ui::OpenTerminalFind);
    cx.run_until_parked();
    assert!(cx.debug_bounds("terminal-find-input").is_some());
    let find_focus = cx.update(|window, cx| window.focused(cx));
    harness
        .panes
        .pane(doomed_pane)
        .set(DevelopmentPaneStatus::TakeoverReconnecting);
    cx.executor()
        .advance_clock(crate::ui::shared_workspaces::POLL_INTERVAL * 3);
    cx.run_until_parked();
    assert!(cx.debug_bounds("shared-pane-takeover-1").is_none());
    assert!(cx.debug_bounds("shared-pane-notice-1").is_none());
    assert!(matches!(
        harness.server.finish(true, true).outcome,
        Outcome::Accepted { .. }
    ));
    project_window(&harness, cx);
    assert_eq!(cx.update(|window, cx| window.focused(cx)), find_focus);
    assert!(cx.debug_bounds("terminal-find-input").is_some());
    assert_eq!(
        harness
            .manager
            .read_with(cx, |manager, _| manager.tab_ids().len()),
        1
    );
}

#[test]
fn optimistic_field_presentation_never_renews_an_unseen_conflicting_version() {
    let server = DelayedServer::new();
    let (_, mut model) = model(&server);
    let local = first_workspace_id();
    let workspace = server.inventory().workspaces[0].id;
    model.rename(local, "First draft".into(), Some(0));
    server.rename_elsewhere(workspace, "Their name");
    let projection = model.poll().inventory.unwrap();
    assert_eq!(projection.workspaces[0].name, "First draft");
    assert_eq!(
        projection.workspaces[0].name_version, 0,
        "an optimistic value still carries the version its editor actually observed"
    );
    model.register(
        local,
        SharedWorkspaceFacts::from_server(&projection.workspaces[0]),
    );
    let observed = model.facts(local).unwrap().name_version;
    model.rename(local, "Later draft".into(), Some(observed));
    assert_eq!(server.finish(true, true).outcome, Outcome::Conflict);
    model.poll();
    assert_eq!(server.finish(true, true).outcome, Outcome::Conflict);
    model.poll();
    assert_eq!(server.inventory().workspaces[0].name, "Their name");
    assert_eq!(
        model.take_conflict().unwrap().draft,
        SharedWorkspaceDraft::Name("Later draft".into())
    );
}
