//! Shared server Workspaces driven through the main Workspace interface.
//!
//! Every test here goes through the public surfaces a person uses: the Tab strip, the Pane
//! Caption, the Pane itself, and the server inventory the window reads. None of them reaches into
//! presentation state, so each one keeps holding as the implementation changes.

#[path = "interaction_tests.rs"]
mod interaction_tests;

use std::path::PathBuf;
use std::rc::Rc;

use gpui::{Entity, Modifiers, TestAppContext, VisualTestContext};
use spaceterm_server::hierarchy::{
    Axis, CloseInventory, ConfirmedPane, Id, Inventory, Mutation, Outcome, Pane, Tab, Workspace,
};

use super::development::testing::{ScriptedEdits, ScriptedPanes};
use super::development::{
    SharedEditSource, SharedEditability, SharedPaneOpener, SharedPaneSessions, SharedRefusalReason,
    SharedSubmission, SharedTabs, SharedWorkspaceDraft, SharedWorkspaceEdits, SharedWorkspaceFacts,
    SharedWorkspaceField, SharedWorkspacePlan, SharedWorkspaces,
};
use crate::terminal::server_session::DevelopmentPaneStatus;
use crate::terminal::{TerminalSessionFactory, WorkspaceTerminalSessionFactory};
use crate::ui::pane_lifecycle::PaneConstruction;
use crate::ui::tab_manager::TabManager;

const WORKSPACE: Id = [1; 32];
const ROOT_TAB: Id = [2; 32];
const ROOT_PANE: Id = [3; 32];
const SECOND_TAB: Id = [4; 32];
const SECOND_TAB_PANE: Id = [5; 32];
const SPLIT_PANE: Id = [6; 32];
const REPLACEMENT_PANE: Id = [7; 32];
const SECOND_REPLACEMENT_PANE: Id = [8; 32];

/// The first Workspace a collection owns, which every harness here binds the server Workspace to.
fn first_workspace_id() -> crate::domain::WorkspaceId {
    crate::domain::workspace_collection::tests::local_collection(
        crate::terminal::testing::test_local_directory(PathBuf::from("/tmp/spaceterm-shared")),
        |_, _| (),
    )
    .active_workspace_id()
    .expect("a new collection presents its first Workspace")
}

fn pane(id: Id, source: Option<Id>, axis: Axis) -> Pane {
    Pane {
        id,
        source,
        axis,
        starting_directory: PathBuf::from("/server/work"),
        current_directory: None,
        repository_directory: None,
        presentation: None,
        confirmed_directory: None,
        creator: None,
    }
}

fn tab(id: Id, panes: Vec<Pane>) -> Tab {
    Tab {
        worktree: None,
        worktree_membership: spaceterm_server::hierarchy::WorktreeMembership::Following,
        worktree_repository: None,
        id,
        root_pane: panes[0].id,
        panes,
    }
}

/// The server boot every published close fact here belongs to.
const BOOT: Id = [9; 32];

/// One server Workspace with `tabs`, whose Panes have no running shell yet.
fn inventory(revision: u64, tabs: Vec<Tab>) -> Inventory {
    let sessions = tabs
        .iter()
        .flat_map(|tab| &tab.panes)
        .map(|pane| ConfirmedPane {
            id: pane.id,
            session: None,
        })
        .collect();
    Inventory {
        repositories: None,
        revision,
        close: Some(CloseInventory {
            controls: Vec::new(),
            boot: BOOT,
            sessions,
        }),
        workspaces: vec![Workspace {
            id: WORKSPACE,
            name: "Build".to_owned(),
            name_version: 0,
            pin_version: 0,
            pinned_directory: None,
            root_tab: ROOT_TAB,
            tabs,
        }],
    }
}

fn root_only() -> Inventory {
    inventory(
        1,
        vec![tab(ROOT_TAB, vec![pane(ROOT_PANE, None, Axis::Right)])],
    )
}

struct SharedHarness {
    manager: Entity<TabManager>,
    edits: Rc<ScriptedEdits>,
    panes: Rc<ScriptedPanes>,
}

/// A Tab strip presenting one server Workspace's Root Tab, as the Workspace Window binds it.
fn shared_tab_manager(cx: &mut TestAppContext) -> (SharedHarness, &mut VisualTestContext) {
    cx.update(crate::ui::init)
        .expect("UI initialization should succeed");
    let session_factory: Rc<dyn TerminalSessionFactory> =
        Rc::new(crate::terminal::testing::TestTerminalSessionFactory::new(
            crate::terminal::testing::TestTerminalSessionRecords::default(),
        ));
    let session_factory = WorkspaceTerminalSessionFactory::new_local(
        session_factory,
        crate::terminal::testing::test_local_directory(PathBuf::from("/tmp/spaceterm-shared-test")),
    );
    let panes = ScriptedPanes::new(session_factory.clone());
    let sessions = SharedPaneSessions::with_opener(
        Rc::clone(&panes) as Rc<dyn SharedPaneOpener>,
        &PaneConstruction::testing(),
    );
    let edits = Rc::new(ScriptedEdits::new(Some(root_only())));
    let source = SharedEditSource::new(Rc::clone(&edits) as Rc<dyn SharedWorkspaceEdits>);
    let (manager, cx) =
        cx.add_window_view(|window, cx| TabManager::new(session_factory.clone(), window, cx));
    cx.update(|window, cx| {
        window.activate_window();
        let (_, state) = panes
            .open(ROOT_PANE)
            .expect("the Root Pane subscription must open");
        manager.update(cx, |manager, cx| {
            manager.adopt_shared(
                SharedTabs::new(source, sessions, WORKSPACE),
                ROOT_TAB,
                ROOT_PANE,
                state,
                window,
                cx,
            );
            manager.focus(window, cx);
        });
    });
    cx.run_until_parked();
    (
        SharedHarness {
            manager,
            edits,
            panes,
        },
        cx,
    )
}

fn publish(harness: &SharedHarness, inventory: Inventory, cx: &mut VisualTestContext) {
    let workspace = inventory.workspaces[0].clone();
    harness.edits.set_inventory(inventory);
    cx.update(|window, cx| {
        harness.manager.update(cx, |manager, cx| {
            manager.reconcile_shared(
                &workspace,
                super::development::SharedRemoval::Published,
                window,
                cx,
            );
        });
    });
    cx.run_until_parked();
}

fn tab_count(harness: &SharedHarness, cx: &mut VisualTestContext) -> usize {
    harness
        .manager
        .read_with(cx, |manager, _| manager.tab_ids().len())
}

fn pane_count(harness: &SharedHarness, cx: &mut VisualTestContext) -> usize {
    harness.manager.read_with(cx, |manager, cx| {
        manager.active_tab_view().read(cx).pane_count()
    })
}

/// Lets every Pane read its connection and paints the frame that follows.
fn settle(_: &SharedHarness, cx: &mut VisualTestContext) {
    cx.run_until_parked();
    cx.update(|window, _| window.refresh());
    cx.run_until_parked();
}

/// Opens a row's context menu, which leaves the selection and focus where they were.
fn right_click(selector: &'static str, cx: &mut VisualTestContext) {
    let position = cx
        .debug_bounds(selector)
        .map(|bounds| bounds.center())
        .unwrap_or_else(|| panic!("{selector} was not rendered"));
    cx.simulate_mouse_move(position, None, Modifiers::none());
    cx.simulate_mouse_down(position, gpui::MouseButton::Right, Modifiers::none());
    cx.simulate_mouse_up(position, gpui::MouseButton::Right, Modifiers::none());
    cx.run_until_parked();
}

fn click(selector: &'static str, cx: &mut VisualTestContext) {
    let position = cx
        .debug_bounds(selector)
        .map(|bounds| bounds.center())
        .unwrap_or_else(|| panic!("{selector} was not rendered"));
    cx.simulate_mouse_move(position, None, Modifiers::none());
    cx.simulate_click(position, Modifiers::none());
    cx.run_until_parked();
}

#[gpui::test]
fn new_tab_should_present_provisionally_while_the_server_validates_it(cx: &mut TestAppContext) {
    let (harness, cx) = shared_tab_manager(cx);
    assert_eq!(tab_count(&harness, cx), 1);

    click("create-tab-button", cx);

    assert_eq!(
        harness.edits.submitted(),
        vec![Mutation::CreateTab {
            workspace: WORKSPACE,
            source: Some(ROOT_PANE),
        }],
        "New Tab should ask the server"
    );
    assert_eq!(
        tab_count(&harness, cx),
        2,
        "the initiating interaction presents its provisional Tab at once"
    );
}

#[gpui::test]
fn a_published_tab_should_join_the_strip_without_taking_the_active_tab(cx: &mut TestAppContext) {
    let (harness, cx) = shared_tab_manager(cx);
    let active = harness
        .manager
        .read_with(cx, |manager, cx| manager.active_terminal_identity(cx).0);

    publish(
        &harness,
        inventory(
            2,
            vec![
                tab(ROOT_TAB, vec![pane(ROOT_PANE, None, Axis::Right)]),
                tab(SECOND_TAB, vec![pane(SECOND_TAB_PANE, None, Axis::Right)]),
            ],
        ),
        cx,
    );

    assert_eq!(tab_count(&harness, cx), 2, "the published Tab should join");
    assert_eq!(
        harness
            .manager
            .read_with(cx, |manager, cx| manager.active_terminal_identity(cx).0),
        active,
        "another client's Tab must not take the Active Tab"
    );
    assert_eq!(
        harness.panes.pane(SECOND_TAB_PANE).selections(),
        0,
        "presenting a published Tab must not ask the server for its Pane"
    );
}

#[gpui::test]
fn a_published_split_should_join_the_layout_without_taking_focus(cx: &mut TestAppContext) {
    let (harness, cx) = shared_tab_manager(cx);
    let focused = harness
        .manager
        .read_with(cx, |manager, cx| manager.active_terminal_identity(cx).1);

    publish(
        &harness,
        inventory(
            2,
            vec![tab(
                ROOT_TAB,
                vec![
                    pane(ROOT_PANE, None, Axis::Right),
                    pane(SPLIT_PANE, Some(ROOT_PANE), Axis::Down),
                ],
            )],
        ),
        cx,
    );

    assert_eq!(
        pane_count(&harness, cx),
        2,
        "the published Pane should join"
    );
    assert_eq!(
        harness
            .manager
            .read_with(cx, |manager, cx| manager.active_terminal_identity(cx).1),
        focused,
        "another client's Pane must not take the Focused Pane"
    );
    assert_eq!(
        harness.panes.pane(SPLIT_PANE).selections(),
        0,
        "presenting a published Pane must not ask the server for it"
    );
}

#[gpui::test]
fn a_split_in_a_shared_workspace_should_ask_the_server_for_the_pane(cx: &mut TestAppContext) {
    let (harness, cx) = shared_tab_manager(cx);

    click("pane-split-down-1", cx);

    assert_eq!(
        harness.edits.submitted(),
        vec![Mutation::SplitPane {
            workspace: WORKSPACE,
            tab: ROOT_TAB,
            source: ROOT_PANE,
            axis: Axis::Down,
        }],
        "Split Down should ask the server"
    );
    assert_eq!(
        pane_count(&harness, cx),
        2,
        "the initiating interaction presents its provisional Pane at once"
    );
}

#[gpui::test]
fn shared_edits_should_be_unavailable_offline_rather_than_queued(cx: &mut TestAppContext) {
    let (harness, cx) = shared_tab_manager(cx);
    harness.edits.set_connected(false);
    cx.update(|window, _| window.refresh());
    cx.run_until_parked();

    click("create-tab-button", cx);
    click("pane-split-right-1", cx);

    assert!(
        harness.edits.submitted().is_empty(),
        "an offline client should submit nothing and queue nothing"
    );
    assert_eq!(tab_count(&harness, cx), 1);
    assert_eq!(pane_count(&harness, cx), 1);
}

#[gpui::test]
fn a_shared_pane_should_refuse_its_own_local_close(cx: &mut TestAppContext) {
    let (harness, cx) = shared_tab_manager(cx);
    publish(
        &harness,
        inventory(
            2,
            vec![tab(
                ROOT_TAB,
                vec![
                    pane(ROOT_PANE, None, Axis::Right),
                    pane(SPLIT_PANE, Some(ROOT_PANE), Axis::Down),
                ],
            )],
        ),
        cx,
    );
    assert_eq!(pane_count(&harness, cx), 2);

    click("pane-close-1", cx);

    assert_eq!(
        pane_count(&harness, cx),
        2,
        "closing a shared Pane needs every client's confirmation, so it closes none locally"
    );
}

#[gpui::test]
fn removed_membership_should_leave_no_desktop_owned_remains(cx: &mut TestAppContext) {
    let (harness, cx) = shared_tab_manager(cx);
    publish(
        &harness,
        inventory(
            2,
            vec![
                tab(ROOT_TAB, vec![pane(ROOT_PANE, None, Axis::Right)]),
                tab(SECOND_TAB, vec![pane(SECOND_TAB_PANE, None, Axis::Right)]),
            ],
        ),
        cx,
    );
    assert_eq!(tab_count(&harness, cx), 2);

    // The second Tab's Pane never connected, so only the published membership can remove it.
    publish(
        &harness,
        inventory(
            3,
            vec![tab(ROOT_TAB, vec![pane(ROOT_PANE, None, Axis::Right)])],
        ),
        cx,
    );

    assert_eq!(
        tab_count(&harness, cx),
        1,
        "a Tab the server removed should leave the strip"
    );
}

#[gpui::test]
fn a_publication_that_replaces_every_known_pane_should_keep_the_tab(cx: &mut TestAppContext) {
    let (harness, cx) = shared_tab_manager(cx);
    assert_eq!(pane_count(&harness, cx), 1);

    // Between two readings the server split the Root Pane twice and the Root Pane's shell exited.
    // Removing it first names no Pane the replacements were split from, so the server clears
    // their creation links and the Tab would lose every Pane it has.
    publish(
        &harness,
        inventory(
            2,
            vec![tab(
                ROOT_TAB,
                vec![
                    pane(REPLACEMENT_PANE, None, Axis::Right),
                    pane(SECOND_REPLACEMENT_PANE, None, Axis::Down),
                ],
            )],
        ),
        cx,
    );

    assert_eq!(tab_count(&harness, cx), 1, "the server's Tab should remain");
    assert_eq!(
        pane_count(&harness, cx),
        2,
        "both published Panes should take their place beside this client's own Root Pane"
    );
}

#[gpui::test]
fn a_shared_pane_whose_shell_exits_should_keep_its_place_until_the_server_publishes(
    cx: &mut TestAppContext,
) {
    let (harness, cx) = shared_tab_manager(cx);

    // The Terminal Session ends before the next reading of the inventory. The server may have
    // created a Pane this client has not seen, so membership waits for what the server publishes.
    harness
        .panes
        .pane(ROOT_PANE)
        .set(DevelopmentPaneStatus::Ended);
    cx.update(|_, cx| {
        harness.manager.update(cx, |manager, cx| {
            manager.exit_shared_pane_for_test(cx);
        });
    });
    cx.run_until_parked();

    assert_eq!(
        tab_count(&harness, cx),
        1,
        "an exit this client heard first must not close a Tab the server still has"
    );
    assert_eq!(pane_count(&harness, cx), 1);

    publish(
        &harness,
        inventory(
            2,
            vec![tab(
                ROOT_TAB,
                vec![pane(REPLACEMENT_PANE, None, Axis::Right)],
            )],
        ),
        cx,
    );

    assert_eq!(
        pane_count(&harness, cx),
        1,
        "the published inventory replaces the Pane that ended"
    );
    assert_eq!(tab_count(&harness, cx), 1);
}

#[gpui::test]
fn focus_a_shared_pane_regains_should_not_ask_the_server_for_it(cx: &mut TestAppContext) {
    let (harness, cx) = shared_tab_manager(cx);
    let selections = harness.panes.pane(ROOT_PANE).selections();

    // Window restoration refocuses the Pane's terminal, which notifies the Tab the same way a
    // person's click does. Only the click is an explicit selection.
    cx.update(|window, cx| {
        harness.manager.update(cx, |manager, cx| {
            manager.focus(window, cx);
        });
    });
    cx.run_until_parked();

    assert_eq!(
        harness.panes.pane(ROOT_PANE).selections(),
        selections,
        "focus the Pane regains must not start a retained shell"
    );
}

#[gpui::test]
fn opening_a_shared_workspace_should_ask_the_server_for_its_focused_pane(cx: &mut TestAppContext) {
    let (harness, cx) = shared_tab_manager(cx);
    publish(
        &harness,
        inventory(
            2,
            vec![tab(
                ROOT_TAB,
                vec![
                    pane(ROOT_PANE, None, Axis::Right),
                    pane(SPLIT_PANE, Some(ROOT_PANE), Axis::Down),
                ],
            )],
        ),
        cx,
    );
    assert_eq!(harness.panes.pane(ROOT_PANE).selections(), 0);
    assert_eq!(harness.panes.pane(SPLIT_PANE).selections(), 0);

    // Opening this Workspace from the sidebar or the switcher hands it the window the Workspace
    // before it had, which is an explicit opening of its Active Tab.
    cx.update(|window, cx| {
        harness.manager.update(cx, |manager, cx| {
            manager.deactivate(cx);
            manager.activate(window, cx);
        });
    });
    cx.run_until_parked();

    assert_eq!(
        harness.panes.pane(ROOT_PANE).selections(),
        1,
        "opening a Workspace should ask the server for its Focused Pane once"
    );
    assert_eq!(
        harness.panes.pane(SPLIT_PANE).selections(),
        0,
        "it should ask for no other Pane"
    );
}

#[gpui::test]
fn an_explicit_pane_press_should_ask_the_server_for_that_pane(cx: &mut TestAppContext) {
    let (harness, cx) = shared_tab_manager(cx);
    publish(
        &harness,
        inventory(
            2,
            vec![tab(
                ROOT_TAB,
                vec![
                    pane(ROOT_PANE, None, Axis::Right),
                    pane(SPLIT_PANE, Some(ROOT_PANE), Axis::Down),
                ],
            )],
        ),
        cx,
    );
    assert_eq!(harness.panes.pane(SPLIT_PANE).selections(), 0);

    click("pane-surface-2", cx);

    assert_eq!(
        harness.panes.pane(SPLIT_PANE).selections(),
        1,
        "a press on a Pane should ask the server for that Pane"
    );
    assert_eq!(
        harness.panes.pane(ROOT_PANE).selections(),
        0,
        "it should ask for no other Pane"
    );
}

#[gpui::test]
fn an_offline_client_should_ask_the_server_for_no_pane(cx: &mut TestAppContext) {
    let (harness, cx) = shared_tab_manager(cx);
    publish(
        &harness,
        inventory(
            2,
            vec![tab(
                ROOT_TAB,
                vec![
                    pane(ROOT_PANE, None, Axis::Right),
                    pane(SPLIT_PANE, Some(ROOT_PANE), Axis::Down),
                ],
            )],
        ),
        cx,
    );
    harness.edits.set_connected(false);

    click("pane-surface-2", cx);

    assert_eq!(
        harness.panes.pane(SPLIT_PANE).selections(),
        0,
        "an unreachable server is asked for nothing, including a retained shell"
    );
}

#[gpui::test]
fn a_takeover_screen_should_replace_the_pane_and_offer_take_over(cx: &mut TestAppContext) {
    let (harness, cx) = shared_tab_manager(cx);
    harness
        .panes
        .pane(ROOT_PANE)
        .set(DevelopmentPaneStatus::Takeover);
    settle(&harness, cx);

    assert!(
        cx.debug_bounds("shared-pane-takeover-1").is_some(),
        "another client's control should replace the Pane with its screen"
    );

    click("shared-pane-take-over-1", cx);

    assert_eq!(
        harness.panes.pane(ROOT_PANE).take_overs(),
        1,
        "Take Over should ask the server once"
    );
}

/// Each change to a Pane's connection hints the Tab, which shows it without a timer, including a
/// change that follows the Tab's previous reading.
#[gpui::test]
fn each_pane_change_should_show_without_a_timer(cx: &mut TestAppContext) {
    let (harness, cx) = shared_tab_manager(cx);
    let pane = harness.panes.pane(ROOT_PANE);

    pane.set(DevelopmentPaneStatus::Takeover);
    settle(&harness, cx);
    assert!(
        cx.debug_bounds("shared-pane-takeover-1").is_some(),
        "the first change shows"
    );

    pane.reconnect();
    settle(&harness, cx);
    assert!(
        cx.debug_bounds("shared-pane-takeover-1").is_none(),
        "a change after the Tab's reading shows too"
    );
}

/// The Takeover Screen names who controls the Pane, and follows control as it moves, so a person
/// knows where typing goes before taking it over. Each change is announced.
#[gpui::test]
fn a_takeover_screen_should_name_the_controlling_client_as_control_moves(cx: &mut TestAppContext) {
    use spaceterm_server::pane::control::Controller;
    use spaceterm_ui::a11y_testing::A11yTree;

    let (harness, cx) = shared_tab_manager(cx);
    let pane = harness.panes.pane(ROOT_PANE);
    let client = |name: &str| Controller::Client {
        name: name.into(),
        this_client: false,
    };
    let announced = |cx: &mut VisualTestContext, title: &str, detail: &str| {
        let tree = A11yTree::read(cx);
        assert_eq!(
            tree.node(title)["aria"]["role"],
            "Group",
            "the screen is named {title:?}"
        );
        assert_eq!(
            tree.node(&format!("{title}. {detail}"))["aria"]["role"],
            "Status",
            "assistive technology hears {title:?}"
        );
    };

    pane.offer_take_over(client("Sadik’s MacBook Pro"));
    settle(&harness, cx);
    announced(
        cx,
        "Controlled by Sadik’s MacBook Pro",
        crate::ui::server_pane::TAKEOVER_DETAIL,
    );

    pane.offer_take_over(client("Build Server"));
    settle(&harness, cx);
    announced(
        cx,
        "Controlled by Build Server",
        crate::ui::server_pane::TAKEOVER_DETAIL,
    );
    assert!(
        A11yTree::read(cx)
            .find("Controlled by Sadik’s MacBook Pro")
            .is_none(),
        "the former Controlling Client is no longer named"
    );

    pane.offer_take_over(Controller::Nobody);
    settle(&harness, cx);
    announced(
        cx,
        "No Client Controls This Pane",
        "Take Over to type and resize this Pane.",
    );

    // An interrupted connection cannot say who controls the Pane now, so it names nobody.
    pane.offer_take_over(client("Build Server"));
    settle(&harness, cx);
    pane.set(DevelopmentPaneStatus::TakeoverReconnecting);
    settle(&harness, cx);
    let tree = A11yTree::read(cx);
    assert!(tree.find("Controlled by Build Server").is_none());
    assert!(tree.find(crate::ui::server_pane::TAKEOVER_TITLE).is_some());
}

/// An ended Session has nothing to take over, so a client without its control grant sees the same
/// Session Ended screen as the one holding it. Interrupted, the screen waits for reconnection.
#[gpui::test]
fn another_clients_ended_pane_should_show_session_ended_without_starting_one(
    cx: &mut TestAppContext,
) {
    use spaceterm_server::pane::{SessionEnd, control::Controller};
    use spaceterm_ui::a11y_testing::A11yTree;

    let (harness, cx) = shared_tab_manager(cx);
    let pane = harness.panes.pane(ROOT_PANE);
    pane.offer_take_over(Controller::Client {
        name: "Build Server".into(),
        this_client: false,
    });
    settle(&harness, cx);
    pane.set_session_end(Some(SessionEnd::Exited { code: 1 }));
    settle(&harness, cx);

    let tree = A11yTree::read(cx);
    assert!(tree.find("Controlled by Build Server").is_none());
    assert!(
        tree.find("Session Ended. The shell exited with code 1.")
            .is_some(),
        "the screen says why the shell ended"
    );
    assert!(cx.debug_bounds("shared-pane-takeover-1").is_none());
    assert!(cx.debug_bounds("shared-pane-start-session-1").is_some());
    assert!(cx.debug_bounds("shared-pane-close-pane-1").is_some());
    assert_eq!(pane.restored_starts(), 0, "presenting it starts nothing");
    assert_eq!(pane.take_overs(), 0, "presenting it takes nothing over");

    pane.set(DevelopmentPaneStatus::TakeoverReconnecting);
    settle(&harness, cx);
    assert_eq!(
        A11yTree::read(cx).node("Reconnecting…")["aria"]["description"],
        "Start Session is available after reconnection."
    );
}

/// Start Session on a Pane whose control grant another client holds takes it over, and the server
/// starts the shell in that one request.
#[gpui::test]
fn start_session_on_another_clients_ended_pane_should_take_it_over(cx: &mut TestAppContext) {
    use spaceterm_server::pane::{SessionEnd, control::Controller};

    let (harness, cx) = shared_tab_manager(cx);
    let pane = harness.panes.pane(ROOT_PANE);
    pane.offer_take_over(Controller::Client {
        name: "Build Server".into(),
        this_client: false,
    });
    pane.set_session_end(Some(SessionEnd::EndAllSessions));
    settle(&harness, cx);

    click("shared-pane-start-session-1", cx);
    assert_eq!(pane.take_overs(), 1);
    assert_eq!(pane.restored_starts(), 0, "Take Over carries the start");
    pane.reconnect();
    settle(&harness, cx);
    assert!(cx.debug_bounds("shared-pane-session-ended-1").is_none());
}

#[gpui::test]
fn an_interrupted_shared_pane_should_offer_retry_until_its_screen_returns(cx: &mut TestAppContext) {
    let (harness, cx) = shared_tab_manager(cx);
    harness
        .panes
        .pane(ROOT_PANE)
        .set(DevelopmentPaneStatus::Reconnecting);
    settle(&harness, cx);

    assert!(
        cx.debug_bounds("shared-pane-notice-1").is_some(),
        "an interruption should say so over the Pane's frozen screen"
    );

    click("shared-pane-retry-1", cx);

    assert_eq!(
        harness.panes.pane(ROOT_PANE).retries(),
        1,
        "Retry should ask for a connection attempt now"
    );

    harness.panes.pane(ROOT_PANE).reconnect();
    settle(&harness, cx);

    assert!(
        cx.debug_bounds("shared-pane-notice-1").is_none(),
        "the notice should leave once the server publishes this client's screens again"
    );
}

#[gpui::test]
fn a_restored_pane_should_offer_start_new_shell_without_starting_one(cx: &mut TestAppContext) {
    let (harness, cx) = shared_tab_manager(cx);
    harness
        .panes
        .pane(ROOT_PANE)
        .set(DevelopmentPaneStatus::Restored);
    settle(&harness, cx);

    assert!(
        cx.debug_bounds("shared-pane-restored-1").is_some(),
        "a Pane retained across a server restart should show its own screen"
    );
    assert_eq!(
        harness.panes.pane(ROOT_PANE).restored_starts(),
        0,
        "rendering the screen must not start a shell"
    );

    click("shared-pane-start-shell-1", cx);

    assert_eq!(
        harness.panes.pane(ROOT_PANE).restored_starts(),
        1,
        "Start New Shell should ask the server once"
    );
}

#[gpui::test]
fn an_ended_pane_should_offer_start_session_without_starting_one(cx: &mut TestAppContext) {
    let (harness, cx) = shared_tab_manager(cx);
    harness
        .panes
        .pane(ROOT_PANE)
        .set(DevelopmentPaneStatus::SessionEnded(
            spaceterm_server::pane::SessionEnd::EndAllSessions,
        ));
    settle(&harness, cx);

    assert!(
        cx.debug_bounds("shared-pane-session-ended-1").is_some(),
        "a Pane whose Session End All Sessions ended should say so"
    );
    assert!(
        cx.debug_bounds("shared-pane-restored-1").is_none(),
        "an ended Session is not a server restart"
    );
    // Repaints, reconnection, and focus restoration never start an ended Session.
    harness
        .panes
        .pane(ROOT_PANE)
        .reconnect_to(DevelopmentPaneStatus::SessionEnded(
            spaceterm_server::pane::SessionEnd::EndAllSessions,
        ));
    settle(&harness, cx);
    assert_eq!(
        harness.panes.pane(ROOT_PANE).restored_starts(),
        0,
        "presenting the ended Session must not start a shell"
    );

    click("shared-pane-start-session-1", cx);

    assert_eq!(
        harness.panes.pane(ROOT_PANE).restored_starts(),
        1,
        "Start Session should ask the server once"
    );
    click("shared-pane-start-session-1", cx);
    assert_eq!(
        harness.panes.pane(ROOT_PANE).restored_starts(),
        1,
        "a start awaiting its answer should not be asked for again"
    );
}

#[gpui::test]
fn an_unavailable_shared_pane_offers_explicit_retry_and_keeps_terminal_content_hidden(
    cx: &mut TestAppContext,
) {
    let (harness, cx) = shared_tab_manager(cx);
    harness
        .panes
        .pane(ROOT_PANE)
        .set(DevelopmentPaneStatus::Failed(
            crate::server_connection::ServerError::DirectoryUnavailable,
        ));
    settle(&harness, cx);
    assert!(
        cx.debug_bounds("shared-pane-directory-unavailable-1")
            .is_some()
    );
    assert!(cx.debug_bounds("shared-pane-directory-retry-1").is_some());
    assert!(cx.debug_bounds("shared-pane-choose-directory-1").is_some());
    assert_eq!(harness.panes.pane(ROOT_PANE).restored_starts(), 0);
    click("shared-pane-directory-retry-1", cx);
    assert_eq!(harness.panes.pane(ROOT_PANE).restored_starts(), 1);
    click("shared-pane-directory-retry-1", cx);
    assert_eq!(harness.panes.pane(ROOT_PANE).restored_starts(), 1);
}

/// Two desktop clients presenting the same server Workspace, each through its own Tab strip.
/// Each one's arrangement and reading state is its own; the server owns only membership.
mod independent_views {
    use super::*;
    use crate::domain::{PaneId, ZoomState};
    use gpui::accesskit::{Action, ActionData};
    use spaceterm_ui::a11y_testing::{A11yTree, perform_with};

    const THIRD_TAB: Id = [10; 32];
    const THIRD_TAB_PANE: Id = [11; 32];
    const FOURTH_TAB: Id = [12; 32];
    const FOURTH_TAB_PANE: Id = [13; 32];

    /// The name a Layout description gives a server Pane.
    fn label(pane: Id) -> &'static str {
        match pane {
            ROOT_PANE => "root",
            SPLIT_PANE => "split",
            REPLACEMENT_PANE => "new",
            _ => "other",
        }
    }

    /// One Pane Layout signature, with every Pane named by its server identity and every Split
    /// by its axis and ratio, so two clients' layouts compare although their local Pane numbers
    /// differ.
    fn describe(signature: &str, server: impl Fn(PaneId) -> Option<Id>) -> String {
        let mut described = String::new();
        let mut rest = signature;
        while !rest.is_empty() {
            if let Some(after) = rest.strip_prefix("pane:") {
                let end = after.find([',', ')']).unwrap_or(after.len());
                let pane = PaneId::new(after[..end].parse().expect("a Pane number"));
                described.push_str(label(server(pane).expect("every Pane here is shared")));
                rest = &after[end..];
            } else if let Some(after) = rest.strip_prefix("split:") {
                let end = after.find('(').expect("a Split lists its children");
                let mut fields = after[..end].split(':').skip(1);
                let axis = fields.next().expect("a Split axis");
                let ratio: f32 = fields
                    .next()
                    .and_then(|ratio| ratio.parse().ok())
                    .expect("a Split ratio");
                described.push_str(&format!("{axis} {ratio:.2}("));
                rest = &after[end + 1..];
            } else {
                described.push_str(&rest[..1]);
                rest = &rest[1..];
            }
        }
        described
    }

    /// One client's own arrangement: its Tab display order, and its Active Tab's Pane Layout.
    fn arrangement(harness: &SharedHarness, cx: &mut VisualTestContext) -> (Vec<Id>, String) {
        harness.manager.read_with(cx, |manager, cx| {
            let order = manager
                .tab_ids()
                .into_iter()
                .map(|tab| manager.shared_server_tab(tab).expect("every Tab is shared"))
                .collect();
            let (active_tab, _) = manager.active_terminal_identity(cx);
            let layout = describe(
                &manager.active_tab_view().read(cx).layout_signature(),
                |pane| manager.shared_server_pane(active_tab, pane, cx),
            );
            (order, layout)
        })
    }

    /// The Active Tab and its Focused Pane, as the server names them.
    fn view(harness: &SharedHarness, cx: &mut VisualTestContext) -> (Id, Id) {
        harness.manager.read_with(cx, |manager, cx| {
            let (tab, pane) = manager.active_terminal_identity(cx);
            (
                manager.shared_server_tab(tab).expect("a shared Tab"),
                manager
                    .shared_server_pane(tab, pane, cx)
                    .expect("a shared Pane"),
            )
        })
    }

    fn zoomed(harness: &SharedHarness, cx: &mut VisualTestContext) -> Option<Id> {
        harness.manager.read_with(cx, |manager, cx| {
            let (tab, _) = manager.active_terminal_identity(cx);
            match manager.active_tab_view().read(cx).zoom_state() {
                ZoomState::Zoomed(pane) => manager.shared_server_pane(tab, pane, cx),
                ZoomState::Restored => None,
            }
        })
    }

    fn keyboard_in_focused_pane(harness: &SharedHarness, cx: &mut VisualTestContext) -> bool {
        cx.update(|window, cx| {
            harness
                .manager
                .read(cx)
                .focused_terminal_is_focused(window, cx)
        })
    }

    /// Both clients read the same publication.
    fn publish_to_both(
        clients: [(&SharedHarness, &mut VisualTestContext); 2],
        inventory: &Inventory,
    ) {
        for (harness, cx) in clients {
            publish(harness, inventory.clone(), cx);
        }
    }

    /// Resizes the Active Tab's only Split to half its current offset through the action
    /// assistive technology performs.
    fn halve_split_accessibly(cx: &mut VisualTestContext) {
        let offset = A11yTree::read(cx).node("Resize Pane split")["aria"]["numeric_value"]
            .as_f64()
            .expect("the Split publishes its offset");
        let tree = A11yTree::read(cx);
        perform_with(
            cx,
            tree.node("Resize Pane split"),
            Action::SetValue,
            Some(ActionData::NumericValue(offset / 2.0)),
        );
        cx.run_until_parked();
    }

    /// Drags local Pane 1 by its caption onto the right edge of local Pane 2.
    fn move_first_pane_right_of_second(cx: &mut VisualTestContext) {
        let target = cx
            .debug_bounds("pane-surface-2")
            .expect("the second Pane was rendered");
        let caption = cx
            .debug_bounds("pane-caption-1-focused")
            .or_else(|| cx.debug_bounds("pane-caption-1-unfocused"))
            .expect("the first Pane's caption was rendered");
        let from = gpui::point(caption.left() + gpui::px(24.0), caption.center().y);
        let edge = gpui::point(target.right() - gpui::px(8.0), target.center().y);
        cx.simulate_mouse_move(from, None, Modifiers::none());
        cx.simulate_mouse_down(from, gpui::MouseButton::Left, Modifiers::none());
        for position in [edge, edge + gpui::point(gpui::px(-1.0), gpui::px(0.0))] {
            cx.simulate_mouse_move(position, Some(gpui::MouseButton::Left), Modifiers::none());
            cx.run_until_parked();
        }
        cx.simulate_mouse_up(edge, gpui::MouseButton::Left, Modifiers::none());
        cx.run_until_parked();
    }

    /// The scenario in ADR 0042: Mac 1 and Mac 2 arrange the same Workspace differently, then
    /// another client creates and deletes Tabs and Panes. Each Mac keeps its own order, Pane
    /// placement, Split axes, and ratios for what survives, and the new work joins where the ADR
    /// says without moving either Mac's Active Tab or Focused Pane.
    #[gpui::test]
    fn two_clients_should_keep_their_own_arrangements_through_shared_creation_and_deletion(
        mac_1: &mut TestAppContext,
        mac_2: &mut TestAppContext,
    ) {
        let (first, mac_1) = shared_tab_manager(mac_1);
        let (second, mac_2) = shared_tab_manager(mac_2);
        publish_to_both(
            [(&first, &mut *mac_1), (&second, &mut *mac_2)],
            &inventory(
                2,
                vec![
                    tab(
                        ROOT_TAB,
                        vec![
                            pane(ROOT_PANE, None, Axis::Right),
                            pane(SPLIT_PANE, Some(ROOT_PANE), Axis::Down),
                        ],
                    ),
                    tab(SECOND_TAB, vec![pane(SECOND_TAB_PANE, None, Axis::Right)]),
                    tab(THIRD_TAB, vec![pane(THIRD_TAB_PANE, None, Axis::Right)]),
                ],
            ),
        );
        let default = (
            vec![ROOT_TAB, SECOND_TAB, THIRD_TAB],
            "Vertical 0.50(root,split)".to_owned(),
        );
        assert_eq!(
            arrangement(&first, mac_1),
            default,
            "creation order by default"
        );
        assert_eq!(
            arrangement(&second, mac_2),
            default,
            "creation order by default"
        );

        // Mac 1 moves its Root Tab to the end and resizes the Split from assistive technology.
        mac_1.dispatch_action(crate::ui::MoveTabRight);
        mac_1.dispatch_action(crate::ui::MoveTabRight);
        mac_1.run_until_parked();
        halve_split_accessibly(mac_1);
        // Mac 2 moves its Root Pane beside the split one.
        move_first_pane_right_of_second(mac_2);

        assert_eq!(
            arrangement(&first, mac_1),
            (
                vec![SECOND_TAB, THIRD_TAB, ROOT_TAB],
                "Vertical 0.25(root,split)".to_owned()
            )
        );
        assert_eq!(
            arrangement(&second, mac_2),
            (
                vec![ROOT_TAB, SECOND_TAB, THIRD_TAB],
                "Horizontal 0.50(split,root)".to_owned()
            ),
            "Mac 1's rearrangement must leave Mac 2's arrangement alone, and Mac 2's own move \
             must leave Mac 1's alone"
        );
        assert!(
            first.edits.submitted().is_empty() && second.edits.submitted().is_empty(),
            "rearranging is a client's own view, so it asks the server for nothing and cannot \
             change its Root Tab or Root Pane"
        );
        let views = (view(&first, mac_1), view(&second, mac_2));

        // Another client deletes the second Tab, creates a fourth, and splits the split Pane.
        publish_to_both(
            [(&first, &mut *mac_1), (&second, &mut *mac_2)],
            &inventory(
                3,
                vec![
                    tab(
                        ROOT_TAB,
                        vec![
                            pane(ROOT_PANE, None, Axis::Right),
                            pane(SPLIT_PANE, Some(ROOT_PANE), Axis::Down),
                            pane(REPLACEMENT_PANE, Some(SPLIT_PANE), Axis::Right),
                        ],
                    ),
                    tab(THIRD_TAB, vec![pane(THIRD_TAB_PANE, None, Axis::Right)]),
                    tab(FOURTH_TAB, vec![pane(FOURTH_TAB_PANE, None, Axis::Right)]),
                ],
            ),
        );

        assert_eq!(
            arrangement(&first, mac_1),
            (
                vec![THIRD_TAB, ROOT_TAB, FOURTH_TAB],
                "Vertical 0.25(root,Horizontal 0.50(split,new))".to_owned()
            ),
            "the new Tab appends to Mac 1's own order, and the new Pane joins beside its source \
             in Mac 1's own layout"
        );
        assert_eq!(
            arrangement(&second, mac_2),
            (
                vec![ROOT_TAB, THIRD_TAB, FOURTH_TAB],
                "Horizontal 0.50(Horizontal 0.50(split,new),root)".to_owned()
            ),
            "the same creation joins Mac 2's different arrangement the same way"
        );

        // The Pane the new one was split from is deleted, and the server clears the link.
        publish_to_both(
            [(&first, &mut *mac_1), (&second, &mut *mac_2)],
            &inventory(
                4,
                vec![
                    tab(
                        ROOT_TAB,
                        vec![
                            pane(ROOT_PANE, None, Axis::Right),
                            pane(REPLACEMENT_PANE, None, Axis::Right),
                        ],
                    ),
                    tab(THIRD_TAB, vec![pane(THIRD_TAB_PANE, None, Axis::Right)]),
                    tab(FOURTH_TAB, vec![pane(FOURTH_TAB_PANE, None, Axis::Right)]),
                ],
            ),
        );

        assert_eq!(
            arrangement(&first, mac_1).1,
            "Vertical 0.25(root,new)",
            "Mac 1 keeps a valid layout of what survives, with its own ratio"
        );
        assert_eq!(
            arrangement(&second, mac_2).1,
            "Horizontal 0.50(new,root)",
            "Mac 2 keeps a valid layout of what survives, with its own placement"
        );
        assert_eq!(
            (view(&first, mac_1), view(&second, mac_2)),
            views,
            "another client's creation and deletion move neither Active Tab nor Focused Pane"
        );
        for (harness, pane) in [(&first, REPLACEMENT_PANE), (&second, REPLACEMENT_PANE)] {
            assert_eq!(
                harness.panes.pane(pane).selections(),
                0,
                "presenting another client's Pane must not ask the server for it"
            );
        }
        assert!(
            first.edits.submitted().is_empty() && second.edits.submitted().is_empty(),
            "reconciling publications submits nothing"
        );
    }

    /// One client zooms and another focuses a different Pane. Another client's split and Tab then
    /// join both without taking either client's zoom, Focused Pane, or keyboard.
    #[gpui::test]
    fn another_clients_creation_should_take_neither_clients_zoom_nor_focus(
        mac_1: &mut TestAppContext,
        mac_2: &mut TestAppContext,
    ) {
        let (first, mac_1) = shared_tab_manager(mac_1);
        let (second, mac_2) = shared_tab_manager(mac_2);
        let split = vec![
            pane(ROOT_PANE, None, Axis::Right),
            pane(SPLIT_PANE, Some(ROOT_PANE), Axis::Down),
        ];
        publish_to_both(
            [(&first, &mut *mac_1), (&second, &mut *mac_2)],
            &inventory(2, vec![tab(ROOT_TAB, split.clone())]),
        );

        // Mac 1 zooms its Root Pane; Mac 2 reads in the split Pane.
        mac_1.dispatch_action(crate::ui::TogglePaneZoom);
        mac_1.run_until_parked();
        click("pane-surface-2", mac_2);
        assert_eq!(zoomed(&first, mac_1), Some(ROOT_PANE));
        assert_eq!(zoomed(&second, mac_2), None, "zoom is Mac 1's own");
        assert_eq!(view(&first, mac_1), (ROOT_TAB, ROOT_PANE));
        assert_eq!(
            view(&second, mac_2),
            (ROOT_TAB, SPLIT_PANE),
            "focus is Mac 2's own"
        );

        // Another client splits the zoomed Pane and creates a Tab.
        let mut created = split.clone();
        created.push(pane(REPLACEMENT_PANE, Some(ROOT_PANE), Axis::Right));
        publish_to_both(
            [(&first, &mut *mac_1), (&second, &mut *mac_2)],
            &inventory(
                3,
                vec![
                    tab(ROOT_TAB, created),
                    tab(SECOND_TAB, vec![pane(SECOND_TAB_PANE, None, Axis::Right)]),
                ],
            ),
        );

        assert_eq!(
            zoomed(&first, mac_1),
            Some(ROOT_PANE),
            "a Pane split from the zoomed one must not end Mac 1's zoom"
        );
        assert_eq!(view(&first, mac_1), (ROOT_TAB, ROOT_PANE));
        assert!(
            keyboard_in_focused_pane(&first, mac_1),
            "Mac 1's keyboard stays in its Pane"
        );
        assert_eq!(zoomed(&second, mac_2), None);
        assert_eq!(view(&second, mac_2), (ROOT_TAB, SPLIT_PANE));
        assert!(
            keyboard_in_focused_pane(&second, mac_2),
            "Mac 2's keyboard stays in its Pane"
        );
        for harness in [&first, &second] {
            assert_eq!(harness.panes.pane(REPLACEMENT_PANE).selections(), 0);
            assert_eq!(harness.panes.pane(SECOND_TAB_PANE).selections(), 0);
        }

        // Another client closes the split Pane, which only Mac 2 was reading.
        let mut closed = vec![pane(ROOT_PANE, None, Axis::Right)];
        closed.push(pane(REPLACEMENT_PANE, Some(ROOT_PANE), Axis::Right));
        let selections =
            [ROOT_PANE, REPLACEMENT_PANE].map(|pane| second.panes.pane(pane).selections());
        publish_to_both(
            [(&first, &mut *mac_1), (&second, &mut *mac_2)],
            &inventory(
                4,
                vec![
                    tab(ROOT_TAB, closed),
                    tab(SECOND_TAB, vec![pane(SECOND_TAB_PANE, None, Axis::Right)]),
                ],
            ),
        );

        assert_eq!(
            zoomed(&first, mac_1),
            Some(ROOT_PANE),
            "closing another Pane must not end Mac 1's zoom"
        );
        assert!(keyboard_in_focused_pane(&first, mac_1));
        let (tab, focused) = view(&second, mac_2);
        assert!(
            tab == ROOT_TAB && [ROOT_PANE, REPLACEMENT_PANE].contains(&focused),
            "Mac 2's Focused Pane left, so its own layout chooses a surviving Pane"
        );
        assert!(
            keyboard_in_focused_pane(&second, mac_2),
            "the keyboard that was in the closed Pane rests in the Pane that follows"
        );
        assert_eq!(
            [ROOT_PANE, REPLACEMENT_PANE].map(|pane| second.panes.pane(pane).selections()),
            selections,
            "nobody on Mac 2 selected a Pane, so the server is asked for nothing"
        );
    }
}

/// The edit state a person reads, independent of any window.
mod edits {
    use super::*;
    use crate::domain::WorkspaceId;
    use crate::ui::pane_lifecycle::PaneConstruction;

    fn shared(inventory: Option<Inventory>) -> (SharedWorkspaces, Rc<ScriptedEdits>) {
        let edits = Rc::new(ScriptedEdits::new(inventory));
        let source = SharedEditSource::new(Rc::clone(&edits) as Rc<dyn SharedWorkspaceEdits>);
        let session_factory: Rc<dyn TerminalSessionFactory> =
            Rc::new(crate::terminal::testing::TestTerminalSessionFactory::new(
                crate::terminal::testing::TestTerminalSessionRecords::default(),
            ));
        let session_factory = WorkspaceTerminalSessionFactory::new_local(
            session_factory,
            crate::terminal::testing::test_local_directory(PathBuf::from("/tmp/spaceterm-shared")),
        );
        let sessions = SharedPaneSessions::with_opener(
            ScriptedPanes::new(session_factory) as Rc<dyn SharedPaneOpener>,
            &PaneConstruction::testing(),
        );
        (SharedWorkspaces::new(source, sessions), edits)
    }

    fn renamed(name: &str, version: u64) -> Inventory {
        let mut inventory = root_only();
        inventory.revision = 2;
        inventory.workspaces[0].name = name.to_owned();
        inventory.workspaces[0].name_version = version;
        inventory
    }

    /// The local Workspace a test binds the server Workspace to.
    fn local() -> WorkspaceId {
        first_workspace_id()
    }

    fn bind(shared: &mut SharedWorkspaces, inventory: &Inventory) {
        shared.register(
            local(),
            SharedWorkspaceFacts::from_server(&inventory.workspaces[0]),
        );
    }

    #[test]
    fn an_offline_client_should_refuse_a_shared_edit_without_submitting_it() {
        let (mut shared, edits) = shared(Some(root_only()));
        bind(&mut shared, &root_only());
        edits.set_connected(false);

        assert_eq!(shared.editability(), SharedEditability::Offline);
        assert_eq!(
            shared.rename(local(), "Logs".to_owned(), Some(0)),
            SharedSubmission::Refused(SharedRefusalReason::Offline)
        );
        assert!(edits.submitted().is_empty());
    }

    #[test]
    fn a_connected_client_should_keep_distinct_edits_ordered_while_an_answer_waits() {
        let (mut shared, edits) = shared(Some(root_only()));
        bind(&mut shared, &root_only());

        assert_eq!(
            shared.rename(local(), "Logs".to_owned(), Some(0)),
            SharedSubmission::Submitted
        );
        assert_eq!(shared.editability(), SharedEditability::Available);
        assert_eq!(
            shared.rename(local(), "Notes".to_owned(), Some(0)),
            SharedSubmission::Submitted
        );
        assert_eq!(edits.submitted().len(), 1, "only the first edit is sent");
    }

    #[test]
    fn a_conflicting_rename_should_keep_the_draft_and_name_the_value_from_the_same_snapshot() {
        let (mut shared, edits) = shared(Some(root_only()));
        bind(&mut shared, &root_only());
        shared.rename(local(), "Mine".to_owned(), Some(0));

        // Another client's rename reached the server first, so this answer and the inventory that
        // carries its value arrive together.
        edits.set_inventory(renamed("Theirs", 1));
        edits.answer(Outcome::Conflict);
        let poll = shared.poll();

        let conflict = shared.take_conflict().expect("the draft should be kept");
        assert_eq!(conflict.workspace, local());
        assert_eq!(conflict.field, SharedWorkspaceField::Name);
        assert_eq!(
            conflict.draft,
            SharedWorkspaceDraft::Name("Mine".to_owned())
        );
        assert_eq!(
            conflict.authoritative.to_string(),
            "Theirs",
            "the conflict should name the value the server holds in the same snapshot"
        );
        assert!(poll.accepted.is_none(), "a refused edit opens nothing");
    }

    #[test]
    fn an_answer_must_reach_the_ui_before_another_edit_can_replace_it() {
        let (mut shared, edits) = shared(Some(root_only()));
        bind(&mut shared, &root_only());
        shared.rename(local(), "Mine".to_owned(), Some(0));
        edits.set_inventory(renamed("Theirs", 1));
        edits.answer(Outcome::Conflict);

        assert_eq!(
            shared.create_workspace("Logs".to_owned(), None),
            SharedSubmission::Submitted,
            "a later connected interaction must retain the earlier conflict beside its snapshot"
        );
        shared.poll();
        assert_eq!(
            shared.take_conflict().expect("unread conflict").draft,
            SharedWorkspaceDraft::Name("Mine".to_owned())
        );
        assert_eq!(
            shared.create_workspace("Logs".to_owned(), None),
            SharedSubmission::Submitted
        );
    }

    #[test]
    fn asking_again_after_reviewing_a_conflict_should_use_the_version_it_reviewed() {
        let (mut shared, edits) = shared(Some(root_only()));
        bind(&mut shared, &root_only());
        shared.rename(local(), "Mine".to_owned(), Some(0));
        edits.set_inventory(renamed("Theirs", 1));
        edits.answer(Outcome::Conflict);
        let poll = shared.poll();
        bind(&mut shared, poll.inventory.as_ref().expect("inventory"));
        let conflict = shared.take_conflict().expect("conflict");

        // The dialog still shows "Theirs" while another accepted edit arrives.
        let mut newer = renamed("Latest", 2);
        newer.revision = 3;
        edits.set_inventory(newer);
        let poll = shared.poll();
        bind(
            &mut shared,
            poll.inventory.as_ref().expect("newer inventory"),
        );

        assert_eq!(shared.ask_again(&conflict), SharedSubmission::Submitted);

        assert_eq!(
            edits.submitted().last(),
            Some(&Mutation::RenameWorkspace {
                workspace: WORKSPACE,
                version: 1,
                name: "Mine".to_owned(),
            }),
            "a person who read the server's value beside their draft asks again at that value"
        );
    }

    #[test]
    fn a_conflict_recovered_before_any_binding_should_keep_its_draft_until_the_projection() {
        // A relaunch finds a retained answer this process never submitted, and no local Workspace
        // exists yet.
        let (mut shared, edits) = shared(Some(root_only()));
        edits.submit_for_recovery(Mutation::RenameWorkspace {
            workspace: WORKSPACE,
            version: 0,
            name: "Recovered".to_owned(),
        });
        edits.set_inventory(renamed("Theirs", 1));
        edits.answer(Outcome::Conflict);

        let poll = shared.poll();
        assert!(
            shared.take_conflict().is_none(),
            "the draft waits rather than resolving to an unknown Workspace"
        );
        bind(&mut shared, poll.inventory.as_ref().expect("inventory"));

        let conflict = shared
            .take_conflict()
            .expect("the recovered draft should survive the projection");
        assert_eq!(
            conflict.draft,
            SharedWorkspaceDraft::Name("Recovered".to_owned())
        );
        assert_eq!(conflict.authoritative.to_string(), "Theirs");
    }

    #[test]
    fn an_answer_recovered_from_an_earlier_run_should_open_nothing() {
        let (mut shared, edits) = shared(Some(root_only()));
        bind(&mut shared, &root_only());
        edits.submit_for_recovery(Mutation::CreateTab {
            workspace: WORKSPACE,
            source: None,
        });
        edits.answer(Outcome::Accepted { target: SECOND_TAB });

        let poll = shared.poll();

        assert!(
            poll.accepted.is_none(),
            "nobody asked for this in this session, so nothing opens"
        );
    }

    #[test]
    fn a_recovered_worktree_removal_keeps_its_target_and_offline_status() {
        use spaceterm_server::worktree::Operation;
        let (mut shared, edits) = shared(Some(root_only()));
        let path = std::path::PathBuf::from("/fixture/linked");
        edits.submit_for_recovery(Mutation::Worktree(Operation::Remove {
            root: "/fixture".into(),
            path: path.clone(),
            boot: BOOT,
            confirmation: [9; 32],
        }));
        shared.poll();
        assert_eq!(
            shared.worktree_status(WORKSPACE, Some(std::path::Path::new("/fixture"))),
            Some("Removing Worktree…")
        );
        assert_eq!(
            shared.row_status([10; 32], Some(std::path::Path::new("/other-repository"))),
            None,
            "an unrelated Workspace's row says nothing about another row's edit"
        );
        assert!(shared.removing_worktree(&path));
        edits.set_connected(false);
        edits.set_error(spaceterm_server::ServerError::NotRunning);
        shared.poll();
        assert_eq!(
            shared
                .row_status(WORKSPACE, Some(std::path::Path::new("/fixture")))
                .map(|status| (status.text, status.warning)),
            Some(("Not connected", true)),
            "the Workspace row must show its offline status"
        );
        assert!(
            shared.removing_worktree(&path),
            "the Worktree row still names its unresolved original"
        );
    }

    #[test]
    fn an_accepted_creation_this_client_asked_for_should_open_once() {
        let (mut shared, edits) = shared(Some(root_only()));
        bind(&mut shared, &root_only());
        shared.create_workspace("Logs".to_owned(), None);
        edits.answer(Outcome::Accepted { target: WORKSPACE });

        let accepted = shared.poll().accepted.expect("the creator opens its own");
        assert_eq!(accepted.target, WORKSPACE);
        assert!(
            matches!(accepted.mutation, Mutation::CreateWorkspace { .. }),
            "the acceptance should name the edit it answers"
        );
        assert!(
            shared.poll().accepted.is_none(),
            "one answer opens its creation once"
        );
    }

    #[test]
    fn an_accepted_workspace_creation_retains_its_authoritative_attachment() {
        let (mut shared, edits) = shared(Some(root_only()));
        shared.poll();
        bind(&mut shared, &root_only());
        shared.create_workspace("Logs".to_owned(), None);
        shared.poll();
        let mut published = root_only();
        let mut created = published.workspaces[0].clone();
        let target = [21; 32];
        created.id = target;
        created.root_tab = [22; 32];
        created.tabs[0].id = [22; 32];
        created.tabs[0].root_pane = [23; 32];
        created.tabs[0].panes[0].id = [23; 32];
        published.revision += 1;
        published.workspaces.push(created);
        edits.set_inventory(published);
        edits.answer(Outcome::Accepted { target });
        let poll = shared.poll();
        assert!(
            shared
                .plan(poll.inventory.as_ref().expect("accepted inventory"))
                .iter()
                .any(|step| {
                    matches!(step, SharedWorkspacePlan::Create { facts } if facts.server == target)
                }),
            "the accepted Workspace remains attached after provisional binding"
        );
    }

    #[test]
    fn a_draft_started_before_a_publication_should_submit_the_version_it_began_with() {
        let (mut shared, edits) = shared(Some(root_only()));
        bind(&mut shared, &root_only());
        // A person starts editing the name while the Workspace sits at name version 0.
        let draft = shared.facts(local()).expect("facts").name_version;

        // Another client's rename reaches the server while that draft is still open, and this
        // client reads it.
        edits.set_inventory(renamed("Theirs", 1));
        let poll = shared.poll();
        bind(&mut shared, poll.inventory.as_ref().expect("inventory"));

        shared.rename(local(), "Mine".to_owned(), Some(draft));

        assert_eq!(
            edits.submitted().last(),
            Some(&Mutation::RenameWorkspace {
                workspace: WORKSPACE,
                version: 0,
                name: "Mine".to_owned(),
            }),
            "the draft must stay fenced to the version it began at, so the server refuses it \
             instead of overwriting the other client"
        );
    }

    #[test]
    fn a_pin_chooser_opened_before_a_publication_should_submit_the_version_it_opened_at() {
        let (mut shared, edits) = shared(Some(root_only()));
        bind(&mut shared, &root_only());
        // A person opens the chooser while the Workspace sits at pin version 0.
        let draft = shared.pin_version(local()).expect("pin version");

        let mut published = root_only();
        published.revision = 2;
        published.workspaces[0].pinned_directory = Some(PathBuf::from("/server/theirs"));
        published.workspaces[0].pin_version = 1;
        edits.set_inventory(published);
        let poll = shared.poll();
        bind(&mut shared, poll.inventory.as_ref().expect("inventory"));

        shared.pin(local(), Some(PathBuf::from("/server/mine")), Some(draft));

        assert_eq!(
            edits.submitted().last(),
            Some(&Mutation::PinWorkspace {
                workspace: WORKSPACE,
                version: 0,
                directory: Some(PathBuf::from("/server/mine")),
            }),
            "the chooser's draft must stay fenced to the pin version it opened at"
        );
    }

    #[test]
    fn a_draft_started_after_a_publication_should_submit_the_version_it_can_see() {
        let (mut shared, edits) = shared(Some(root_only()));
        bind(&mut shared, &root_only());
        edits.set_inventory(renamed("Theirs", 1));
        let poll = shared.poll();
        bind(&mut shared, poll.inventory.as_ref().expect("inventory"));

        // The person starts editing after reading the published name.
        let draft = shared.facts(local()).expect("facts").name_version;
        shared.rename(local(), "Mine".to_owned(), Some(draft));

        assert_eq!(
            edits.submitted().last(),
            Some(&Mutation::RenameWorkspace {
                workspace: WORKSPACE,
                version: 1,
                name: "Mine".to_owned(),
            }),
            "a draft started on the published value is fenced to that value"
        );
    }

    #[test]
    fn detached_workspace_keeps_its_latest_arrangement_while_other_views_are_saved() {
        use spaceterm_server::client_view::{
            ClientView, Layout, LayoutNode, TabView, WorkspaceView,
        };
        let (mut shared, _) = shared(Some(root_only()));
        shared.poll();
        bind(&mut shared, &root_only());
        let personal = WorkspaceView {
            workspace: WORKSPACE,
            tabs: vec![TabView {
                tab: ROOT_TAB,
                layout: Layout {
                    nodes: vec![LayoutNode::Pane(ROOT_PANE)],
                },
            }],
        };
        shared.save_arrangements(ClientView {
            workspaces: vec![personal.clone()],
        });
        shared.detach(local()).unwrap();
        shared.forget(local());
        shared.save_arrangements(ClientView::default());
        shared.attach(WORKSPACE).unwrap();
        assert_eq!(shared.saved_arrangement(WORKSPACE), Some(&personal));
    }

    #[test]
    fn offline_detach_survives_publication_and_reattach_never_submits_shared_creation() {
        let (mut shared, edits) = shared(Some(root_only()));
        shared.poll();
        bind(&mut shared, &root_only());
        edits.set_connected(false);
        shared.poll();
        shared.detach(local()).unwrap();
        shared.forget(local());
        assert!(shared.plan(&root_only()).is_empty());
        assert!(
            shared.available_workspaces().is_empty(),
            "detached work needs a connected server to be offered"
        );
        assert!(shared.attach(WORKSPACE).is_err());
        edits.set_connected(true);
        shared.poll();
        assert_eq!(shared.available_workspaces().len(), 1);
        shared.attach(WORKSPACE).unwrap();
        assert!(matches!(
            shared.plan(&root_only()).as_slice(),
            [SharedWorkspacePlan::Create { .. }]
        ));
        let mut gone = root_only();
        gone.revision += 1;
        gone.workspaces.clear();
        edits.set_inventory(gone);
        shared.poll();
        assert!(
            shared.attach(WORKSPACE).is_err(),
            "killed work is never recreated by a saved attachment"
        );
        assert!(edits.submitted().is_empty());
    }

    #[test]
    fn a_plan_should_create_update_and_remove_against_the_published_inventory() {
        let (mut shared, _) = shared(Some(root_only()));
        shared.poll();
        let plan = shared.plan(&root_only());
        assert!(matches!(
            plan.as_slice(),
            [SharedWorkspacePlan::Create { .. }]
        ));

        bind(&mut shared, &root_only());
        let plan = shared.plan(&renamed("Theirs", 1));
        match plan.as_slice() {
            [
                SharedWorkspacePlan::Update {
                    local: bound,
                    facts,
                },
            ] => {
                assert_eq!(*bound, local());
                assert_eq!(facts.name, "Theirs");
            }
            other => panic!("expected one update, got {} steps", other.len()),
        }

        let gone = Inventory {
            repositories: None,
            revision: 3,
            workspaces: Vec::new(),
            close: None,
        };
        assert!(matches!(
            shared.plan(&gone).as_slice(),
            [SharedWorkspacePlan::Removed { local: bound }] if *bound == local()
        ));
    }

    #[test]
    fn a_waiting_draft_for_a_removed_workspace_should_report_the_removal() {
        let (mut shared, edits) = shared(Some(root_only()));
        edits.submit_for_recovery(Mutation::RenameWorkspace {
            workspace: WORKSPACE,
            version: 0,
            name: "Recovered".to_owned(),
        });
        edits.set_inventory(renamed("Theirs", 1));
        edits.answer(Outcome::Conflict);
        shared.poll();

        edits.set_inventory(Inventory {
            repositories: None,
            revision: 9,
            workspaces: Vec::new(),
            close: None,
        });
        shared.poll();

        assert!(shared.take_conflict().is_none());
        assert!(
            shared.take_refusal().is_some(),
            "a draft whose Workspace is gone reports the removal instead of waiting"
        );
    }
}

/// What a shared close confirmation captures, says, and asks the server to end.
mod close {
    use super::*;
    use crate::close_confirmation::CloseTarget as LocalCloseTarget;
    use crate::domain::TabId;
    use crate::ui::pane_lifecycle::PaneConstruction;
    use crate::ui::shared_workspaces::{SharedCloseState, SharedCloseUnavailable};
    use spaceterm_server::hierarchy::{CloseConfirmation, CloseTarget};

    fn shared(inventory: Inventory) -> (SharedWorkspaces, Rc<ScriptedEdits>) {
        let edits = Rc::new(ScriptedEdits::new(Some(inventory)));
        let source = SharedEditSource::new(Rc::clone(&edits) as Rc<dyn SharedWorkspaceEdits>);
        let session_factory = WorkspaceTerminalSessionFactory::new_local(
            Rc::new(crate::terminal::testing::TestTerminalSessionFactory::new(
                crate::terminal::testing::TestTerminalSessionRecords::default(),
            )),
            crate::terminal::testing::test_local_directory(PathBuf::from("/tmp/spaceterm-shared")),
        );
        let sessions = SharedPaneSessions::with_opener(
            ScriptedPanes::new(session_factory) as Rc<dyn SharedPaneOpener>,
            &PaneConstruction::testing(),
        );
        let mut shared = SharedWorkspaces::new(source, sessions);
        shared.poll();
        (shared, edits)
    }

    /// A Root Tab split in two, beside a second Tab.
    fn split_beside_second_tab() -> Inventory {
        inventory(
            2,
            vec![
                tab(
                    ROOT_TAB,
                    vec![
                        pane(ROOT_PANE, None, Axis::Right),
                        pane(SPLIT_PANE, Some(ROOT_PANE), Axis::Down),
                    ],
                ),
                tab(SECOND_TAB, vec![pane(SECOND_TAB_PANE, None, Axis::Right)]),
            ],
        )
    }

    fn with_session(mut inventory: Inventory, pane: Id, session: Id) -> Inventory {
        let facts = inventory.close.as_mut().expect("close facts");
        for confirmed in &mut facts.sessions {
            if confirmed.id == pane {
                confirmed.session = Some(session);
            }
        }
        inventory
    }

    const ROOT_TAB_TARGET: CloseTarget = CloseTarget::Tab {
        workspace: WORKSPACE,
        tab: ROOT_TAB,
    };

    fn local_tab() -> LocalCloseTarget {
        LocalCloseTarget::Tab {
            workspace_id: first_workspace_id(),
            tab_id: TabId::new(1),
        }
    }

    #[test]
    fn a_confirmed_close_should_ask_the_server_to_end_exactly_what_it_showed() {
        let published = with_session(split_beside_second_tab(), SPLIT_PANE, [40; 32]);
        let (mut shared, edits) = shared(published.clone());

        let approval = shared
            .close_approval(ROOT_TAB_TARGET)
            .expect("the published Tab can be reviewed");
        assert_eq!(shared.close_state(&approval), SharedCloseState::Current);
        assert_eq!(
            shared.close(approval, local_tab()),
            SharedSubmission::Submitted
        );

        assert_eq!(
            edits.submitted(),
            vec![Mutation::Close {
                target: ROOT_TAB_TARGET,
                confirmation: CloseConfirmation {
                    own_controls: None,
                    boot: BOOT,
                    panes: vec![
                        ConfirmedPane {
                            id: ROOT_PANE,
                            session: None,
                        },
                        ConfirmedPane {
                            id: SPLIT_PANE,
                            session: Some([40; 32]),
                        },
                    ],
                    removes_tab: true,
                    removes_workspace: false,
                },
            }],
            "the request names both Panes and the Session the person saw, and nothing in the \
             second Tab"
        );
    }

    #[test]
    fn a_session_that_starts_after_the_review_should_make_it_stale_without_a_new_revision() {
        let (mut shared, edits) = shared(split_beside_second_tab());
        let approval = shared
            .close_approval(ROOT_TAB_TARGET)
            .expect("the published Tab can be reviewed");

        // Another client starts the restored shell. Membership and its revision stay the same.
        edits.set_inventory(with_session(split_beside_second_tab(), ROOT_PANE, [41; 32]));
        shared.poll();

        assert_eq!(shared.close_state(&approval), SharedCloseState::Changed);
        assert!(edits.submitted().is_empty());
    }

    /// Another client closes the split beside the reviewed Pane. The Pane and its Session stay
    /// the same, but closing it now also closes its Tab, which the person never reviewed.
    #[test]
    fn a_sibling_close_that_makes_the_pane_its_tabs_last_should_require_a_fresh_review() {
        const ROOT_PANE_TARGET: CloseTarget = CloseTarget::Pane {
            workspace: WORKSPACE,
            tab: ROOT_TAB,
            pane: ROOT_PANE,
        };
        let (mut shared, edits) = shared(split_beside_second_tab());
        let approval = shared
            .close_approval(ROOT_PANE_TARGET)
            .expect("the published Pane can be reviewed");
        assert_eq!(
            approval.message(),
            "The Pane closes for every client. No Terminal Session is running in it."
        );
        let local_pane = LocalCloseTarget::Pane {
            workspace_id: first_workspace_id(),
            tab_id: TabId::new(1),
            pane_id: crate::domain::PaneId::new(1),
        };
        let (mut initial, initial_edits) = self::shared(split_beside_second_tab());
        assert_eq!(
            initial.close(approval.clone(), local_pane),
            SharedSubmission::Submitted
        );
        assert!(matches!(
            initial_edits.submitted().as_slice(),
            [Mutation::Close {
                confirmation: CloseConfirmation {
                    removes_tab: false,
                    ..
                },
                ..
            }]
        ));

        edits.set_inventory(inventory(
            3,
            vec![
                tab(ROOT_TAB, vec![pane(ROOT_PANE, None, Axis::Right)]),
                tab(SECOND_TAB, vec![pane(SECOND_TAB_PANE, None, Axis::Right)]),
            ],
        ));
        shared.poll();

        assert_eq!(
            shared.close_state(&approval),
            SharedCloseState::Changed,
            "the reviewed close no longer says its Tab closes too"
        );
        let fresh = shared
            .close_approval(ROOT_PANE_TARGET)
            .expect("the Pane can be reviewed again");
        assert_eq!(
            fresh.message(),
            "This is the only Pane in its Tab, so the Tab closes for every client. No Terminal \
             Session is running in it."
        );
        assert_eq!(fresh.detail(), None, "the Workspace keeps its second Tab");
        assert!(edits.submitted().is_empty());
        assert_eq!(shared.close(fresh, local_pane), SharedSubmission::Submitted);
        assert!(matches!(
            edits.submitted().as_slice(),
            [Mutation::Close {
                confirmation: CloseConfirmation {
                    removes_tab: true,
                    removes_workspace: false,
                    ..
                },
                ..
            }]
        ));
    }

    #[test]
    fn a_target_another_client_removed_should_be_missing_rather_than_changed() {
        let (mut shared, edits) = shared(split_beside_second_tab());
        let approval = shared
            .close_approval(ROOT_TAB_TARGET)
            .expect("the published Tab can be reviewed");

        edits.set_inventory(inventory(
            3,
            vec![tab(
                SECOND_TAB,
                vec![pane(SECOND_TAB_PANE, None, Axis::Right)],
            )],
        ));
        shared.poll();

        assert_eq!(shared.close_state(&approval), SharedCloseState::Missing);
        assert_eq!(
            shared.close_approval(ROOT_TAB_TARGET),
            Err(SharedCloseUnavailable::Missing)
        );
    }

    #[test]
    fn a_close_without_reported_sessions_should_not_be_offered_for_review() {
        let mut unreported = split_beside_second_tab();
        unreported.close = None;
        let (shared, _) = shared(unreported);

        assert_eq!(
            shared.close_approval(ROOT_TAB_TARGET),
            Err(SharedCloseUnavailable::Unreported),
            "a person cannot approve Sessions the server has not reported"
        );
    }

    #[test]
    fn an_offline_client_should_offer_no_close_review() {
        let (shared, edits) = shared(split_beside_second_tab());
        edits.set_connected(false);

        assert_eq!(
            shared.close_approval(ROOT_TAB_TARGET),
            Err(SharedCloseUnavailable::Edits(SharedRefusalReason::Offline))
        );
    }

    #[test]
    fn a_stale_answer_should_name_the_local_target_only_for_the_client_that_asked() {
        let (mut asked, edits) = shared(split_beside_second_tab());
        let approval = asked
            .close_approval(ROOT_TAB_TARGET)
            .expect("the published Tab can be reviewed");
        asked.close(approval, local_tab());
        edits.answer(Outcome::StaleConfirmation);
        asked.poll();

        assert_eq!(
            asked.take_stale_close(),
            Some((ROOT_TAB_TARGET, Some(local_tab())))
        );
        assert_eq!(asked.take_stale_close(), None, "one answer is read once");
        assert_eq!(
            edits.submitted().len(),
            1,
            "a stale approval is never retried on its own"
        );

        // An answer recovered after a relaunch belongs to no review this process opened.
        let (mut recovered, edits) = shared(split_beside_second_tab());
        edits.submit_for_recovery(Mutation::Close {
            target: ROOT_TAB_TARGET,
            confirmation: CloseConfirmation {
                own_controls: None,
                boot: BOOT,
                panes: Vec::new(),
                removes_tab: true,
                removes_workspace: false,
            },
        });
        edits.answer(Outcome::StaleConfirmation);
        recovered.poll();
        assert_eq!(recovered.take_stale_close(), Some((ROOT_TAB_TARGET, None)));
    }

    #[test]
    fn each_confirmation_should_state_what_ends_for_every_client() {
        // Both Root Tab Panes run a shell; the second Tab's Pane has not started one yet.
        let published = with_session(
            with_session(split_beside_second_tab(), ROOT_PANE, [40; 32]),
            SPLIT_PANE,
            [41; 32],
        );
        let (shared, _) = shared(published);
        let tab = shared
            .close_approval(ROOT_TAB_TARGET)
            .expect("the published Tab can be reviewed");
        assert_eq!(
            tab.message(),
            "The Tab and its 2 Panes close for every client. This ends 2 Terminal Sessions and stops their running commands."
        );
        assert_eq!(tab.detail(), None, "the Workspace keeps its second Tab");

        let unstarted = shared
            .close_approval(CloseTarget::Pane {
                workspace: WORKSPACE,
                tab: SECOND_TAB,
                pane: SECOND_TAB_PANE,
            })
            .expect("the published Pane can be reviewed");
        assert_eq!(
            unstarted.message(),
            "This is the only Pane in its Tab, so the Tab closes for every client. No Terminal \
             Session is running in it.",
            "a retained Pane without a shell ends no Session"
        );

        let running = shared
            .close_approval(CloseTarget::Pane {
                workspace: WORKSPACE,
                tab: ROOT_TAB,
                pane: SPLIT_PANE,
            })
            .expect("the published Pane can be reviewed");
        assert_eq!(
            running.message(),
            "This ends 1 Terminal Session and stops its running commands for every client."
        );

        let kill = shared
            .close_approval(CloseTarget::Workspace {
                workspace: WORKSPACE,
            })
            .expect("the published Workspace can be reviewed");
        assert_eq!(kill.title(), "Kill Workspace \u{201C}Build\u{201D}?");
        assert_eq!(kill.confirm_label(), "Kill Workspace");
        assert_eq!(
            kill.message(),
            "Its 2 Tabs and 2 Terminal Sessions end for every client. This can\u{2019}t be undone.",
            "Kill counts live Sessions, not Panes"
        );
        assert_eq!(
            kill.detail(),
            Some("Repository files and Worktrees stay on disk.")
        );
    }

    /// The Tab outside the Worktree holds no Pane, so closing the Worktree's Tabs empties the
    /// Workspace and the server removes it.
    #[test]
    fn closing_a_worktrees_tabs_that_empty_the_workspace_should_say_it_is_removed() {
        let mut worktree_tab = tab(ROOT_TAB, vec![pane(ROOT_PANE, None, Axis::Right)]);
        worktree_tab.worktree = Some(PathBuf::from("/server/work-feature"));
        let empty_tab = Tab {
            root_pane: SECOND_TAB_PANE,
            panes: Vec::new(),
            ..tab(SECOND_TAB, vec![pane(SECOND_TAB_PANE, None, Axis::Right)])
        };
        let (shared, _) = shared(inventory(1, vec![worktree_tab, empty_tab]));
        let close = shared
            .close_approval(CloseTarget::Worktree {
                workspace: WORKSPACE,
                tab: ROOT_TAB,
            })
            .expect("the published Worktree can be reviewed");

        assert_eq!(close.title(), "Kill Workspace \u{201C}Build\u{201D}?");
        assert_eq!(
            close.message(),
            "This is the only Tab in \u{201C}Build\u{201D}, so the Workspace is removed for every \
             client.",
            "no shell runs, so no Session ends"
        );
        assert_eq!(
            close.detail(),
            Some("Repository files and Worktrees stay on disk. This can\u{2019}t be undone.")
        );
    }

    #[test]
    fn closing_a_workspaces_only_pane_should_say_the_workspace_is_removed() {
        let (shared, _) = shared(with_session(root_only(), ROOT_PANE, [42; 32]));
        let only = shared
            .close_approval(CloseTarget::Pane {
                workspace: WORKSPACE,
                tab: ROOT_TAB,
                pane: ROOT_PANE,
            })
            .expect("the published Pane can be reviewed");

        assert_eq!(
            only.title(),
            "Kill Workspace \u{201C}Build\u{201D}?",
            "the final Pane requires Workspace-level review"
        );
        assert_eq!(only.confirm_label(), "Kill Workspace");
        assert_eq!(
            only.message(),
            "This is the only Pane in \u{201C}Build\u{201D}, so the Workspace is removed for every client. This ends 1 Terminal Session."
        );
        assert_eq!(
            only.detail(),
            Some("Repository files and Worktrees stay on disk. This can\u{2019}t be undone.")
        );
    }
}

/// The draft a person types, and the field version it stays fenced to.
mod draft {
    use std::cell::RefCell;

    use super::*;
    use crate::ui::workspace_sidebar::{
        SharedWorkspaceRow, SidebarEvent, WorkspaceRowViewModel, WorkspaceSidebar,
    };

    /// One shared Workspace row at `name_version`, as the Workspace Window publishes it.
    fn row(name: &str, name_version: u64) -> WorkspaceRowViewModel {
        WorkspaceRowViewModel {
            workspace_id: first_workspace_id(),
            name: name.to_owned().into(),
            path: "~/work".into(),
            machine: None,
            tooltip: "Server: /server/work".into(),
            pinned: false,
            remote_connection_phase: None,
            repository: None,
            worktrees: None,
            creates_worktrees: false,
            active: true,
            shared: Some(SharedWorkspaceRow {
                detaches: true,
                name_version,
                edits: true,
                edits_reason: "",
                status: None,
            }),
        }
    }

    type Renames = Rc<RefCell<Vec<SidebarEvent>>>;

    fn sidebar(
        cx: &mut TestAppContext,
    ) -> (Entity<WorkspaceSidebar>, Renames, &mut VisualTestContext) {
        cx.update(crate::ui::init)
            .expect("UI initialization should succeed");
        let (sidebar, cx) =
            cx.add_window_view(|window, cx| WorkspaceSidebar::new(true, window, cx));
        let events: Renames = Rc::new(RefCell::new(Vec::new()));
        let collected = Rc::clone(&events);
        cx.update(|window, cx| {
            window.activate_window();
            cx.subscribe(&sidebar, move |_, event: &SidebarEvent, _| {
                collected.borrow_mut().push(event.clone());
            })
            .detach();
        });
        cx.run_until_parked();
        (sidebar, events, cx)
    }

    fn publish(
        sidebar: &Entity<WorkspaceSidebar>,
        row: WorkspaceRowViewModel,
        cx: &mut VisualTestContext,
    ) {
        cx.update(|_, cx| {
            sidebar.update(cx, |sidebar, cx| {
                sidebar.set_rows(vec![row], Vec::new(), cx)
            });
        });
        cx.run_until_parked();
    }

    fn submitted_rename(events: &Renames) -> (String, Option<u64>) {
        events
            .borrow()
            .iter()
            .find_map(|event| match event {
                SidebarEvent::Rename { name, version, .. } => Some((name.clone(), *version)),
                _ => None,
            })
            .expect("the inline editor should have submitted its draft")
    }

    #[gpui::test]
    fn republishing_the_same_rows_should_not_redraw_the_sidebar(cx: &mut TestAppContext) {
        let (sidebar, _events, cx) = sidebar(cx);
        publish(&sidebar, row("Build", 0), cx);
        let notifications = std::rc::Rc::new(std::cell::Cell::new(0));
        cx.update(|_, cx| {
            let notifications = notifications.clone();
            cx.observe(&sidebar, move |_, _| {
                notifications.set(notifications.get() + 1)
            })
            .detach();
        });
        let set = |row: WorkspaceRowViewModel, cx: &mut VisualTestContext| {
            cx.update(|_, cx| {
                sidebar.update(cx, |sidebar, cx| {
                    sidebar.set_rows(vec![row], Vec::new(), cx)
                })
            })
        };

        assert!(!set(row("Build", 0), cx));
        cx.run_until_parked();
        assert_eq!(notifications.get(), 0);
        assert!(set(row("Theirs", 1), cx));
        cx.run_until_parked();
        assert_eq!(notifications.get(), 1);
    }

    #[gpui::test]
    fn a_rename_published_while_the_editor_is_open_should_not_overwrite_it(
        cx: &mut TestAppContext,
    ) {
        let (sidebar, events, cx) = sidebar(cx);
        publish(&sidebar, row("Build", 0), cx);

        right_click("workspace-row-1-active", cx);
        click("workspace-menu-row-rename", cx);
        assert!(
            cx.debug_bounds("workspace-rename-input").is_some(),
            "the inline editor should be open on the row"
        );

        // Another client renames the Workspace while this draft is still being typed.
        publish(&sidebar, row("Theirs", 1), cx);
        cx.simulate_keystrokes("cmd-a M i n e enter");
        cx.run_until_parked();

        assert_eq!(
            submitted_rename(&events),
            ("Mine".to_owned(), Some(0)),
            "the draft should carry the version the editor opened on, so the server refuses it \
             instead of overwriting the other client"
        );
    }

    #[gpui::test]
    fn a_rename_started_after_a_publication_should_carry_the_published_version(
        cx: &mut TestAppContext,
    ) {
        let (sidebar, events, cx) = sidebar(cx);
        publish(&sidebar, row("Build", 0), cx);
        publish(&sidebar, row("Theirs", 1), cx);

        right_click("workspace-row-1-active", cx);
        click("workspace-menu-row-rename", cx);
        cx.simulate_keystrokes("cmd-a M i n e enter");
        cx.run_until_parked();

        assert_eq!(
            submitted_rename(&events),
            ("Mine".to_owned(), Some(1)),
            "a draft started on the published name is fenced to that name"
        );
    }

    #[gpui::test]
    fn a_rename_of_a_workspace_this_window_owns_should_carry_no_shared_version(
        cx: &mut TestAppContext,
    ) {
        let (sidebar, events, cx) = sidebar(cx);
        let mut own = row("Build", 0);
        own.shared = None;
        publish(&sidebar, own, cx);

        right_click("workspace-row-1-active", cx);
        click("workspace-menu-row-rename", cx);
        cx.simulate_keystrokes("cmd-a M i n e enter");
        cx.run_until_parked();

        assert_eq!(
            submitted_rename(&events),
            ("Mine".to_owned(), None),
            "a Workspace this window owns has no server version to fence"
        );
    }
}

#[path = "retention_tests.rs"]
mod retention_tests;

#[path = "optimistic_tests.rs"]
mod optimistic;
