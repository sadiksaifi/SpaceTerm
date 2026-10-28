//! The server picker, All Servers and the selected server's card, driven through a scripted local
//! server and a registry of saved paired servers that reaches no network.

use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui::{Entity, Modifiers, TestAppContext, VisualTestContext};
use spaceterm_ui::a11y_testing::{A11yTree, perform};

use super::super::direct_routes::UNPAIR_SELF_CONFIRM_SELECTOR;
use super::super::servers::testing::{ScriptedServer, connected};
use super::super::servers::{LocalServerControl, OfferedLocalServer, OfferedServer};
use super::ServerChoice;
use crate::appearance::Appearance;
use crate::direct_connections::{DirectConnections, DirectRouteId, DirectRouteStatus};
use crate::platform::appearance::testing::RecordingAppearancePlatform;
use crate::platform::window_movement::RecordingOperatingSystemWindowDragPlatform;
use crate::server_context::{LastKnownFacts, ObservedServerStatus, ServerKey};
use crate::settings::SettingsDocument;
use crate::settings::storage::testing::MemoryStorage;
use crate::ui::appearance_runtime;
use crate::ui::direct_servers::DirectServerAction;
use crate::ui::settings_window::{SettingsSectionId, SettingsWindow};

const NAVIGATION_SELECTOR: &str = "settings-navigation-settings-section-servers";

/// Saved paired servers and their routes, in the order the registry lists them.
pub(in crate::ui::settings_window) type Paired = (DirectConnections, Vec<DirectRouteId>);

/// Paired servers named `names`, saved and disconnected as after a relaunch, each at its own
/// address. Nothing starts a connection.
pub(in crate::ui::settings_window) fn disconnected(names: &[&str]) -> Paired {
    let endpoints: Vec<String> = (10..)
        .take(names.len())
        .map(|host| format!("quic://192.168.1.{host}:9231"))
        .collect();
    let routes: Vec<(&str, &str)> = names
        .iter()
        .copied()
        .zip(endpoints.iter().map(String::as_str))
        .collect();
    crate::direct_connections::tests::disconnected_routes_at(&routes)
}

/// Paired servers named `names`, each connected over a handle that reaches no network. A test
/// presents each server's writer itself.
pub(in crate::ui::settings_window) fn connected_paired(
    names: &[&str],
    cx: &mut TestAppContext,
) -> Paired {
    // The registry connects each route on its own thread.
    cx.executor().allow_parking();
    let (direct, routes) = crate::direct_connections::tests::connected_routes(names);
    let deadline = Instant::now() + Duration::from_secs(5);
    while !routes.iter().all(|route| direct.server(route).is_some()) {
        assert!(Instant::now() < deadline, "the paired servers connect");
        std::thread::sleep(Duration::from_millis(2));
    }
    (direct, routes)
}

pub(in crate::ui::settings_window) struct Servers<'a> {
    pub(in crate::ui::settings_window) window: Entity<SettingsWindow>,
    pub(in crate::ui::settings_window) direct: DirectConnections,
    pub(in crate::ui::settings_window) routes: Vec<DirectRouteId>,
    pub(in crate::ui::settings_window) storage: Arc<MemoryStorage>,
    pub(in crate::ui::settings_window) cx: &'a mut VisualTestContext,
}

impl Servers<'_> {
    /// Whether every saved route is still disconnected, so nothing reached any server.
    fn all_disconnected(&self) -> bool {
        self.routes
            .iter()
            .all(|route| self.direct.status(route) == Some(DirectRouteStatus::Disconnected))
    }
}

/// Opens Settings on Servers with the local server when `local` is set and each of `names` saved
/// as a disconnected paired server.
pub(in crate::ui::settings_window) fn open_servers<'a>(
    local: bool,
    names: &[&str],
    cx: &'a mut TestAppContext,
) -> Servers<'a> {
    open_with(local, disconnected(names), cx)
}

/// Opens Settings on Servers with the local server when `local` is set and `paired` as the
/// desktop's direct registry.
pub(in crate::ui::settings_window) fn open_with<'a>(
    local: bool,
    paired: Paired,
    cx: &'a mut TestAppContext,
) -> Servers<'a> {
    let (direct, routes) = paired;
    let storage = MemoryStorage::with_document(&SettingsDocument::default());
    let settings = crate::settings::Settings::load(storage.clone());
    let platform = RecordingAppearancePlatform::default();
    platform.set_system_appearance(Some(Appearance::Dark));
    let registry = direct.clone();
    cx.update(|cx| {
        appearance_runtime::install(settings, Rc::new(platform), cx)
            .expect("appearance runtime should install");
        crate::ui::init(cx).expect("UI initialization should succeed");
        crate::ui::direct_servers::install(registry, cx);
    });
    let offered: OfferedLocalServer = local.then(|| OfferedServer {
        connection: Ok(ScriptedServer::new(Ok(connected(1))) as Arc<dyn LocalServerControl>),
        service: None,
        update: None,
    });
    let (window, cx) = cx.add_window_view(|window, cx| {
        SettingsWindow::new_with_capabilities(
            Rc::new(RecordingOperatingSystemWindowDragPlatform::default()),
            Default::default(),
            None,
            offered,
            window,
            cx,
        )
    });
    cx.update(|window, _| window.activate_window());
    cx.run_until_parked();
    click(NAVIGATION_SELECTOR, cx);
    Servers {
        window,
        direct,
        routes,
        storage,
        cx,
    }
}

pub(in crate::ui::settings_window) fn click(selector: &str, cx: &mut VisualTestContext) {
    let selector: &'static str = Box::leak(selector.to_owned().into_boxed_str());
    // Every step but the Server row's one button lives in its ⋯ menu.
    if cx.debug_bounds(selector).is_none()
        && cx
            .debug_bounds(super::super::server_card::MENU_SELECTOR)
            .is_some()
    {
        click_at(super::super::server_card::MENU_SELECTOR, cx);
    }
    click_at(selector, cx);
}

fn click_at(selector: &'static str, cx: &mut VisualTestContext) {
    let position = cx
        .debug_bounds(selector)
        .unwrap_or_else(|| panic!("{selector} was not rendered"))
        .center();
    cx.simulate_mouse_move(position, None, Modifiers::none());
    cx.simulate_click(position, Modifiers::none());
    cx.run_until_parked();
}

/// Whether `selector` is on the page, or offered in the Server row's ⋯ menu. It opens the menu
/// to look and closes it again without choosing anything.
fn rendered(selector: &str, cx: &mut VisualTestContext) -> bool {
    let selector: &'static str = Box::leak(selector.to_owned().into_boxed_str());
    if cx.debug_bounds(selector).is_some() {
        return true;
    }
    if cx
        .debug_bounds(super::super::server_card::MENU_SELECTOR)
        .is_none()
    {
        return false;
    }
    click_at(super::super::server_card::MENU_SELECTOR, cx);
    let offered = cx.debug_bounds(selector).is_some();
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    offered
}

/// Waits for the registry's workers, repainting as their results arrive.
pub(in crate::ui::settings_window) fn settle(
    cx: &mut VisualTestContext,
    mut done: impl FnMut(&mut VisualTestContext) -> bool,
) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        cx.run_until_parked();
        if done(cx) {
            return;
        }
        assert!(Instant::now() < deadline, "the registry settles");
        std::thread::sleep(Duration::from_millis(2));
    }
}

/// The servers All Servers offers, in order, by the name assistive technology hears.
fn listed(cx: &mut VisualTestContext) -> Vec<(String, bool)> {
    let tree = A11yTree::read(cx);
    // The card's heading shares the list's name, so the list is found by its role.
    let list = tree
        .with_role("ListBox")
        .into_iter()
        .find(|node| node["aria"]["label"] == super::SERVER_LIST_NAME)
        .expect("All Servers lists the servers");
    tree.children(list)
        .into_iter()
        .filter(|node| node["aria"]["role"] == "ListBoxOption")
        .map(|node| {
            (
                node["aria"]["label"].as_str().unwrap().to_owned(),
                node["aria"]["selected"] == true,
            )
        })
        .collect()
}

/// The name the test desktop's wording gives the local server.
fn local_name() -> &'static str {
    crate::desktop_profile::testing_presentation()
        .wording()
        .this_computer
}

fn select(name: &str, cx: &mut VisualTestContext) {
    let tree = A11yTree::read(cx);
    let item = tree.node(name);
    assert_eq!(item["aria"]["role"], "ListBoxOption");
    perform(cx, item, gpui::accesskit::Action::Click);
    cx.run_until_parked();
}

/// What the card's Status row says about the selected server.
fn status(servers: &Servers<'_>) -> String {
    servers.window.read_with(servers.cx, |settings, _| {
        settings
            .selected_server()
            .map(|choice| settings.server_state(&choice).state.to_string())
            .unwrap_or_default()
    })
}

/// The footnote under the server card, which says how to change rows that can't change now.
fn footnote(servers: &Servers<'_>) -> String {
    servers.window.read_with(servers.cx, |settings, cx| {
        settings
            .server_card_footnote(cx)
            .map(|footnote| footnote.to_string())
            .unwrap_or_default()
    })
}

fn selected(cx: &mut VisualTestContext) -> ServerKey {
    cx.update(|_, cx| super::super::server_selection::selected(cx))
}

#[gpui::test]
fn all_servers_names_this_computer_first_then_each_paired_server(cx: &mut TestAppContext) {
    let servers = open_servers(true, &["penguin", "build"], cx);

    assert_eq!(
        listed(servers.cx),
        vec![
            (local_name().to_owned(), true),
            ("penguin".to_owned(), false),
            ("build".to_owned(), false),
        ],
        "the local server leads and is selected; paired servers follow"
    );
    let tree = A11yTree::read(servers.cx);
    assert!(
        tree.node(local_name())["aria"]["description"]
            .as_str()
            .is_some_and(|description| description.contains("Running \u{b7} this account")),
        "each server says how it reads now"
    );
    assert_eq!(
        tree.node("penguin")["aria"]["description"],
        "quic://192.168.1.10:9231, Not connected",
        "a paired server names its address and its short status"
    );
}

#[gpui::test]
fn the_picker_and_all_servers_share_one_selection_that_connects_nothing(cx: &mut TestAppContext) {
    let servers = open_servers(true, &["penguin"], cx);
    assert_eq!(
        A11yTree::read(servers.cx).node(super::PICKER_NAME)["aria"]["value"],
        local_name()
    );

    select("penguin", servers.cx);

    let tree = A11yTree::read(servers.cx);
    let picker = tree.node(super::PICKER_NAME);
    assert_eq!(picker["aria"]["role"], "ComboBox");
    assert_eq!(picker["aria"]["value"], "penguin");
    assert_eq!(picker["aria"]["description"], "Choose or add a server");
    assert_eq!(
        selected(servers.cx),
        ServerChoice::Direct(servers.routes[0]).key(),
        "the choice is retained for Git and the next launch"
    );
    assert_eq!(status(&servers), "Not connected");
    assert!(
        servers.all_disconnected(),
        "choosing a server connects to nothing"
    );

    click("settings-navigation-settings-section-git", servers.cx);
    assert_eq!(
        A11yTree::read(servers.cx).node(super::PICKER_NAME)["aria"]["value"],
        "penguin",
        "Git presents the same server"
    );
}

/// A paired server SpaceTerm isn't connected to shows its rows and says how to change them.
#[gpui::test]
fn a_disconnected_paired_server_offers_connect_and_dims_its_settings(cx: &mut TestAppContext) {
    let servers = open_servers(true, &["penguin"], cx);
    select("penguin", servers.cx);

    for row in [
        "settings-row-server-status",
        DirectServerAction::Connect.selector(),
    ] {
        assert!(rendered(row, servers.cx), "{row} presents penguin");
    }
    // A paired server is administered on its own computer.
    for row in [
        "settings-row-end-all-sessions",
        "settings-row-network-access",
    ] {
        assert!(
            !rendered(row, servers.cx),
            "{row} stays with penguin's computer"
        );
    }
    assert!(
        footnote(&servers).contains("Connect to penguin to change these settings."),
        "got {:?}",
        footnote(&servers)
    );
    let network = servers.window.read_with(servers.cx, |settings, cx| {
        settings.network_access_footnote(cx)
    });
    assert_eq!(
        network, None,
        "penguin's network access isn't presented here"
    );
    assert!(servers.all_disconnected());
}

/// Server administration stays on the server's own computer: only this computer's server
/// presents Start Automatically, Network Access, Devices and End All Sessions.
#[test]
fn only_this_computers_server_presents_administration_rows() {
    use super::super::catalog::SettingsRowId;

    let paired = ServerChoice::Direct([1; 32]);
    for row in [
        SettingsRowId::ServerAutoStart,
        SettingsRowId::NetworkAccess,
        SettingsRowId::PairedDevices,
        SettingsRowId::EndAllSessions,
    ] {
        assert!(
            super::row_presents(row, Some(&ServerChoice::Local)),
            "{row:?}"
        );
        assert!(!super::row_presents(row, Some(&paired)), "{row:?}");
    }
}

#[gpui::test]
fn the_local_server_offers_no_removal_or_pairing_ceremony(cx: &mut TestAppContext) {
    let servers = open_servers(true, &["penguin"], cx);

    let tree = A11yTree::read(servers.cx);
    assert!(
        tree.with_role("Button")
            .iter()
            .all(|button| !button["aria"]["label"]
                .as_str()
                .is_some_and(|label| label.contains("Remove") || label.contains("Unpair"))),
        "the account-local server has nothing to remove or unpair"
    );
    assert!(!rendered("settings-server-unpair", servers.cx));
}

#[gpui::test]
fn the_arrow_keys_move_the_selection_while_the_list_has_focus(cx: &mut TestAppContext) {
    let servers = open_servers(true, &["build", "penguin"], cx);
    servers.cx.update(|window, cx| {
        let focus = servers.window.read(cx).server_list_focus.clone();
        focus.focus(window, cx);
    });
    servers.cx.run_until_parked();

    servers.cx.simulate_keystrokes("down");
    servers.cx.run_until_parked();
    assert_eq!(listed(servers.cx)[1], ("build".to_owned(), true));
    servers.cx.simulate_keystrokes("end");
    servers.cx.run_until_parked();
    assert_eq!(listed(servers.cx)[2], ("penguin".to_owned(), true));
    servers.cx.simulate_keystrokes("home");
    servers.cx.run_until_parked();
    assert_eq!(listed(servers.cx)[0], (local_name().to_owned(), true));
}

/// A paired server SpaceTerm isn't connected to shows what it last reported, here the version it
/// ran and that it needed an update, without reaching it.
#[gpui::test]
fn a_disconnected_paired_server_shows_its_last_known_facts(cx: &mut TestAppContext) {
    let (direct, routes) = disconnected(&["penguin"]);
    let key = ServerChoice::Direct(routes[0]).key();
    cx.update(|cx| {
        super::super::server_selection::remember(
            key,
            LastKnownFacts {
                observed_at: 10,
                status: ObservedServerStatus::NeedsUpdate,
                version: Some("1.4.0".to_owned()),
                ..LastKnownFacts::default()
            },
            cx,
        );
    });
    let servers = open_with(true, (direct, routes), cx);
    select("penguin", servers.cx);

    assert_eq!(status(&servers), "Needs update");
    let version = version_announcement(servers.cx);
    assert!(
        version
            .as_deref()
            .is_some_and(|subtitle| subtitle.starts_with("Version 1.4.0")),
        "got {version:?}"
    );
    assert!(servers.all_disconnected());
}

/// The open picker's rows for servers named `name`, as assistive technology hears them: each
/// row's description, in order. All Servers lists the same servers in a list of its own, so only
/// the picker's choices count.
fn picker_rows(name: &str, cx: &mut VisualTestContext) -> Vec<String> {
    let tree = A11yTree::read(cx);
    let choices = tree
        .with_role("ListBox")
        .into_iter()
        .find(|list| list["aria"]["label"] == super::PICKER_NAME)
        .expect("the picker's choices are open");
    tree.descendants(choices)
        .into_iter()
        .filter(|row| row["aria"]["role"] == "ListBoxOption" && row["aria"]["label"] == name)
        .filter_map(|row| row["aria"]["description"].as_str().map(str::to_owned))
        .collect()
}

/// Two paired servers can share a name and a status. On Servers and on Git, the picker says
/// where each one is, so choosing one never means guessing.
#[gpui::test]
fn the_picker_tells_apart_servers_that_share_a_name(cx: &mut TestAppContext) {
    let servers = open_servers(true, &["studio", "studio"], cx);

    for section in ["servers", "git"] {
        click(
            &format!("settings-navigation-settings-section-{section}"),
            servers.cx,
        );
        click(super::PICKER_SELECTOR, servers.cx);
        let rows = picker_rows("studio", servers.cx);
        assert_eq!(rows.len(), 2, "both servers are offered on {section}");
        assert_ne!(rows[0], rows[1], "the two rows read apart on {section}");
        assert!(
            rows[0].contains("192.168.1.10:9231") && rows[1].contains("192.168.1.11:9231"),
            "each row names its own address on {section}, got {rows:?}"
        );
        servers.cx.simulate_keystrokes("escape");
        servers.cx.run_until_parked();
    }

    // This computer's server lists first, then the two paired servers in order.
    click(super::PICKER_SELECTOR, servers.cx);
    click(&format!("{}-2", super::PICKER_SELECTOR), servers.cx);
    assert_eq!(
        selected(servers.cx),
        ServerChoice::Direct(servers.routes[1]).key(),
        "choosing the second row selects the server at its address"
    );
    let tree = A11yTree::read(servers.cx);
    let picker = tree.node(super::PICKER_NAME);
    assert_eq!(picker["aria"]["value"], "studio");
    assert!(
        picker["aria"]["description"]
            .as_str()
            .is_some_and(|description| description.contains("192.168.1.11:9231")),
        "the closed picker says which studio it shows, got {:?}",
        picker["aria"]["description"]
    );
}

/// The Server row's subtitle, such as "Version 1.4.0 · Update it from its own computer".
fn version_announcement(cx: &mut VisualTestContext) -> Option<String> {
    A11yTree::read(cx).in_order().iter().find_map(|node| {
        ["label", "value"].into_iter().find_map(|field| {
            node["aria"][field]
                .as_str()
                .filter(|text| text.starts_with("Version "))
                .map(str::to_owned)
        })
    })
}

struct RecordingMovement;

impl crate::platform::window_movement::WindowMovementFactory for RecordingMovement {
    fn create(
        &self,
    ) -> Rc<dyn crate::platform::window_movement::OperatingSystemWindowDragPlatform> {
        Rc::new(RecordingOperatingSystemWindowDragPlatform::default())
    }
}

/// A surface outside Settings, such as the updates menu, opens Settings on the server it names,
/// whether Settings is closed or already open on another server.
#[gpui::test]
fn opening_server_settings_selects_the_named_server(cx: &mut TestAppContext) {
    let (direct, routes) = disconnected(&["alpha", "bravo"]);
    let settings =
        crate::settings::Settings::load(MemoryStorage::with_document(&SettingsDocument::default()));
    let registry = direct.clone();
    cx.update(|cx| {
        appearance_runtime::install(
            settings,
            Rc::new(RecordingAppearancePlatform::default()),
            cx,
        )
        .expect("appearance runtime should install");
        crate::ui::init(cx).expect("UI initialization should succeed");
        crate::ui::direct_servers::install(registry, cx);
        super::super::configure_window_chrome(
            Rc::new(RecordingMovement),
            Default::default(),
            None,
            cx,
        );
    });
    let opened = |target: crate::ui::ServerTarget, cx: &mut TestAppContext| {
        cx.update(|cx| super::super::open_server_settings(target, cx));
        cx.run_until_parked();
        let windows: Vec<_> = cx
            .windows()
            .into_iter()
            .filter_map(|window| window.downcast::<SettingsWindow>())
            .collect();
        let [window] = windows[..] else {
            panic!("Settings should have one window, found {}", windows.len());
        };
        window
            .read_with(cx, |settings, _| {
                (settings.active_section, settings.selected_server())
            })
            .expect("Settings should stay open")
    };

    let new_window = opened(crate::ui::ServerTarget::Direct(routes[1]), cx);
    let open_window = opened(crate::ui::ServerTarget::Local, cx);

    assert_eq!(
        new_window,
        (
            SettingsSectionId::Servers,
            Some(ServerChoice::Direct(routes[1]))
        )
    );
    assert_eq!(
        open_window,
        (SettingsSectionId::Servers, Some(ServerChoice::Local))
    );
    assert!(
        routes
            .iter()
            .all(|route| direct.status(route) == Some(DirectRouteStatus::Disconnected)),
        "opening Settings on a server connects to nothing"
    );
}

/// Unpairs the selected paired server from its ⋯ menu, confirming the alert.
fn unpair_selected(cx: &mut VisualTestContext) {
    click(super::super::server_card::MENU_SELECTOR, cx);
    click("settings-server-unpair", cx);
    answer_alert(UNPAIR_SELF_CONFIRM_SELECTOR, cx);
}

#[gpui::test]
fn unpairing_the_selected_server_presents_this_computer(cx: &mut TestAppContext) {
    // Unpair starts the registry's worker, which tries the server on its own thread.
    cx.executor().allow_parking();
    let servers = open_servers(true, &["build", "penguin"], cx);
    select("penguin", servers.cx);

    unpair_selected(servers.cx);

    let remaining = vec![(local_name().to_owned(), true), ("build".to_owned(), false)];
    settle(servers.cx, |cx| listed(cx) == remaining);
    assert!(
        servers.direct.route(&servers.routes[1]).is_none(),
        "the saved route is gone"
    );
    settle(servers.cx, |cx| selected(cx) == ServerKey::Local);
}

#[gpui::test]
fn without_a_local_server_unpairing_presents_the_first_paired_server(cx: &mut TestAppContext) {
    cx.executor().allow_parking();
    let servers = open_servers(false, &["alpha", "bravo", "charlie"], cx);
    select("bravo", servers.cx);

    unpair_selected(servers.cx);

    let remaining = vec![("alpha".to_owned(), true), ("charlie".to_owned(), false)];
    settle(servers.cx, |cx| listed(cx) == remaining);
}

/// A choice retained from an earlier launch returns once its saved route is listed.
#[gpui::test]
fn a_retained_choice_returns_once_its_saved_route_loads(cx: &mut TestAppContext) {
    let (direct, routes) = disconnected(&["penguin"]);
    let key = ServerChoice::Direct(routes[0]).key();
    cx.update(|cx| super::super::server_selection::select(key, cx));

    let servers = open_with(true, (direct, routes), cx);

    assert_eq!(listed(servers.cx)[1], ("penguin".to_owned(), true));
}

/// A loaded registry that no longer includes the retained server is a real answer: the choice
/// and its facts go, and this computer's server is presented.
#[gpui::test]
fn a_retained_choice_whose_route_is_gone_gives_way_once_loading_finishes(cx: &mut TestAppContext) {
    let gone = ServerKey::Direct([99; 32]);
    cx.update(|cx| {
        super::super::server_selection::select(gone.clone(), cx);
        super::super::server_selection::remember(
            gone.clone(),
            LastKnownFacts {
                observed_at: 1,
                ..Default::default()
            },
            cx,
        );
    });

    let servers = open_servers(true, &[], cx);
    let direct = servers.direct.clone();
    settle(servers.cx, |_| direct.retained_inventory_loaded());
    servers.cx.run_until_parked();

    assert_eq!(selected(servers.cx), ServerKey::Local);
    assert!(
        servers
            .cx
            .update(|_, cx| super::super::server_selection::facts(&gone, cx))
            .is_none(),
        "a forgotten server's facts go with it"
    );
}

#[gpui::test]
fn rendering_the_servers_section_reaches_no_computer(cx: &mut TestAppContext) {
    let servers = open_servers(true, &["build", "penguin"], cx);
    select("penguin", servers.cx);
    select("build", servers.cx);
    for _ in 0..3 {
        servers.cx.update(|window, _| window.refresh());
        servers.cx.run_until_parked();
    }

    assert!(servers.all_disconnected());
}

#[gpui::test]
fn searching_for_a_server_by_name_reveals_and_selects_it(cx: &mut TestAppContext) {
    let mut servers = open_servers(true, &["build", "penguin"], cx);
    click("settings-navigation-settings-section-interface", servers.cx);

    search_for("PENG", &mut servers);

    servers.window.read_with(servers.cx, |settings, _| {
        assert_eq!(settings.active_section, SettingsSectionId::Servers);
        assert_eq!(
            settings.revealed,
            Some(super::super::SettingsRowId::ServerStatus)
        );
    });
    assert_eq!(listed(servers.cx)[2], ("penguin".to_owned(), true));
    assert!(servers.all_disconnected());
}

/// A word that only appears inside a server's name answers the Settings it names, not that
/// server. "servers" names All Servers, so the list shows which server the search presents.
#[gpui::test]
fn a_query_inside_a_server_name_keeps_the_local_server(cx: &mut TestAppContext) {
    let mut servers = open_servers(true, &["build.servers"], cx);
    search_for("servers", &mut servers);

    assert_eq!(listed(servers.cx)[0], (local_name().to_owned(), true));
    assert_eq!(listed(servers.cx)[1], ("build.servers".to_owned(), false));
}

/// A Setting the query names by its own words comes before a server that happens to share it.
#[gpui::test]
fn a_setting_named_by_the_query_ranks_before_a_server_with_that_name(cx: &mut TestAppContext) {
    let mut servers = open_servers(true, &["font-box"], cx);
    click("settings-navigation-settings-section-interface", servers.cx);
    search_for("font", &mut servers);

    servers.window.read_with(servers.cx, |settings, _| {
        assert_eq!(settings.active_section, SettingsSectionId::Font);
        assert_eq!(
            settings.revealed.map(|row| row.descriptor().section),
            Some(SettingsSectionId::Font)
        );
    });
}

/// Short queries are partial words a person is still typing, so they select no server.
#[gpui::test]
fn a_short_query_selects_no_server(cx: &mut TestAppContext) {
    let mut servers = open_servers(true, &["penguin"], cx);
    for query in ["p", "pe"] {
        search_for(query, &mut servers);
        assert_eq!(listed(servers.cx)[0], (local_name().to_owned(), true));
    }
}

/// A search shows the server it names only while it runs. Clearing it returns to the server the
/// person chose.
#[gpui::test]
fn clearing_a_search_returns_to_the_server_the_person_chose(cx: &mut TestAppContext) {
    let mut servers = open_servers(true, &["build", "penguin"], cx);
    select("penguin", servers.cx);
    search_for("build", &mut servers);
    assert_eq!(listed(servers.cx)[1], ("build".to_owned(), true));

    search_for("", &mut servers);
    assert_eq!(listed(servers.cx)[2], ("penguin".to_owned(), true));
}

#[test]
fn a_typed_host_is_offered_over_ssh_and_a_partial_name_is_not() {
    for host in ["penguin", "me@build.local", "10.0.0.4", "[::1]"] {
        assert!(super::looks_like_host(host), "{host}");
    }
    for typed in ["", "  ", "two words", "-oProxyCommand=x", "a/b"] {
        assert!(!super::looks_like_host(typed), "{typed:?}");
    }
}

/// New Worktrees go where the selected server's own setting names, on the Git page.
#[gpui::test]
fn searching_for_the_worktree_location_reveals_the_selected_servers_setting(
    cx: &mut TestAppContext,
) {
    let (mut servers, _alpha, _bravo) = present_both(cx);
    select("bravo", servers.cx);

    search_for("worktree", &mut servers);

    servers.window.read_with(servers.cx, |settings, _| {
        assert_eq!(settings.active_section, SettingsSectionId::Git);
        assert_eq!(
            settings.revealed,
            Some(super::super::SettingsRowId::ServerWorktreeLocation)
        );
    });
    settle(servers.cx, |cx| {
        location_field(cx) == "~/bravo/{repository}/{branch}"
    });
}

#[gpui::test]
fn a_server_settings_change_reaches_only_the_selected_server(cx: &mut TestAppContext) {
    use crate::ui::shared_workspaces::testing::ScriptedEdits;
    use spaceterm_server::hierarchy::Mutation;
    use spaceterm_server::worktree::Operation;

    let (servers, alpha, bravo) = present_both(cx);
    select("bravo", servers.cx);
    click("settings-navigation-settings-section-git", servers.cx);
    settle(servers.cx, |cx| {
        location_field(cx) == "~/bravo/{repository}/{branch}"
    });
    assert!(
        A11yTree::read(servers.cx)
            .with_role("Heading")
            .iter()
            .any(|heading| heading["aria"]["label"] == "Git on bravo"),
        "the section names the server it acts on"
    );

    click("settings-server-worktree-location", servers.cx);
    servers.cx.simulate_keystrokes("cmd-a");
    servers
        .cx
        .simulate_input("~/elsewhere/{repository}/{branch}");
    servers.cx.simulate_keystrokes("enter");
    let submitted = |edits: &ScriptedEdits| edits.submitted();
    settle(servers.cx, |_| !submitted(&bravo).is_empty());

    assert_eq!(
        submitted(&bravo),
        vec![Mutation::Worktree(Operation::SetLocation {
            version: 0,
            template: "~/elsewhere/{repository}/{branch}".into(),
        })]
    );
    assert!(
        submitted(&alpha).is_empty(),
        "the other server is untouched"
    );
}

#[gpui::test]
fn choosing_another_server_carries_no_draft_to_it(cx: &mut TestAppContext) {
    let (servers, alpha, bravo) = present_both(cx);
    select("bravo", servers.cx);
    select("alpha", servers.cx);
    click("settings-navigation-settings-section-git", servers.cx);
    settle(servers.cx, |cx| {
        location_field(cx) == "~/alpha/{repository}/{branch}"
    });
    click("settings-server-worktree-location", servers.cx);
    servers.cx.simulate_keystrokes("cmd-a");
    servers.cx.simulate_input("~/draft/{repository}/{branch}");

    let bravo_route = servers.routes[1];
    servers.window.update(servers.cx, |settings, cx| {
        settings.select_server(ServerChoice::Direct(bravo_route), cx);
    });
    settle(servers.cx, |cx| {
        location_field(cx) == "~/bravo/{repository}/{branch}"
    });
    servers.cx.simulate_keystrokes("enter");
    servers.cx.run_until_parked();

    assert!(alpha.submitted().is_empty());
    assert!(
        bravo.submitted().is_empty(),
        "a draft typed for alpha must never be written to bravo"
    );
}

fn search_for(query: &str, servers: &mut Servers<'_>) {
    let search = servers
        .window
        .read_with(servers.cx, |settings, _| settings.search.clone());
    let query = query.to_owned();
    servers.cx.update(|_, cx| {
        search.update(cx, |search, cx| search.set_value(query, cx));
    });
    servers.cx.run_until_parked();
}

/// Two connected paired servers, each presented with its own scripted writer, as the Workspace
/// Window presents them.
fn present_both<'a>(
    cx: &'a mut TestAppContext,
) -> (
    Servers<'a>,
    Rc<crate::ui::shared_workspaces::testing::ScriptedEdits>,
    Rc<crate::ui::shared_workspaces::testing::ScriptedEdits>,
) {
    use crate::ui::server_worktrees::testing::{ScriptedReads, present_direct};
    use crate::ui::shared_workspaces::testing::ScriptedEdits;

    let paired = connected_paired(&["alpha", "bravo"], cx);
    let servers = open_with(false, paired, cx);
    // Each writer wraps its route's connection, as the Workspace Window's does.
    let writers: Vec<_> = ["alpha", "bravo"]
        .into_iter()
        .zip(servers.routes.clone())
        .map(|(name, route)| {
            let edits = Rc::new(ScriptedEdits::new(None));
            edits.set_worktree_writes(true);
            let reads = ScriptedReads::new(&format!("~/{name}/{{repository}}/{{branch}}"));
            let writer = edits.clone();
            servers
                .cx
                .update(|_, cx| present_direct(route, writer, reads, cx));
            edits
        })
        .collect();
    let [alpha, bravo]: [_; 2] = writers.try_into().ok().unwrap();
    (servers, alpha, bravo)
}

fn location_field(cx: &mut VisualTestContext) -> String {
    A11yTree::read(cx).node("New Worktree Location")["aria"]["value"]
        .as_str()
        .unwrap_or_default()
        .to_owned()
}

/// Answers the open alert with the action `selector` names.
fn answer_alert(selector: &str, cx: &mut VisualTestContext) {
    click(&format!("modal-action-{selector}"), cx);
}

/// Reset All and Import replace only the Settings Document. Saved paired servers live in their
/// own record, and Server Settings belong to each server, so neither changes.
#[gpui::test]
fn reset_all_and_import_leave_server_connections_and_server_settings_alone(
    cx: &mut TestAppContext,
) {
    let (servers, alpha, bravo) = present_both(cx);
    select("bravo", servers.cx);
    click("settings-navigation-settings-section-git", servers.cx);
    settle(servers.cx, |cx| {
        location_field(cx) == "~/bravo/{repository}/{branch}"
    });
    let saved_before = servers.direct.routes();
    click("settings-navigation-settings-section-advanced", servers.cx);
    let detail = servers
        .window
        .read_with(servers.cx, |settings, _| settings.reset_all_detail());
    assert_eq!(
        detail,
        "This can\u{2019}t be undone. Server connections and system permissions are kept."
    );

    click("settings-reset-all", servers.cx);
    answer_alert("settings-reset-all-confirm", servers.cx);
    let mut exported = SettingsDocument::default();
    exported.appearance.window.density = crate::appearance::ChromeDensity::Comfortable;
    let exported = crate::settings::export_settings(&exported)
        .unwrap()
        .into_bytes();
    servers.cx.update(|native, cx| {
        servers.window.update(cx, |settings, cx| {
            settings.finish_settings_import(Ok(exported), native, cx);
        });
    });
    servers.cx.run_until_parked();
    answer_alert("settings-import-confirm", servers.cx);
    servers
        .cx
        .executor()
        .advance_clock(super::super::editor::COMMIT_DELAY * 2);
    servers.cx.run_until_parked();

    let retained = servers.storage.document().expect("the import is saved");
    assert_eq!(
        retained.appearance.window.density,
        crate::appearance::ChromeDensity::Comfortable
    );
    assert_eq!(
        retained.schema_version, 7,
        "the Settings Document stays schema 7"
    );
    assert_eq!(
        servers.direct.routes(),
        saved_before,
        "both servers stay saved and connected"
    );
    assert!(
        saved_before
            .iter()
            .all(|(_, status)| *status == DirectRouteStatus::Connected)
    );
    assert!(alpha.submitted().is_empty() && bravo.submitted().is_empty());
    click("settings-navigation-settings-section-git", servers.cx);
    settle(servers.cx, |cx| {
        location_field(cx) == "~/bravo/{repository}/{branch}"
    });
}

/// One server run with `live` running Terminal Sessions and a Pane waiting for its first shell.
fn running_sessions(boot: u8, live: u8) -> spaceterm_server::hierarchy::Inventory {
    use spaceterm_server::hierarchy::{CloseInventory, ConfirmedPane, Inventory};
    let mut sessions: Vec<_> = (0..live)
        .map(|pane| ConfirmedPane {
            id: [pane + 1; 32],
            session: Some([pane + 100; 32]),
        })
        .collect();
    sessions.push(ConfirmedPane {
        id: [99; 32],
        session: None,
    });
    Inventory {
        repositories: None,
        revision: 1,
        workspaces: Vec::new(),
        close: Some(CloseInventory {
            controls: Vec::new(),
            boot: [boot; 32],
            sessions,
        }),
    }
}

/// This computer's server's scripted writer, starting with `inventory`.
fn local_writer(
    inventory: spaceterm_server::hierarchy::Inventory,
) -> Rc<crate::ui::shared_workspaces::testing::ScriptedEdits> {
    Rc::new(crate::ui::shared_workspaces::testing::ScriptedEdits::new(
        Some(inventory),
    ))
}

/// Settings with this computer's server, selected and presented through `local`, and one saved
/// paired server, `bravo`. The writer is registered before Settings opens, so its first reading
/// is the scripted one and no real server is asked.
fn present_local_sessions(
    local: Rc<crate::ui::shared_workspaces::testing::ScriptedEdits>,
    cx: &mut TestAppContext,
) -> Servers<'_> {
    use crate::ui::server_worktrees::testing::{ScriptedReads, present_local};

    let reads = ScriptedReads::new("~/{repository}/{branch}");
    cx.update(|cx| present_local(local, reads, cx));
    let servers = open_servers(true, &["bravo"], cx);
    servers.cx.run_until_parked();
    servers
}

fn end_all_sessions_caption(servers: &Servers<'_>) -> String {
    servers.window.read_with(servers.cx, |settings, _| {
        settings.end_all_sessions_caption().0.to_string()
    })
}

/// The confirmation names this computer's server, and only its current run is asked to end its
/// Sessions, once. Cancelling asks nothing. A paired server offers no End All Sessions, because a
/// server is administered on its own computer.
#[gpui::test]
fn end_all_sessions_confirms_and_reaches_only_this_computers_server(cx: &mut TestAppContext) {
    use super::super::end_sessions::{END_CANCEL_SELECTOR, END_CONFIRM_SELECTOR, END_SELECTOR};
    use spaceterm_server::hierarchy::{Mutation, Outcome};

    let local = local_writer(running_sessions(2, 2));
    let servers = present_local_sessions(local.clone(), cx);
    let name = local_name();
    servers.cx.run_until_parked();
    assert!(rendered("settings-row-end-all-sessions", servers.cx));
    assert_eq!(
        end_all_sessions_caption(&servers),
        format!(
            "Ends all 2 running Terminal Sessions on {name} for every client and clears their \
             output. The server keeps running, and its Workspaces, Tabs, and Panes remain."
        )
    );

    click(END_SELECTOR, servers.cx);
    let tree = A11yTree::read(servers.cx);
    assert!(
        tree.find(&format!("End All Sessions on {name}?")).is_some(),
        "the confirmation names the server"
    );
    assert_eq!(
        tree.node(&format!("End All Sessions on {name}?"))["aria"]["description"],
        "This ends 2 Terminal Sessions for every client and clears their output.",
        "the confirmation counts the Sessions it ends"
    );
    answer_alert(END_CANCEL_SELECTOR, servers.cx);
    assert!(local.submitted().is_empty(), "Cancel asks nothing");

    click(END_SELECTOR, servers.cx);
    answer_alert(END_CONFIRM_SELECTOR, servers.cx);
    assert_eq!(
        local.submitted(),
        vec![Mutation::EndAllSessions {
            confirmation: running_sessions(2, 2).close.unwrap()
        }],
        "the request names the exact Sessions the confirmation showed"
    );

    local.answer(Outcome::Accepted { target: [2; 32] });
    let caption = |cx: &mut VisualTestContext| {
        servers
            .window
            .read_with(cx, |settings, _| settings.end_all_sessions_caption().0)
    };
    settle(servers.cx, |cx| {
        cx.executor().advance_clock(Duration::from_millis(100));
        caption(cx).starts_with(&format!("Every Terminal Session on {name} ended."))
    });
    assert_eq!(local.submitted().len(), 1, "nothing asks again");

    select("bravo", servers.cx);
    assert!(
        !rendered("settings-row-end-all-sessions", servers.cx),
        "a paired server offers no End All Sessions"
    );
}

/// A confirmation answers for the server run it showed. A restart since then submits nothing,
/// and a server's stale answer says nothing ended.
#[gpui::test]
fn end_all_sessions_refuses_a_confirmation_for_an_earlier_server_run(cx: &mut TestAppContext) {
    use super::super::end_sessions::{END_CONFIRM_SELECTOR, END_SELECTOR};
    use spaceterm_server::hierarchy::{Mutation, Outcome};

    let local = local_writer(running_sessions(2, 1));
    let servers = present_local_sessions(local.clone(), cx);
    let name = local_name();
    servers.cx.run_until_parked();

    click(END_SELECTOR, servers.cx);
    local.set_inventory(running_sessions(3, 1));
    answer_alert(END_CONFIRM_SELECTOR, servers.cx);
    assert!(
        local.submitted().is_empty(),
        "a restarted server's Sessions were never confirmed"
    );
    assert_eq!(
        end_all_sessions_caption(&servers),
        format!("{name} restarted, so nothing ended. Try again.")
    );

    // The server itself answers a confirmation it outran with the same explanation.
    servers
        .cx
        .executor()
        .advance_clock(Duration::from_millis(600));
    servers.cx.run_until_parked();
    click(END_SELECTOR, servers.cx);
    answer_alert(END_CONFIRM_SELECTOR, servers.cx);
    assert_eq!(
        local.submitted(),
        vec![Mutation::EndAllSessions {
            confirmation: running_sessions(3, 1).close.unwrap()
        }]
    );
    // The server's stale answer can't say whether it restarted, so the row claims only a change.
    local.answer(Outcome::StaleConfirmation);
    let caption = |cx: &mut VisualTestContext| {
        servers
            .window
            .read_with(cx, |settings, _| settings.end_all_sessions_caption().0)
    };
    settle(servers.cx, |cx| {
        cx.executor().advance_clock(Duration::from_millis(100));
        caption(cx)
            == format!("The Terminal Sessions on {name} changed, so nothing ended. Try again.")
    });
}

/// A confirmation names the exact Sessions it showed. Another client starting or ending one in
/// the same server run makes it stale: nothing is submitted, and the row doesn't claim the server
/// restarted.
#[gpui::test]
fn end_all_sessions_refuses_a_confirmation_whose_sessions_changed(cx: &mut TestAppContext) {
    use super::super::end_sessions::{END_CONFIRM_SELECTOR, END_SELECTOR};

    let local = local_writer(running_sessions(2, 1));
    let servers = present_local_sessions(local.clone(), cx);
    let name = local_name();
    servers.cx.run_until_parked();

    click(END_SELECTOR, servers.cx);
    // Another client ends the confirmed Session and starts a replacement in the same run.
    let mut replaced = running_sessions(2, 1);
    replaced.close.as_mut().unwrap().sessions[0].session = Some([200; 32]);
    local.set_inventory(replaced);
    answer_alert(END_CONFIRM_SELECTOR, servers.cx);

    assert!(
        local.submitted().is_empty(),
        "a replacement Session was never confirmed"
    );
    let caption = end_all_sessions_caption(&servers);
    assert!(
        caption == format!("The Terminal Sessions on {name} changed, so nothing ended. Try again."),
        "got {caption:?}"
    );
    assert!(!caption.contains("restarted"), "got {caption:?}");
}

/// A connected server that doesn't offer End All Sessions says so and asks nothing.
#[gpui::test]
fn end_all_sessions_is_unavailable_on_a_server_without_it(cx: &mut TestAppContext) {
    use super::super::end_sessions::END_SELECTOR;

    let local = local_writer(running_sessions(2, 2));
    local.set_end_all_sessions(false);
    let servers = present_local_sessions(local.clone(), cx);
    let name = local_name();
    servers.cx.run_until_parked();

    assert_eq!(
        end_all_sessions_caption(&servers),
        format!(
            "{name} doesn\u{2019}t support ending all its Terminal Sessions. Update the server to \
             use End All Sessions."
        )
    );
    click(END_SELECTOR, servers.cx);
    assert!(
        A11yTree::read(servers.cx)
            .find(&format!("End All Sessions on {name}?"))
            .is_none(),
        "nothing asks to confirm an action the server doesn't offer"
    );
    assert!(local.submitted().is_empty());
}

fn bounds(selector: &str, cx: &mut VisualTestContext) -> gpui::Bounds<gpui::Pixels> {
    let selector: &'static str = Box::leak(selector.to_owned().into_boxed_str());
    cx.debug_bounds(selector)
        .unwrap_or_else(|| panic!("{selector} was not rendered"))
}

#[gpui::test]
fn all_servers_rows_keep_the_edges_and_insets_of_the_rows_around_them(cx: &mut TestAppContext) {
    let servers = open_servers(true, &["penguin"], cx);
    let cx = servers.cx;
    let status = super::super::catalog::SettingsRowId::ServerStatus
        .descriptor()
        .selector;
    let label = bounds(&format!("{status}-label"), cx);
    let control = bounds("settings-server-status", cx);
    let row = bounds(status, cx);

    for index in 0..2 {
        let item = format!("{}-{index}", super::LIST_SELECTOR);
        assert_eq!(
            bounds(&format!("{item}-indicator"), cx).left(),
            label.left(),
            "a listed server's status should start on the labels' edge"
        );
        assert_eq!(
            bounds(&format!("{item}-check"), cx).right(),
            control.right(),
            "a listed server's checkmark should end on the controls' edge"
        );
        assert_eq!(
            bounds(&item, cx).size.height,
            row.size.height,
            "a listed server should be as tall as a described row"
        );
    }
    assert_eq!(
        bounds(&format!("{}-1", super::LIST_SELECTOR), cx).top(),
        bounds(&format!("{}-0", super::LIST_SELECTOR), cx).bottom(),
        "listed servers should meet at a separator, as a card's rows do"
    );
    assert_eq!(
        bounds(&format!("{}-icon", super::ADD_SERVER_SELECTOR), cx).left(),
        label.left(),
        "Add Server's plus should start on the labels' edge"
    );
    assert_eq!(
        bounds(&format!("{}-disclosure", super::PICKER_SELECTOR), cx).right(),
        control.right(),
        "the picker's chevron should end on the controls' edge"
    );
}

/// The corners of the hover fill painted over `selector` while the pointer rests on it.
fn hovered_corners(selector: &str, cx: &mut VisualTestContext) -> gpui::Corners<gpui::Pixels> {
    let item = bounds(selector, cx);
    cx.simulate_mouse_move(item.center(), None, Modifiers::none());
    cx.run_until_parked();
    cx.update(|window, cx| {
        let card = crate::ui::appearance::chrome(cx).host_colors(spaceterm_ui::ControlHost::Card);
        let hover =
            gpui::Background::from(crate::ui::appearance::gpui_color(card.row_hover_background));
        let scale = window.scale_factor();
        let quad = window
            .painted_quads()
            .into_iter()
            .find(|quad| quad.bounds == item.scale(scale) && quad.background == hover)
            .unwrap_or_else(|| panic!("{selector} should paint a hover fill"));
        let corners = quad.corner_radii;
        gpui::Corners {
            top_left: gpui::px(corners.top_left.0 / scale),
            top_right: gpui::px(corners.top_right.0 / scale),
            bottom_right: gpui::px(corners.bottom_right.0 / scale),
            bottom_left: gpui::px(corners.bottom_left.0 / scale),
        }
    })
}

#[gpui::test]
fn a_hovered_row_rounds_only_the_corners_it_shares_with_its_card(cx: &mut TestAppContext) {
    let servers = open_servers(true, &["penguin"], cx);
    let cx = servers.cx;
    let card = crate::ui::chrome_geometry::RadiusRole::Card.pixels()
        - gpui::px(crate::ui::chrome_geometry::HAIRLINE);
    let square = gpui::px(0.0);

    let first = hovered_corners(&format!("{}-0", super::LIST_SELECTOR), cx);
    assert_eq!(
        (first.top_left, first.top_right, first.bottom_left),
        (card, card, square),
        "the card's first row should round only its top corners"
    );
    let middle = hovered_corners(&format!("{}-1", super::LIST_SELECTOR), cx);
    assert_eq!(
        (middle.top_left, middle.bottom_right),
        (square, square),
        "a row between others should stay square"
    );
    let last = hovered_corners(super::ADD_SERVER_SELECTOR, cx);
    assert_eq!(
        (last.top_left, last.bottom_left, last.bottom_right),
        (square, card, card),
        "the card's last row should round only its bottom corners"
    );
}

#[gpui::test]
fn the_picker_keeps_clear_of_client_drawn_window_controls(cx: &mut TestAppContext) {
    let servers = open_servers(true, &["penguin"], cx);
    let cx = servers.cx;
    cx.simulate_decorations(gpui::Decorations::Client {
        tiling: gpui::Tiling::default(),
    });
    cx.simulate_button_layout(Some(gpui::WindowButtonLayout {
        left: [None; 3],
        right: [Some(gpui::WindowButton::Close), None, None],
    }));
    cx.run_until_parked();

    let picker = bounds(super::PICKER_SELECTOR, cx);
    let close = bounds("window-close", cx);
    assert!(
        picker.right() < close.left(),
        "the picker's hover surface should stop before Close, got {picker:?} beside {close:?}"
    );
}

/// A server running no Terminal Session says so under End All Sessions, which has nothing to end.
#[gpui::test]
fn a_server_running_no_terminal_session_says_so_under_end_all_sessions(cx: &mut TestAppContext) {
    let local = local_writer(running_sessions(2, 0));
    let servers = present_local_sessions(local, cx);
    let name = local_name();
    servers.cx.run_until_parked();

    let note = servers
        .window
        .read_with(servers.cx, |settings, _| settings.end_all_sessions_note());

    assert_eq!(
        note.as_deref(),
        Some(format!("No Terminal Sessions are running on {name}.").as_str())
    );
}
