//! A paired route's Workspaces stay presented, from what this client retained, while the route
//! has no connection: before its first connection after launch and after Disconnect.

use std::rc::Rc;

use gpui::{TestAppContext, VisualTestContext};
use spaceterm_server::client_view::RetainedInventory;
use spaceterm_server::hierarchy::{Inventory, Mutation};

use super::{MultiServerHarness, ServerKey, named_route, server_inventory, settle_shared_panes};
use crate::direct_connections::DirectRouteId;
use crate::domain::{PresentedWorkspace as _, WorkspaceId};
use crate::ui::shared_workspaces::testing::{FixedOwner, ScriptedEdits, ScriptedPanes};
use crate::ui::shared_workspaces::{DirectServer, SharedRefusalReason, SharedSubmission};

/// `workspaces` as the server publishes them, each Pane titled by the program running in it.
fn titled_inventory(revision: u64, workspaces: &[(&str, u8)]) -> Inventory {
    use spaceterm_server::pane::metadata::{
        DirectoryProvenance, Freshness, Progress, PromptZone, Snapshot, TitleGlyph, TitleProvenance,
    };
    let mut inventory = server_inventory(revision, workspaces);
    for workspace in &mut inventory.workspaces {
        let name = workspace.name.clone();
        for pane in workspace.tabs.iter_mut().flat_map(|tab| &mut tab.panes) {
            pane.presentation = Some(Box::new(Snapshot {
                revision: 1,
                shell_name: "zsh".to_owned(),
                title: format!("vim {name}"),
                title_provenance: TitleProvenance::TerminalControl,
                directory: "/srv/work".to_owned(),
                directory_provenance: DirectoryProvenance::Osc7,
                user: None,
                host: None,
                home: None,
                freshness: Freshness::Live,
                finished_commands: 0,
                prompt_zone: PromptZone::Prompt,
                command: None,
                title_activity: false,
                title_glyph: TitleGlyph::Reported,
                progress: Progress::None,
            }));
        }
    }
    inventory
}

impl MultiServerHarness {
    /// Binds the saved route `name` at `route` with no connection, as launch does before the
    /// route's first connection.
    fn present_saved_route(
        &self,
        name: &str,
        route: DirectRouteId,
        retained: Option<RetainedInventory>,
        cx: &mut VisualTestContext,
    ) -> ServerKey {
        let key = cx.update(|window, cx| {
            self.manager.update(cx, |manager, cx| {
                let key = manager
                    .present_direct_route(
                        DirectServer {
                            route,
                            name: name.to_owned().into(),
                        },
                        window,
                        cx,
                    )
                    .expect("the route is presented");
                if let Some(shared) = manager.shared.get_mut(key) {
                    shared.identify(Rc::new(FixedOwner(route)));
                    match retained {
                        Some(copy) => shared.retain_copy_for_test(copy),
                        None => shared.retain_inventory_copies(),
                    }
                }
                manager.present_retained_copy(&route);
                manager.sync_shared_workspaces(key, window, cx);
                key
            })
        });
        settle_shared_panes(cx);
        key
    }

    /// Connects `route` over `edits`, which publishes `workspaces`, and attaches each one. Its
    /// Panes open through a fresh scripted opener.
    fn connect_route(
        &self,
        route: DirectRouteId,
        edits: &Rc<ScriptedEdits>,
        workspaces: &[(&str, u8)],
        cx: &mut VisualTestContext,
    ) -> Rc<ScriptedPanes> {
        let panes = ScriptedPanes::new(Self::session_factory());
        let key = self.follow(route, Some((Rc::clone(edits), Rc::clone(&panes))), cx);
        self.attach(key, workspaces, cx);
        panes
    }

    /// Disconnects `route`, as the person choosing Disconnect does.
    fn disconnect_route(&self, route: DirectRouteId, cx: &mut VisualTestContext) {
        self.follow(route, None, cx);
    }

    fn follow(
        &self,
        route: DirectRouteId,
        connection: Option<(Rc<ScriptedEdits>, Rc<ScriptedPanes>)>,
        cx: &mut VisualTestContext,
    ) -> ServerKey {
        let key = cx.update(|window, cx| {
            self.manager.update(cx, |manager, cx| {
                manager.follow_direct_route(
                    &route,
                    connection.map(|(edits, panes)| (edits as Rc<_>, panes as Rc<_>)),
                );
                let key = manager
                    .server_key(&crate::ui::ServerTarget::Direct(route))
                    .expect("the route stays presented");
                manager.sync_shared_workspaces(key, window, cx);
                key
            })
        });
        settle_shared_panes(cx);
        key
    }

    /// The local Workspace presenting the server Workspace `server` at `key`.
    fn local(&self, key: ServerKey, server: u8, cx: &mut VisualTestContext) -> Option<WorkspaceId> {
        self.manager.read_with(cx, |manager, _| {
            manager.shared.get(key)?.local([server; 32])
        })
    }

    /// The row status `key`'s server Workspace `server` shows in place of its directory.
    fn row_status(
        &self,
        key: ServerKey,
        server: u8,
        cx: &mut VisualTestContext,
    ) -> Option<&'static str> {
        self.manager.read_with(cx, |manager, _| {
            manager
                .shared
                .get(key)?
                .row_status([server; 32], None)
                .map(|status| status.text)
        })
    }

    /// Whether this client's attachments let the route at `key` open the server Pane `pane`.
    fn permits(&self, key: ServerKey, pane: u8, cx: &mut VisualTestContext) -> bool {
        self.manager.read_with(cx, |manager, _| {
            manager
                .shared
                .get(key)
                .is_some_and(|shared| shared.permits([pane; 32]))
        })
    }

    /// Republishes `edits`' inventory as `inventory` and reads it.
    fn republish(
        &self,
        key: ServerKey,
        edits: &ScriptedEdits,
        inventory: Inventory,
        cx: &mut VisualTestContext,
    ) {
        edits.set_inventory(inventory);
        cx.update(|window, cx| {
            self.manager.update(cx, |manager, cx| {
                manager.sync_shared_workspaces(key, window, cx);
            });
        });
        settle_shared_panes(cx);
    }

    /// The Active Workspace's Active Tab title and how many Tabs it has.
    fn active_tabs(&self, cx: &mut VisualTestContext) -> (String, usize) {
        self.manager.read_with(cx, |manager, cx| {
            let tabs = manager.workspaces.active().payload().read(cx);
            (
                tabs.active_tab_view().read(cx).tab_title().to_string(),
                tabs.tab_ids().len(),
            )
        })
    }

    fn activate(&self, workspace: WorkspaceId, cx: &mut VisualTestContext) {
        cx.update(|window, cx| {
            self.manager.update(cx, |manager, cx| {
                manager.activate_workspace(workspace, window, cx);
            });
        });
        settle_shared_panes(cx);
    }

    /// Asks the route at `key` for a new Tab in its Workspace `server`.
    fn create_tab(
        &self,
        key: ServerKey,
        server: u8,
        cx: &mut VisualTestContext,
    ) -> SharedSubmission {
        self.manager.update(cx, |manager, _| {
            let shared = manager.shared.get_mut(key).expect("the route is presented");
            shared.edits().submit(Mutation::CreateTab {
                workspace: [server; 32],
                source: Some([server + 50; 32]),
            })
        })
    }
}

fn offline_notice_shown(cx: &mut VisualTestContext) -> bool {
    spaceterm_ui::a11y_testing::A11yTree::read(cx)
        .in_order()
        .iter()
        .any(|node| {
            node["aria"]["label"].as_str() == Some(crate::ui::server_pane::OFFLINE_TITLE)
                || node["aria"]["value"].as_str() == Some(crate::ui::server_pane::OFFLINE_TITLE)
        })
}

/// After launch, before the route's first connection, the Workspaces and Tabs it retained are
/// in the sidebar, marked unavailable. A person navigates them, while shared edits stay
/// unavailable and no Pane reaches a Terminal Session.
#[gpui::test]
fn a_saved_route_should_present_what_it_retained_before_it_connects(cx: &mut TestAppContext) {
    let (harness, cx) = MultiServerHarness::new(cx);
    harness.publish(ServerKey::LOCAL, &harness.local, &[("Here", 1)], cx);
    let route = named_route("penguin");
    let retained = RetainedInventory::retain(&titled_inventory(9, &[("Far", 1), ("Farther", 2)]));

    let key = harness.present_saved_route("penguin", route, Some(retained), cx);
    let farther = harness.local(key, 2, cx).expect("Farther is presented");
    harness.activate(farther, cx);

    assert_eq!(harness.names(cx), ["Default", "Here", "Far", "Farther"]);
    assert_eq!(harness.active_name(cx), "Farther");
    // Storage keeps the shell's name, never the title a program set.
    assert_eq!(harness.active_tabs(cx), ("zsh".to_owned(), 1));
    assert_eq!(harness.row_status(key, 2, cx), Some("Not connected"));
    assert!(offline_notice_shown(cx), "the Pane says it isn't connected");
    assert_eq!(
        harness.create_tab(key, 2, cx),
        SharedSubmission::Refused(SharedRefusalReason::Offline)
    );
    assert!(harness.manager.read_with(cx, |manager, _| {
        manager
            .shared
            .get(key)
            .is_some_and(|shared| shared.presents_retained_copy())
            && !manager.direct_route_connected(&route)
    }));
}

/// Disconnect keeps the route's Workspaces where they were, marked unavailable, and the next
/// connection reconciles them with the server's inventory in place: a Workspace the server
/// removed leaves, a new Tab joins, and the Workspace the person was looking at stays active.
#[gpui::test]
fn disconnect_should_keep_the_routes_workspaces_until_reconnecting_reconciles_them(
    cx: &mut TestAppContext,
) {
    let (harness, cx) = MultiServerHarness::new(cx);
    let route = named_route("penguin");
    let key = harness.present_saved_route("penguin", route, None, cx);
    let published = titled_inventory(harness.next_revision(), &[("Far", 1), ("Farther", 2)]);
    let edits = Rc::new(ScriptedEdits::new(Some(published.clone())));
    let first = harness.connect_route(route, &edits, &[("Far", 1), ("Farther", 2)], cx);
    let (far, farther) = (harness.local(key, 1, cx), harness.local(key, 2, cx));
    harness.activate(farther.expect("Farther is presented"), cx);
    let opened_connected = first.opened().len();

    harness.disconnect_route(route, cx);
    let disconnected = (
        harness.names(cx),
        harness.local(key, 1, cx),
        harness.local(key, 2, cx),
        harness.active_name(cx),
        harness.row_status(key, 2, cx),
        harness.active_tabs(cx),
        offline_notice_shown(cx),
        harness.create_tab(key, 2, cx),
    );
    let retained = harness
        .manager
        .read_with(cx, |manager, _| manager.shared.get(key)?.retained_copy());

    let mut reconciled = titled_inventory(harness.next_revision(), &[("Farther", 2)]);
    let mut second_tab = reconciled.workspaces[0].tabs[0].clone();
    second_tab.id = [60; 32];
    second_tab.root_pane = [160; 32];
    second_tab.panes[0].id = [160; 32];
    reconciled.workspaces[0].tabs.push(second_tab);
    let returned = Rc::new(ScriptedEdits::new(Some(reconciled)));
    let second = harness.connect_route(route, &returned, &[("Farther", 2)], cx);

    assert!(opened_connected > 0);
    assert_eq!(
        disconnected,
        (
            vec!["Default".to_owned(), "Far".to_owned(), "Farther".to_owned()],
            far,
            farther,
            "Farther".to_owned(),
            Some("Not connected"),
            ("vim Farther".to_owned(), 1),
            true,
            SharedSubmission::Refused(SharedRefusalReason::Offline),
        )
    );
    assert_eq!(
        retained,
        Some(RetainedInventory::retain(&published)),
        "Disconnect keeps what the server last confirmed"
    );
    assert_eq!(harness.names(cx), ["Default", "Farther"]);
    assert_eq!(
        harness.local(key, 2, cx),
        farther,
        "Farther keeps its place"
    );
    assert_eq!(harness.active_name(cx), "Farther");
    assert_eq!(harness.active_tabs(cx).1, 2);
    assert_eq!(harness.row_status(key, 2, cx), None);
    assert!(
        !second.opened().is_empty(),
        "each Pane opens again through the new connection"
    );
    assert!(!offline_notice_shown(cx));
    assert_eq!(harness.create_tab(key, 2, cx), SharedSubmission::Submitted);
    assert_eq!(edits.submitted(), Vec::new());
}

/// Removing the route's Saved Connection removes its retained copy, so nothing presents it
/// again.
#[gpui::test]
fn discarding_a_routes_copy_should_leave_nothing_to_present(cx: &mut TestAppContext) {
    let (harness, cx) = MultiServerHarness::new(cx);
    let route = named_route("penguin");
    let retained = RetainedInventory::retain(&titled_inventory(9, &[("Far", 1)]));
    let key = harness.present_saved_route("penguin", route, Some(retained), cx);

    let copy = harness.manager.update(cx, |manager, _| {
        let shared = manager.shared.get_mut(key)?;
        shared.discard_retained_copy();
        shared.retained_copy()
    });

    assert_eq!(copy, None);
}

/// The most common relaunch: the server's inventory is unchanged since this client retained it,
/// so it arrives at the copy's revision. It still reads the attachments and opens each Pane.
#[gpui::test]
fn a_server_publishing_the_retained_revision_should_open_the_routes_panes(cx: &mut TestAppContext) {
    let (harness, cx) = MultiServerHarness::new(cx);
    let route = named_route("penguin");
    let published = titled_inventory(9, &[("Far", 1), ("Farther", 2)]);
    let key = harness.present_saved_route(
        "penguin",
        route,
        Some(RetainedInventory::retain(&published)),
        cx,
    );
    harness.activate(harness.local(key, 1, cx).expect("Far is presented"), cx);
    let before = (harness.permits(key, 101, cx), offline_notice_shown(cx));

    // A connection publishes its inventory once the server admits it, after the route follows it.
    let edits = Rc::new(ScriptedEdits::new(None));
    let panes = harness.connect_route(route, &edits, &[], cx);
    harness.republish(key, &edits, published, cx);

    assert_eq!(
        before,
        (false, true),
        "a relaunched client permits no Pane yet"
    );
    assert_eq!(harness.names(cx), ["Default", "Far", "Farther"]);
    assert!(harness.permits(key, 101, cx) && harness.permits(key, 102, cx));
    assert!(
        !panes.opened().is_empty(),
        "the Panes open through the connection"
    );
    assert!(!offline_notice_shown(cx));
    assert_eq!(harness.row_status(key, 1, cx), None);
    assert_eq!(harness.create_tab(key, 1, cx), SharedSubmission::Submitted);
}

/// Reconnecting to a server that changed nothing while this client was disconnected opens the
/// Panes again at the same revision.
#[gpui::test]
fn reconnecting_at_the_same_revision_should_open_the_routes_panes_again(cx: &mut TestAppContext) {
    let (harness, cx) = MultiServerHarness::new(cx);
    let route = named_route("penguin");
    let key = harness.present_saved_route("penguin", route, None, cx);
    let published = titled_inventory(harness.next_revision(), &[("Far", 1)]);
    let edits = Rc::new(ScriptedEdits::new(Some(published.clone())));
    harness.connect_route(route, &edits, &[("Far", 1)], cx);
    harness.disconnect_route(route, cx);
    let disconnected = offline_notice_shown(cx);

    let returned = Rc::new(ScriptedEdits::new(None));
    let panes = harness.connect_route(route, &returned, &[], cx);
    harness.republish(key, &returned, published, cx);

    assert!(disconnected);
    assert_eq!(harness.names(cx), ["Default", "Far"]);
    assert!(harness.permits(key, 101, cx));
    assert!(!panes.opened().is_empty());
    assert!(!offline_notice_shown(cx));
    assert_eq!(harness.create_tab(key, 1, cx), SharedSubmission::Submitted);
}

/// A connection that has not published its inventory yet leaves the retained copy presented and
/// shared edits unavailable, so no edit is asked against what the server may have changed.
#[gpui::test]
fn a_connection_should_take_edits_only_once_the_server_publishes(cx: &mut TestAppContext) {
    let (harness, cx) = MultiServerHarness::new(cx);
    let route = named_route("penguin");
    let published = titled_inventory(9, &[("Far", 1)]);
    let key = harness.present_saved_route(
        "penguin",
        route,
        Some(RetainedInventory::retain(&published)),
        cx,
    );

    let edits = Rc::new(ScriptedEdits::new(None));
    harness.connect_route(route, &edits, &[], cx);
    let waiting = (
        harness.names(cx),
        harness.row_status(key, 1, cx),
        harness.create_tab(key, 1, cx),
    );
    harness.republish(key, &edits, published, cx);

    assert_eq!(
        waiting,
        (
            vec!["Default".to_owned(), "Far".to_owned()],
            Some("Not connected"),
            SharedSubmission::Refused(SharedRefusalReason::Offline),
        )
    );
    assert_eq!(harness.row_status(key, 1, cx), None);
    assert!(harness.permits(key, 101, cx));
    assert_eq!(harness.create_tab(key, 1, cx), SharedSubmission::Submitted);
    assert_eq!(edits.submitted().len(), 1);
}

/// A title a program sets changes the server's inventory often. The copy in storage changes only
/// when the Workspaces, Tabs, Panes, Splits or names do.
#[gpui::test]
fn only_a_structural_change_should_write_the_routes_copy(cx: &mut TestAppContext) {
    let (harness, cx) = MultiServerHarness::new(cx);
    let route = named_route("penguin");
    let key = harness.present_saved_route("penguin", route, None, cx);
    let first = titled_inventory(harness.next_revision(), &[("Far", 1)]);
    let edits = Rc::new(ScriptedEdits::new(Some(first.clone())));
    harness.connect_route(route, &edits, &[("Far", 1)], cx);
    let saved = |cx: &mut VisualTestContext| {
        harness.manager.read_with(cx, |manager, _| {
            manager
                .shared
                .get(key)
                .and_then(|shared| shared.saved_copy().cloned())
        })
    };
    let written = saved(cx);

    let mut retitled = titled_inventory(harness.next_revision(), &[("Far", 1)]);
    if let Some(presentation) = retitled.workspaces[0].tabs[0].panes[0]
        .presentation
        .as_mut()
    {
        presentation.title = "ssh tester@server".to_owned();
    }
    harness.republish(key, &edits, retitled, cx);
    let after_title = saved(cx);
    let renamed = titled_inventory(harness.next_revision(), &[("Further", 1)]);
    harness.republish(key, &edits, renamed.clone(), cx);

    assert_eq!(written, Some(RetainedInventory::retain(&first)));
    assert_eq!(after_title, written, "a new title writes nothing");
    assert_eq!(saved(cx), Some(RetainedInventory::retain(&renamed)));
}

/// Only Settings is open when a paired server refuses its New Worktree Location change, and the
/// person has not answered the failure Settings shows when the Workspace Window presents that
/// server. The window leaves the failure to Settings, and alerts it only once Settings gives it
/// up.
#[gpui::test]
fn a_window_should_leave_a_failure_settings_still_shows_to_settings(cx: &mut TestAppContext) {
    use spaceterm_server::hierarchy::Outcome;
    use spaceterm_server::worktree::{Error, Operation};

    const ALERT: &str = "modal-action-server-worktree-location-failed-ok";
    let (harness, cx) = MultiServerHarness::new(cx);
    let route = named_route("penguin");
    let edits = Rc::new(ScriptedEdits::new(Some(titled_inventory(
        harness.next_revision(),
        &[("Far", 1)],
    ))));
    edits.set_worktree_writes(true);
    // Settings writes through the route's writer and keeps the answer it shows.
    let answer = cx.update(|_, cx| {
        crate::ui::server_worktrees::testing::present_direct(
            route,
            Rc::clone(&edits),
            crate::ui::server_worktrees::testing::ScriptedReads::new("~/{branch}"),
            cx,
        );
        let client = crate::ui::server_worktrees::Client::direct(&route, cx).expect("a writer");
        let answer = client.source.owner();
        client
            .write_owned(
                Operation::SetLocation {
                    version: 0,
                    template: "/Volumes/Locked/{branch}".into(),
                },
                answer.ticket(),
                cx,
            )
            .detach();
        answer
    });
    edits.answer(Outcome::WorktreeFailed(Error::Unavailable));
    cx.executor()
        .advance_clock(std::time::Duration::from_millis(50));
    cx.run_until_parked();

    harness.present_saved_route("penguin", route, None, cx);
    harness.connect_route(route, &edits, &[("Far", 1)], cx);
    let alerted_while_settings_shows = cx.debug_bounds(ALERT).is_some();
    drop(answer);
    cx.run_until_parked();

    assert!(
        !alerted_while_settings_shows,
        "Settings still shows the failure"
    );
    assert!(
        cx.debug_bounds(ALERT).is_some(),
        "the window presents it once Settings gives it up"
    );
}
