//! A Workspace Window reopened while SpaceTerm keeps running presents the view the closed one
//! left. Anything that disappeared meanwhile falls back to the default without an error.

use std::rc::Rc;

use gpui::{Entity, TestAppContext, VisualTestContext};
use spaceterm_server::hierarchy::{Id, Inventory};

use super::{
    LOCAL_OWNER, MultiServerHarness, PaneConstruction, ServerKey, ServerOrigin, SidebarEvent,
    SidebarState, WorkspaceManager, named_route, server_inventory, workspace_adapters,
};
use crate::domain::PresentedWorkspace as _;
use crate::ui::shared_workspaces::testing::{FixedOwner, ScriptedEdits, ScriptedPanes};
use crate::ui::shared_workspaces::{DirectServer, SharedEditSource, SharedPaneSessions};

/// The Workspace `number` of [`server_inventory`] with a second Tab, whose id and Pane follow
/// from `number`.
fn with_second_tab(mut inventory: Inventory, number: u8) -> Inventory {
    let workspace = inventory
        .workspaces
        .iter_mut()
        .find(|workspace| workspace.id == [number; 32])
        .expect("the Workspace is published");
    let mut tab = workspace.tabs[0].clone();
    tab.id = second_tab(number);
    tab.root_pane = second_pane(number);
    tab.panes[0].id = second_pane(number);
    workspace.tabs.push(tab);
    inventory
}

const fn second_tab(number: u8) -> Id {
    [number + 60; 32]
}

const fn second_pane(number: u8) -> Id {
    [number + 110; 32]
}

const fn root_pane(number: u8) -> Id {
    [number + 100; 32]
}

/// A Workspace Window as SpaceTerm opens one, presenting the view the last closed one left.
fn open_window(cx: &mut TestAppContext) -> (Entity<WorkspaceManager>, VisualTestContext) {
    cx.update(crate::ui::init)
        .expect("UI initialization should succeed");
    let handle = cx.add_window(|window, cx| {
        crate::ui::updates::server_rows::testing::install_up_to_date(cx);
        WorkspaceManager::new_with_adapters(
            std::env::temp_dir(),
            workspace_adapters(),
            SidebarState::Expanded,
            window,
            cx,
        )
    });
    let manager = handle
        .update(cx, |manager, window, cx| {
            manager.focus(window, cx);
            if let Some(view) = crate::ui::remembered_view::take_workspace_window(cx) {
                manager.restore_remembered(view, window, cx);
            }
            cx.entity()
        })
        .expect("the window is open");
    let cx = VisualTestContext::from_window(handle.into(), cx);
    cx.run_until_parked();
    (manager, cx)
}

/// Presents `origin`'s Workspaces over `edits` with `attached` attached, as the server keeps
/// this client's attachments for every window.
fn present(
    manager: &Entity<WorkspaceManager>,
    origin: ServerOrigin,
    owner: Id,
    edits: &Rc<ScriptedEdits>,
    attached: &[u8],
    cx: &mut VisualTestContext,
) -> ServerKey {
    let attached: Vec<Id> = attached.iter().map(|number| [*number; 32]).collect();
    let key = cx.update(|window, cx| {
        manager.update(cx, |manager, cx| {
            let key = manager.present_server_workspaces(
                origin,
                SharedEditSource::new(Rc::clone(edits) as Rc<_>),
                SharedPaneSessions::with_opener(
                    ScriptedPanes::new(MultiServerHarness::session_factory()) as Rc<_>,
                    &PaneConstruction::testing(),
                ),
                window,
                cx,
            );
            if let Some(shared) = manager.shared.get_mut(key) {
                shared.identify(Rc::new(FixedOwner(owner)));
                shared.attach_for_test(&attached);
            }
            manager.sync_shared_workspaces(key, window, cx);
            key
        })
    });
    cx.run_until_parked();
    key
}

fn penguin() -> ServerOrigin {
    ServerOrigin::Direct(DirectServer {
        route: named_route("penguin"),
        name: "penguin".into(),
    })
}

fn present_penguin(
    manager: &Entity<WorkspaceManager>,
    edits: &Rc<ScriptedEdits>,
    attached: &[u8],
    cx: &mut VisualTestContext,
) -> ServerKey {
    present(
        manager,
        penguin(),
        named_route("penguin"),
        edits,
        attached,
        cx,
    )
}

fn sync(manager: &Entity<WorkspaceManager>, key: ServerKey, cx: &mut VisualTestContext) {
    cx.update(|window, cx| {
        manager.update(cx, |manager, cx| manager.sync_shared_workspaces(key, window, cx));
    });
    cx.run_until_parked();
}

fn named(manager: &Entity<WorkspaceManager>, name: &str, cx: &mut VisualTestContext) -> crate::domain::WorkspaceId {
    let name = name.to_owned();
    manager.read_with(cx, move |manager, _| {
        manager
            .workspaces
            .iter()
            .find(|workspace| workspace.name() == name)
            .map(|workspace| workspace.id())
            .expect("the Workspace is presented")
    })
}

/// Selects the Workspace `name`, as clicking its sidebar row does.
fn select(manager: &Entity<WorkspaceManager>, name: &str, cx: &mut VisualTestContext) {
    let workspace_id = named(manager, name, cx);
    cx.update(|window, cx| {
        manager.update(cx, |manager, cx| {
            manager.handle_sidebar_event(
                SidebarEvent::Activate {
                    workspace_id,
                    focus_pane: false,
                },
                window,
                cx,
            );
        });
    });
    cx.run_until_parked();
}

/// Opens the Tab `tab` of the Active Workspace, as clicking it does.
fn open_tab(manager: &Entity<WorkspaceManager>, tab: Id, cx: &mut VisualTestContext) {
    cx.update(|window, cx| {
        let active = manager
            .read(cx)
            .active_tab_manager()
            .expect("a Workspace is active");
        active.update(cx, |tabs, cx| tabs.open_accepted_shared(tab, window, cx));
    });
    cx.run_until_parked();
}

fn active_name(manager: &Entity<WorkspaceManager>, cx: &mut VisualTestContext) -> String {
    manager.read_with(cx, |manager, _| manager.workspaces.active().name().to_owned())
}

/// The server Pane the Active Tab focuses.
fn focused_pane(manager: &Entity<WorkspaceManager>, cx: &mut VisualTestContext) -> Option<Id> {
    manager.read_with(cx, |manager, cx| {
        manager
            .active_tab_manager()
            .and_then(|tabs| tabs.read(cx).focused_server_pane(cx))
    })
}

/// Closes the window as Cmd+Q does while SpaceTerm keeps running.
fn close(manager: &Entity<WorkspaceManager>, cx: &mut VisualTestContext) {
    cx.update(|window, cx| {
        manager.update(cx, |manager, cx| manager.request_window_close(window, cx));
    });
    cx.run_until_parked();
}

/// Penguin publishes Logs and Build, where Build has a second Tab. The window selects Build and
/// its second Tab, then closes.
fn closed_on_builds_second_tab(cx: &mut TestAppContext) -> Rc<ScriptedEdits> {
    let edits = Rc::new(ScriptedEdits::new(Some(with_second_tab(
        server_inventory(1, &[("Logs", 1), ("Build", 2)]),
        2,
    ))));
    let (manager, mut window) = open_window(cx);
    present_penguin(&manager, &edits, &[1, 2], &mut window);
    select(&manager, "Build", &mut window);
    open_tab(&manager, second_tab(2), &mut window);
    assert_eq!(focused_pane(&manager, &mut window), Some(second_pane(2)));
    close(&manager, &mut window);
    edits
}

#[gpui::test]
fn a_reopened_window_presents_the_workspace_and_tab_the_closed_one_left(
    cx: &mut TestAppContext,
) {
    let edits = closed_on_builds_second_tab(cx);

    let (manager, mut window) = open_window(cx);
    present_penguin(&manager, &edits, &[1, 2], &mut window);

    assert_eq!(active_name(&manager, &mut window), "Build");
    assert_eq!(focused_pane(&manager, &mut window), Some(second_pane(2)));
}

#[gpui::test]
fn a_workspace_killed_while_the_window_was_closed_selects_the_first_in_display_order(
    cx: &mut TestAppContext,
) {
    let edits = closed_on_builds_second_tab(cx);
    edits.set_inventory(server_inventory(2, &[("Logs", 1), ("Tests", 3)]));

    let (manager, mut window) = open_window(cx);
    present_penguin(&manager, &edits, &[1, 3], &mut window);

    let first = manager.read_with(&window, |manager, _| {
        manager
            .workspaces
            .iter()
            .next()
            .map(|workspace| workspace.name().to_owned())
    });
    assert_eq!(first.as_deref(), Some("Logs"));
    assert_eq!(active_name(&manager, &mut window), "Logs");
}

#[gpui::test]
fn a_tab_closed_while_the_window_was_closed_leaves_the_root_tab_selected(
    cx: &mut TestAppContext,
) {
    let edits = closed_on_builds_second_tab(cx);
    edits.set_inventory(server_inventory(2, &[("Logs", 1), ("Build", 2)]));

    let (manager, mut window) = open_window(cx);
    present_penguin(&manager, &edits, &[1, 2], &mut window);

    assert_eq!(active_name(&manager, &mut window), "Build");
    assert_eq!(focused_pane(&manager, &mut window), Some(root_pane(2)));
}

/// This computer's server publishes Here. Penguin publishes Far. The window selects Far and
/// closes, then penguin goes offline.
fn closed_on_an_offline_servers_workspace(
    cx: &mut TestAppContext,
) -> (Rc<ScriptedEdits>, Rc<ScriptedEdits>) {
    let local = Rc::new(ScriptedEdits::new(Some(server_inventory(1, &[("Here", 1)]))));
    let edits = Rc::new(ScriptedEdits::new(Some(server_inventory(1, &[("Far", 5)]))));
    let (manager, mut window) = open_window(cx);
    present(&manager, ServerOrigin::Local, LOCAL_OWNER, &local, &[1], &mut window);
    present_penguin(&manager, &edits, &[5], &mut window);
    select(&manager, "Far", &mut window);
    close(&manager, &mut window);
    let offline = Rc::new(ScriptedEdits::new(None));
    offline.set_connected(false);
    (local, offline)
}

#[gpui::test]
fn an_offline_servers_workspace_is_selected_when_its_server_returns(cx: &mut TestAppContext) {
    let (local, offline) = closed_on_an_offline_servers_workspace(cx);

    let (manager, mut window) = open_window(cx);
    present(&manager, ServerOrigin::Local, LOCAL_OWNER, &local, &[1], &mut window);
    let key = present_penguin(&manager, &offline, &[5], &mut window);
    assert_eq!(active_name(&manager, &mut window), "Here");

    offline.set_inventory(server_inventory(2, &[("Far", 5)]));
    offline.set_connected(true);
    sync(&manager, key, &mut window);
    assert_eq!(active_name(&manager, &mut window), "Far");
}

#[gpui::test]
fn a_selection_after_reopening_stays_when_the_offline_server_returns(cx: &mut TestAppContext) {
    let (local, offline) = closed_on_an_offline_servers_workspace(cx);
    let more = Rc::new(ScriptedEdits::new(Some(server_inventory(
        2,
        &[("Here", 1), ("There", 2)],
    ))));
    let _ = local;

    let (manager, mut window) = open_window(cx);
    present(&manager, ServerOrigin::Local, LOCAL_OWNER, &more, &[1, 2], &mut window);
    let key = present_penguin(&manager, &offline, &[5], &mut window);
    select(&manager, "There", &mut window);

    offline.set_inventory(server_inventory(2, &[("Far", 5)]));
    offline.set_connected(true);
    sync(&manager, key, &mut window);
    assert_eq!(active_name(&manager, &mut window), "There");
}

/// The Focused Pane's view in the Active Tab.
fn focused_view(
    manager: &Entity<WorkspaceManager>,
    cx: &mut VisualTestContext,
) -> Option<crate::ui::remembered_view::RememberedPane> {
    manager.read_with(cx, |manager, cx| {
        let tabs = manager.active_tab_manager()?;
        let tabs = tabs.read(cx);
        let focused = tabs.active_terminal_identity(cx);
        tabs.terminal_panes(cx)
            .find(|(tab, pane, _)| (*tab, *pane) == focused)
            .map(|(_, _, terminal)| terminal.remembered(cx))
    })
}

#[gpui::test]
fn a_reopened_window_presents_each_panes_font_size_and_find(cx: &mut TestAppContext) {
    let edits = Rc::new(ScriptedEdits::new(Some(with_second_tab(
        server_inventory(1, &[("Logs", 1), ("Build", 2)]),
        2,
    ))));
    let (manager, mut window) = open_window(cx);
    present_penguin(&manager, &edits, &[1, 2], &mut window);
    select(&manager, "Build", &mut window);
    open_tab(&manager, second_tab(2), &mut window);
    window.dispatch_action(crate::ui::IncreaseTerminalFontSize);
    window.dispatch_action(crate::ui::IncreaseTerminalFontSize);
    window.dispatch_action(crate::ui::OpenTerminalFind);
    window.run_until_parked();
    window.simulate_input("needle");
    window.run_until_parked();
    let left = crate::ui::remembered_view::RememberedPane {
        font_zoom: 2.0,
        find: Some("needle".to_owned()),
    };
    assert_eq!(focused_view(&manager, &mut window), Some(left.clone()));
    close(&manager, &mut window);

    let (manager, mut window) = open_window(cx);
    present_penguin(&manager, &edits, &[1, 2], &mut window);

    assert_eq!(focused_view(&manager, &mut window), Some(left));
    assert!(window.debug_bounds("terminal-find-bar").is_some());
}
