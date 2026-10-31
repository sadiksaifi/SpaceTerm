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
    let panes = ScriptedPanes::new(MultiServerHarness::session_factory());
    present_with(manager, origin, owner, edits, Some(attached), &panes, cx)
}

/// Presents `origin`'s Workspaces over `edits`, opening Panes through `panes`. Without
/// `attached`, this client has never chosen any, as a new client's first view.
fn present_with(
    manager: &Entity<WorkspaceManager>,
    origin: ServerOrigin,
    owner: Id,
    edits: &Rc<ScriptedEdits>,
    attached: Option<&[u8]>,
    panes: &Rc<ScriptedPanes>,
    cx: &mut VisualTestContext,
) -> ServerKey {
    let attached: Option<Vec<Id>> =
        attached.map(|attached| attached.iter().map(|number| [*number; 32]).collect());
    let key = cx.update(|window, cx| {
        manager.update(cx, |manager, cx| {
            let key = manager.present_server_workspaces(
                origin,
                SharedEditSource::new(Rc::clone(edits) as Rc<_>),
                SharedPaneSessions::with_opener(
                    Rc::clone(panes) as Rc<_>,
                    &PaneConstruction::testing(),
                ),
                window,
                cx,
            );
            if let Some(shared) = manager.shared.get_mut(key) {
                shared.identify(Rc::new(FixedOwner(owner)));
                if let Some(attached) = &attached {
                    shared.attach_for_test(attached);
                }
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

/// A new client's first view opens a Workspace for the person, but a window reopened with the
/// view the last one left opens none on its own: it takes no Pane control and starts no shell.
#[gpui::test]
fn a_reopened_window_never_opens_a_new_clients_first_view(cx: &mut TestAppContext) {
    let (_, offline) = closed_on_an_offline_servers_workspace(cx);
    offline.set_inventory(server_inventory(2, &[("Far", 5)]));
    offline.set_connected(true);
    let first_view = Rc::new(ScriptedEdits::new(Some(server_inventory(1, &[("Here", 1)]))));
    let local_panes = ScriptedPanes::new(MultiServerHarness::session_factory());

    let (manager, mut window) = open_window(cx);
    present_with(
        &manager,
        ServerOrigin::Local,
        LOCAL_OWNER,
        &first_view,
        None,
        &local_panes,
        &mut window,
    );
    assert_eq!(local_panes.pane(root_pane(1)).selections(), 0);
    present_penguin(&manager, &offline, &[5], &mut window);

    assert_eq!(active_name(&manager, &mut window), "Far");
    assert_eq!(local_panes.pane(root_pane(1)).selections(), 0);
}

const fn third_tab(number: u8) -> Id {
    [number + 70; 32]
}

/// Penguin's Build with a Root Tab, its second Tab and a third Tab.
fn three_tabs(revision: u64) -> Inventory {
    let mut inventory = with_second_tab(server_inventory(revision, &[("Build", 2)]), 2);
    let mut third = inventory.workspaces[0].tabs[1].clone();
    third.id = third_tab(2);
    third.root_pane = [2 + 120; 32];
    third.panes[0].id = [2 + 120; 32];
    inventory.workspaces[0].tabs.push(third);
    inventory
}

/// Presents penguin offline from what this client retained, `copy`, as a reopened window does
/// before the route connects.
fn present_retained_penguin(
    manager: &Entity<WorkspaceManager>,
    copy: spaceterm_server::client_view::RetainedInventory,
    cx: &mut VisualTestContext,
) -> ServerKey {
    let route = named_route("penguin");
    let key = cx.update(|window, cx| {
        manager.update(cx, |manager, cx| {
            let key = manager
                .present_direct_route(
                    DirectServer {
                        route,
                        name: "penguin".into(),
                    },
                    window,
                    cx,
                )
                .expect("the route is presented");
            if let Some(shared) = manager.shared.get_mut(key) {
                shared.identify(Rc::new(FixedOwner(route)));
                shared.retain_copy_for_test(copy);
            }
            manager.present_retained_copy(&route);
            manager.sync_shared_workspaces(key, window, cx);
            key
        })
    });
    cx.run_until_parked();
    key
}

/// Connects penguin's route over `edits`.
fn connect_penguin(manager: &Entity<WorkspaceManager>, edits: &Rc<ScriptedEdits>, cx: &mut VisualTestContext) {
    let route = named_route("penguin");
    let panes = ScriptedPanes::new(MultiServerHarness::session_factory());
    cx.update(|window, cx| {
        manager.update(cx, |manager, cx| {
            manager.follow_direct_route(&route, Some((Rc::clone(edits) as Rc<_>, panes as Rc<_>)));
            let key = manager
                .server_key(&crate::ui::ServerTarget::Direct(route))
                .expect("the route stays presented");
            manager.sync_shared_workspaces(key, window, cx);
        });
    });
    cx.run_until_parked();
}

/// A retained copy still lists the Tab another client closed while the window was closed. Once
/// the server publishes, the Root Tab is selected rather than the closed Tab's neighbor.
#[gpui::test]
fn a_tab_closed_while_offline_leaves_the_root_tab_once_the_server_publishes(
    cx: &mut TestAppContext,
) {
    let edits = Rc::new(ScriptedEdits::new(Some(three_tabs(1))));
    let (manager, mut window) = open_window(cx);
    present_penguin(&manager, &edits, &[2], &mut window);
    open_tab(&manager, second_tab(2), &mut window);
    assert_eq!(focused_pane(&manager, &mut window), Some(second_pane(2)));
    close(&manager, &mut window);

    let (manager, mut window) = open_window(cx);
    present_retained_penguin(
        &manager,
        spaceterm_server::client_view::RetainedInventory::retain(&three_tabs(1)),
        &mut window,
    );
    assert_eq!(focused_pane(&manager, &mut window), Some(second_pane(2)));

    let mut closed = three_tabs(2);
    closed.workspaces[0].tabs.remove(1);
    let live = Rc::new(ScriptedEdits::new(Some(closed)));
    connect_penguin(&manager, &live, &mut window);

    assert_eq!(active_name(&manager, &mut window), "Build");
    assert_eq!(focused_pane(&manager, &mut window), Some(root_pane(2)));
}

/// The first Workspace in display order is the first in the order the person keeps, which the
/// window may still be reading when the server publishes.
#[gpui::test]
fn a_killed_workspaces_fallback_waits_for_the_saved_workspace_order(cx: &mut TestAppContext) {
    let edits = closed_on_builds_second_tab(cx);
    edits.set_inventory(server_inventory(2, &[("Logs", 1), ("Tests", 3)]));

    let (manager, mut window) = open_window(cx);
    manager.update(&mut window, |manager, _| manager.read_display_order_later());
    present_penguin(&manager, &edits, &[1, 3], &mut window);
    assert_eq!(active_name(&manager, &mut window), "Logs");

    let reference = |number: u8| spaceterm_server::workspace_order::WorkspaceReference {
        owner: spaceterm_server::workspace_order::OwnerId::from_bytes(named_route("penguin")),
        workspace: [number; 32],
    };
    let mut saved = spaceterm_server::workspace_order::WorkspaceOrder::default();
    saved
        .record(&[reference(3), reference(1)])
        .expect("the order records");
    window.update(|window, cx| {
        manager.update(cx, |manager, cx| {
            manager.display_order_read(Some(saved), window, cx);
        });
    });
    window.run_until_parked();

    assert_eq!(active_name(&manager, &mut window), "Tests");
}

/// Clicking the Pane already focused is the person's choice too, so the remembered Workspace
/// never replaces it when its server returns.
#[gpui::test]
fn selecting_the_focused_pane_after_reopening_keeps_it_when_the_server_returns(
    cx: &mut TestAppContext,
) {
    let (local, offline) = closed_on_an_offline_servers_workspace(cx);

    let (manager, mut window) = open_window(cx);
    present(&manager, ServerOrigin::Local, LOCAL_OWNER, &local, &[1], &mut window);
    let key = present_penguin(&manager, &offline, &[5], &mut window);
    assert_eq!(focused_pane(&manager, &mut window), Some(root_pane(1)));
    window.update(|window, cx| {
        let tabs = manager
            .read(cx)
            .active_tab_manager()
            .expect("Here is active");
        let tab = tabs.read(cx).active_tab_view();
        tab.update(cx, |tab, cx| {
            tab.focus_shared_pane_explicitly(root_pane(1), window, cx);
        });
    });
    window.run_until_parked();

    offline.set_inventory(server_inventory(2, &[("Far", 5)]));
    offline.set_connected(true);
    sync(&manager, key, &mut window);
    assert_eq!(active_name(&manager, &mut window), "Here");
}

/// Whether the sidebar holds the keyboard, as the keyboard brought it there.
fn sidebar_has_keyboard(manager: &Entity<WorkspaceManager>, cx: &mut VisualTestContext) -> bool {
    cx.update(|window, cx| manager.read(cx).sidebar.read(cx).has_visible_focus(window))
}

/// Keyboard focus in the sidebar returns with the reopened window, on the same row, once the
/// window's Workspaces arrive and whatever its server presents in their Panes.
#[gpui::test]
fn a_reopened_window_returns_the_keyboard_to_the_sidebar(cx: &mut TestAppContext) {
    let edits = Rc::new(ScriptedEdits::new(Some(server_inventory(
        1,
        &[("Logs", 1), ("Build", 2)],
    ))));
    let (manager, mut window) = open_window(cx);
    present_penguin(&manager, &edits, &[1, 2], &mut window);
    select(&manager, "Build", &mut window);
    // Cmd+Shift+E, as the person moves the keyboard to the sidebar.
    window.update(|window, cx| {
        manager.update(cx, |manager, cx| manager.toggle_sidebar_focus(window, cx));
    });
    window.run_until_parked();
    assert!(sidebar_has_keyboard(&manager, &mut window));
    close(&manager, &mut window);

    let (manager, mut window) = open_window(cx);
    assert!(sidebar_has_keyboard(&manager, &mut window));
    present_penguin(&manager, &edits, &[1, 2], &mut window);
    window.run_until_parked();

    assert_eq!(active_name(&manager, &mut window), "Build");
    assert!(sidebar_has_keyboard(&manager, &mut window));
    window.simulate_keystrokes("up");
    window.run_until_parked();
    assert!(
        sidebar_has_keyboard(&manager, &mut window),
        "keys reach the sidebar"
    );
}

/// A window closed before its server returned leaves the next window the view it never got to
/// present.
#[gpui::test]
fn closing_before_the_server_returns_keeps_the_unfinished_view(cx: &mut TestAppContext) {
    let edits = closed_on_builds_second_tab(cx);
    let offline = Rc::new(ScriptedEdits::new(None));
    offline.set_connected(false);

    let (manager, mut window) = open_window(cx);
    present_penguin(&manager, &offline, &[1, 2], &mut window);
    assert_eq!(
        manager.read_with(&window, |manager, _| manager.workspaces.len()),
        0,
        "nothing is presented while the server is away"
    );
    close(&manager, &mut window);

    let (manager, mut window) = open_window(cx);
    present_penguin(&manager, &edits, &[1, 2], &mut window);

    assert_eq!(active_name(&manager, &mut window), "Build");
    assert_eq!(focused_pane(&manager, &mut window), Some(second_pane(2)));
}

/// Penguin's Build, whose Root Tab is split into a second Pane.
fn split_root_tab(revision: u64) -> Inventory {
    let mut inventory = server_inventory(revision, &[("Build", 2)]);
    let tab = &mut inventory.workspaces[0].tabs[0];
    let mut split = tab.panes[0].clone();
    split.id = [2 + 130; 32];
    split.source = Some(root_pane(2));
    tab.panes.push(split);
    inventory
}

/// Whether the Active Tab presents its Focused Pane zoomed.
fn zoomed(manager: &Entity<WorkspaceManager>, cx: &mut VisualTestContext) -> bool {
    manager.read_with(cx, |manager, cx| {
        manager.active_tab_manager().is_some_and(|tabs| {
            matches!(
                tabs.read(cx).active_tab_view().read(cx).zoom_state(),
                crate::domain::ZoomState::Zoomed(_)
            )
        })
    })
}

/// A zoom the person changes before the server publishes is newer than the remembered one, so
/// publication leaves it.
#[gpui::test]
fn a_zoom_changed_before_the_server_publishes_stays(cx: &mut TestAppContext) {
    let edits = Rc::new(ScriptedEdits::new(Some(split_root_tab(1))));
    let (manager, mut window) = open_window(cx);
    present_penguin(&manager, &edits, &[2], &mut window);
    assert!(!zoomed(&manager, &mut window));
    close(&manager, &mut window);

    let (manager, mut window) = open_window(cx);
    present_retained_penguin(
        &manager,
        spaceterm_server::client_view::RetainedInventory::retain(&split_root_tab(1)),
        &mut window,
    );
    // Cmd+Shift+Enter in the Root Tab.
    window.update(|window, cx| {
        let tab = manager
            .read(cx)
            .active_tab_manager()
            .expect("Build is active")
            .read(cx)
            .active_tab_view();
        tab.update(cx, |tab, cx| tab.toggle_zoom(window, cx));
    });
    window.run_until_parked();
    assert!(zoomed(&manager, &mut window));

    connect_penguin(
        &manager,
        &Rc::new(ScriptedEdits::new(Some(split_root_tab(2)))),
        &mut window,
    );
    assert!(zoomed(&manager, &mut window));
}
