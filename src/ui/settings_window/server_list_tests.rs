//! The Servers section's server list and the selected server's detail, driven through a scripted
//! local server and a registry of saved SSH connections whose backend counts every remote call.

use std::rc::Rc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use gpui::{Entity, Modifiers, TestAppContext, VisualTestContext};
use spaceterm_ui::a11y_testing::{A11yTree, perform};

use super::super::servers::testing::{ScriptedServer, connected};
use super::super::servers::{LocalServerControl, OfferedLocalServer, OfferedServer};
use super::ServerChoice;
use crate::appearance::Appearance;
use crate::domain::SshDestination;
use crate::platform::appearance::testing::RecordingAppearancePlatform;
use crate::platform::window_movement::RecordingOperatingSystemWindowDragPlatform;
use crate::settings::SettingsDocument;
use crate::settings::storage::testing::MemoryStorage;
use crate::ssh::cancellation::SshCancellationToken;
use crate::ssh::server_connection::{
    ConnectMode, ConnectProgress, ConnectedSshServer, RemoteConnectionStatus,
    RetainedSshConnection, ServerAvailability, SshConnectBackend, SshConnectFailure,
    SshConnectionIntentStore, SshIntentStoreError, SshServerConnections, SshServerContinuity,
    SshServerFacts, SshServerLease, SshServerObservation,
};
use crate::ui::appearance_runtime;
use crate::ui::settings_window::{SettingsSectionId, SettingsWindow};

const NAVIGATION_SELECTOR: &str = "settings-navigation-settings-section-servers";

/// The SSH computers the test reaches, counting every request that would leave this computer.
#[derive(Default)]
struct Computers {
    connects: AtomicUsize,
    observations: AtomicUsize,
    /// Destinations whose host never answers.
    unreachable: Mutex<Vec<String>>,
}

impl Computers {
    fn remote_calls(&self) -> usize {
        self.connects.load(Ordering::SeqCst) + self.observations.load(Ordering::SeqCst)
    }
}

struct Lease;
impl SshServerLease for Lease {
    fn is_alive(&self) -> bool {
        true
    }
}

fn facts() -> SshServerFacts {
    SshServerFacts {
        platform: ("Example OS".into(), "x86_64".into()),
        version: Some("1.4.0".into()),
        starts_automatically: true,
    }
}

impl SshConnectBackend for Computers {
    fn connect(
        &self,
        destination: &SshDestination,
        _: ConnectMode,
        _: Option<&SshServerContinuity>,
        _: &SshCancellationToken,
        _: &ConnectProgress<'_>,
    ) -> Result<ConnectedSshServer, SshConnectFailure> {
        self.connects.fetch_add(1, Ordering::SeqCst);
        if self
            .unreachable
            .lock()
            .unwrap()
            .iter()
            .any(|host| host == destination.as_str())
        {
            return Err(SshConnectFailure::Unavailable(
                crate::ssh::server_connection::UnavailableKind::HostUnreachable,
            ));
        }
        Ok(ConnectedSshServer {
            server: crate::local_server::LocalServer::for_identity("spaceterm-development")
                .expect("the development identity resolves"),
            continuity: SshServerContinuity {
                route: [1; 32],
                account: 501,
                build: "spaceterm-development".into(),
                server: [2; 32],
            },
            lease: Arc::new(Lease),
            protocol_major: 1,
            facts: facts(),
        })
    }

    fn observe(
        &self,
        _: &SshDestination,
        _: Option<&SshServerContinuity>,
        _: &SshCancellationToken,
        _: &ConnectProgress<'_>,
    ) -> Result<SshServerObservation, SshConnectFailure> {
        self.observations.fetch_add(1, Ordering::SeqCst);
        Ok(SshServerObservation {
            continuity: None,
            availability: ServerAvailability::Running,
            facts: facts(),
        })
    }
}

/// The private record of saved connections, kept apart from the Settings Document.
#[derive(Default)]
struct SavedConnections {
    records: Mutex<Vec<RetainedSshConnection>>,
    /// Whether removing a record fails, as unavailable storage does.
    removal_fails: std::sync::atomic::AtomicBool,
}

impl SavedConnections {
    fn destinations(&self) -> Vec<String> {
        let mut saved: Vec<_> = self
            .records
            .lock()
            .unwrap()
            .iter()
            .map(|record| record.destination.as_str().to_owned())
            .collect();
        saved.sort();
        saved
    }
}

impl SshConnectionIntentStore for SavedConnections {
    fn load(&self) -> Result<Vec<RetainedSshConnection>, SshIntentStoreError> {
        Ok(self.records.lock().unwrap().clone())
    }
    fn retain(&self, record: RetainedSshConnection) -> Result<(), SshIntentStoreError> {
        let mut records = self.records.lock().unwrap();
        records.retain(|other| other.destination != record.destination);
        records.push(record);
        Ok(())
    }
    fn remove(&self, destination: &SshDestination) -> Result<(), SshIntentStoreError> {
        if self.removal_fails.load(Ordering::SeqCst) {
            return Err(SshIntentStoreError::Unavailable);
        }
        self.records
            .lock()
            .unwrap()
            .retain(|record| record.destination != *destination);
        Ok(())
    }
}

fn destination(name: &str) -> SshDestination {
    SshDestination::new(name.to_owned()).expect("a valid SSH destination")
}

struct Servers<'a> {
    window: Entity<SettingsWindow>,
    computers: Arc<Computers>,
    saved: Arc<SavedConnections>,
    connections: SshServerConnections,
    storage: Arc<MemoryStorage>,
    cx: &'a mut VisualTestContext,
}

/// Opens Settings on Servers with the local server when `local` is set and each `saved` SSH
/// connection listed as it is after a relaunch: saved, inactive, and never reached.
fn open_servers<'a>(local: bool, saved: &[&str], cx: &'a mut TestAppContext) -> Servers<'a> {
    let computers = Arc::new(Computers::default());
    let records = Arc::new(SavedConnections::default());
    *records.records.lock().unwrap() = saved
        .iter()
        .map(|name| RetainedSshConnection {
            destination: destination(name),
            continuity: None,
            active: false,
        })
        .collect();
    let connections = SshServerConnections::new(computers.clone(), records.clone());
    connections.resume_retained();
    let deadline = Instant::now() + Duration::from_secs(5);
    while connections.connections().len() < saved.len() {
        assert!(Instant::now() < deadline, "the saved connections load");
        std::thread::sleep(Duration::from_millis(2));
    }
    let storage = MemoryStorage::with_document(&SettingsDocument::default());
    let settings = crate::settings::Settings::load(storage.clone());
    let platform = RecordingAppearancePlatform::default();
    platform.set_system_appearance(Some(Appearance::Dark));
    let registry = connections.clone();
    cx.update(|cx| {
        appearance_runtime::install(settings, Rc::new(platform), cx)
            .expect("appearance runtime should install");
        crate::ui::init(cx).expect("UI initialization should succeed");
        crate::ui::ssh_servers::install(registry, None, cx);
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
        computers,
        saved: records,
        connections,
        storage,
        cx,
    }
}

fn click(selector: &str, cx: &mut VisualTestContext) {
    let selector: &'static str = Box::leak(selector.to_owned().into_boxed_str());
    let position = cx
        .debug_bounds(selector)
        .unwrap_or_else(|| panic!("{selector} was not rendered"))
        .center();
    cx.simulate_mouse_move(position, None, Modifiers::none());
    cx.simulate_click(position, Modifiers::none());
    cx.run_until_parked();
}

fn rendered(selector: &str, cx: &mut VisualTestContext) -> bool {
    let selector: &'static str = Box::leak(selector.to_owned().into_boxed_str());
    cx.debug_bounds(selector).is_some()
}

/// Waits for the registry's workers, repainting as their results arrive.
fn settle(cx: &mut VisualTestContext, mut done: impl FnMut(&mut VisualTestContext) -> bool) {
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

/// The servers the list offers, in order, by the name assistive technology hears.
fn listed(cx: &mut VisualTestContext) -> Vec<(String, bool)> {
    let tree = A11yTree::read(cx);
    let list = tree.node(super::SERVER_LIST_NAME);
    assert_eq!(list["aria"]["role"], "ListBox");
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

#[gpui::test]
fn the_list_names_this_computer_first_then_each_saved_ssh_server(cx: &mut TestAppContext) {
    let servers = open_servers(true, &["penguin", "build.local"], cx);

    assert_eq!(
        listed(servers.cx),
        vec![
            (local_name().to_owned(), true),
            ("build.local".to_owned(), false),
            ("penguin".to_owned(), false),
        ],
        "the local server leads and is selected; saved SSH servers follow by destination"
    );
    let tree = A11yTree::read(servers.cx);
    let local = tree.node(local_name());
    assert!(
        local["aria"]["description"]
            .as_str()
            .is_some_and(|description| description.contains("This account")),
        "each item names its connection method"
    );
    assert!(
        tree.node("penguin")["aria"]["description"]
            .as_str()
            .is_some_and(
                |description| description.contains("SSH") && description.contains("Not Connected")
            ),
        "each item names its method and compact status"
    );
}

/// The heading texts the page presents, in order.
fn headings(cx: &mut VisualTestContext) -> Vec<String> {
    A11yTree::read(cx)
        .with_role("Heading")
        .into_iter()
        .filter_map(|node| node["aria"]["label"].as_str().map(str::to_owned))
        .collect()
}

fn select(name: &str, cx: &mut VisualTestContext) {
    let tree = A11yTree::read(cx);
    let item = tree.node(name);
    assert_eq!(item["aria"]["role"], "ListBoxOption");
    perform(cx, item, gpui::accesskit::Action::Click);
    cx.run_until_parked();
}

#[gpui::test]
fn selecting_an_ssh_server_presents_its_detail_under_headings_naming_it(cx: &mut TestAppContext) {
    let servers = open_servers(true, &["penguin"], cx);
    assert!(rendered("settings-row-local-server", servers.cx));
    assert!(headings(servers.cx).contains(&format!("Connection to {}", local_name())));

    click("settings-server-list-1", servers.cx);

    assert_eq!(
        listed(servers.cx),
        vec![
            (local_name().to_owned(), false),
            ("penguin".to_owned(), true)
        ]
    );
    let headings = headings(servers.cx);
    for heading in ["penguin", "Connection to penguin"] {
        assert!(
            headings.iter().any(|shown| shown == heading),
            "{heading:?} should name the selected machine, got {headings:?}"
        );
    }
    assert!(
        !headings.iter().any(|shown| shown.contains(local_name())),
        "no heading may name a machine the rows don't act on, got {headings:?}"
    );
    for local in [
        "settings-row-local-server",
        "settings-row-server-service",
        "settings-row-server-update",
    ] {
        assert!(
            !rendered(local, servers.cx),
            "{local} belongs to the local server"
        );
    }
    for ssh in [
        "settings-row-ssh-server-status",
        "settings-row-ssh-server-version",
        "settings-row-server-connection",
        "settings-row-direct-network-access",
        "settings-row-direct-network-pairing",
        "settings-row-remove-server",
    ] {
        assert!(
            rendered(ssh, servers.cx),
            "{ssh} presents the selected SSH server"
        );
    }
    let tree = A11yTree::read(servers.cx);
    assert!(
        tree.find_text("Unknown").is_some(),
        "facts read as unknown until SpaceTerm observes the server"
    );
    assert!(
        tree.find_text(
            "Connect to \u{201C}penguin\u{201D} to see or change its direct network access."
        )
        .is_some(),
        "direct network access waits for a connection to the selected server"
    );
}

/// A build with no remote operations owner offers no remote maintenance, so the selected SSH
/// server keeps the rows that only report it.
#[gpui::test]
fn without_an_operations_owner_an_ssh_server_offers_no_maintenance(cx: &mut TestAppContext) {
    let servers = open_servers(true, &["penguin"], cx);

    click("settings-server-list-1", servers.cx);

    for absent in [
        "settings-row-ssh-server-service",
        "settings-row-ssh-server-update",
        "settings-row-removed-server-operations",
    ] {
        assert!(
            !rendered(absent, servers.cx),
            "{absent} needs an operations owner this build has none of"
        );
    }
    assert!(rendered("settings-row-ssh-server-status", servers.cx));
    assert!(rendered("settings-row-ssh-server-version", servers.cx));
}

#[gpui::test]
fn the_local_server_offers_no_removal_or_pairing_ceremony(cx: &mut TestAppContext) {
    let servers = open_servers(true, &["penguin"], cx);

    assert!(rendered("settings-row-server-connection", servers.cx));
    assert!(!rendered("settings-row-remove-server", servers.cx));
    assert!(!rendered("settings-ssh-server-remove", servers.cx));
    let tree = A11yTree::read(servers.cx);
    assert!(
        tree.with_role("Button")
            .iter()
            .all(|button| !button["aria"]["label"]
                .as_str()
                .is_some_and(|label| label.contains("Remove") || label.contains("Unpair"))),
        "the account-local server has nothing to remove or unpair"
    );
}

#[gpui::test]
fn the_arrow_keys_move_the_selection_while_the_list_has_focus(cx: &mut TestAppContext) {
    let servers = open_servers(true, &["build.local", "penguin"], cx);
    servers.cx.update(|window, cx| {
        let focus = servers.window.read(cx).server_list_focus.clone();
        focus.focus(window, cx);
    });
    servers.cx.run_until_parked();

    servers.cx.simulate_keystrokes("down");
    servers.cx.run_until_parked();
    assert_eq!(listed(servers.cx)[1], ("build.local".to_owned(), true));
    servers.cx.simulate_keystrokes("end");
    servers.cx.run_until_parked();
    assert_eq!(listed(servers.cx)[2], ("penguin".to_owned(), true));
    servers.cx.simulate_keystrokes("home");
    servers.cx.run_until_parked();
    assert_eq!(listed(servers.cx)[0], (local_name().to_owned(), true));
}

#[gpui::test]
fn check_status_observes_an_inactive_server_once_and_leaves_a_presented_one_alone(
    cx: &mut TestAppContext,
) {
    let servers = open_servers(true, &["penguin"], cx);
    select("penguin", servers.cx);

    click("settings-ssh-server-check", servers.cx);
    let computers = servers.computers.clone();
    settle(servers.cx, |_| {
        computers.observations.load(Ordering::SeqCst) == 1
    });
    let connections = servers.connections.clone();
    settle(servers.cx, |_| {
        matches!(
            connections.status(&destination("penguin")),
            RemoteConnectionStatus::Checked { .. }
        )
    });
    assert!(
        A11yTree::read(servers.cx).find_text("1.4.0").is_some(),
        "the observed version replaces Unknown"
    );
    let caption = servers.window.read_with(servers.cx, |settings, _| {
        settings
            .ssh_server_caption(super::super::SettingsRowId::SshServerStatus)
            .map(|(caption, _)| caption.to_string())
            .unwrap_or_default()
    });
    assert!(
        caption.contains("was running") && caption.contains("Last checked just now"),
        "a check says what it found and when, got {caption:?}"
    );

    click("settings-ssh-server-connect", servers.cx);
    settle(servers.cx, |_| {
        connections.server(&destination("penguin")).is_some()
    });
    servers.cx.run_until_parked();

    assert!(
        !rendered("settings-ssh-server-check", servers.cx),
        "a presented server already reports its state, so Check Status is not offered"
    );
    assert!(rendered("settings-ssh-server-disconnect", servers.cx));
    assert_eq!(computers.observations.load(Ordering::SeqCst), 1);
}

#[gpui::test]
fn after_disconnect_the_detail_presents_the_facts_last_observed(cx: &mut TestAppContext) {
    let servers = open_servers(true, &["penguin"], cx);
    select("penguin", servers.cx);
    click("settings-ssh-server-connect", servers.cx);
    let connections = servers.connections.clone();
    settle(servers.cx, |_| {
        connections.server(&destination("penguin")).is_some()
    });
    assert!(
        A11yTree::read(servers.cx)
            .find_text(LAST_OBSERVED)
            .is_none(),
        "a connected server's facts are current"
    );

    click("settings-ssh-server-disconnect", servers.cx);

    let tree = A11yTree::read(servers.cx);
    assert!(
        tree.find_text("1.4.0").is_some(),
        "the version stays presented"
    );
    assert!(
        tree.find_text(LAST_OBSERVED).is_some(),
        "the detail says the facts were observed earlier"
    );
    assert_eq!(servers.computers.observations.load(Ordering::SeqCst), 0);
}

const LAST_OBSERVED: &str = "SpaceTerm last observed these details when it connected or checked \
                             the status. They may have changed since.";

struct RecordingMovement;

impl crate::platform::window_movement::WindowMovementFactory for RecordingMovement {
    fn create(
        &self,
    ) -> Rc<dyn crate::platform::window_movement::OperatingSystemWindowDragPlatform> {
        Rc::new(RecordingOperatingSystemWindowDragPlatform::default())
    }
}

/// A surface outside Settings, such as a machine heading, opens Settings on the server it names,
/// whether Settings is closed or already open on another server.
#[gpui::test]
fn opening_server_settings_selects_the_named_server(cx: &mut TestAppContext) {
    let records = Arc::new(SavedConnections::default());
    *records.records.lock().unwrap() = ["alpha", "bravo"]
        .into_iter()
        .map(|name| RetainedSshConnection {
            destination: destination(name),
            continuity: None,
            active: false,
        })
        .collect();
    let computers = Arc::new(Computers::default());
    let connections = SshServerConnections::new(computers.clone(), records);
    connections.resume_retained();
    let deadline = Instant::now() + Duration::from_secs(5);
    while connections.connections().len() < 2 {
        assert!(Instant::now() < deadline, "the saved connections load");
        std::thread::sleep(Duration::from_millis(2));
    }
    let settings =
        crate::settings::Settings::load(MemoryStorage::with_document(&SettingsDocument::default()));
    cx.update(|cx| {
        appearance_runtime::install(
            settings,
            Rc::new(RecordingAppearancePlatform::default()),
            cx,
        )
        .expect("appearance runtime should install");
        crate::ui::init(cx).expect("UI initialization should succeed");
        crate::ui::ssh_servers::install(connections, None, cx);
        super::super::configure_window_chrome(
            Rc::new(RecordingMovement),
            Default::default(),
            None,
            cx,
        );
    });
    let opened = |destination: Option<SshDestination>, cx: &mut TestAppContext| {
        cx.update(|cx| {
            super::super::open_server_settings(
                destination.map_or(
                    crate::ui::MachineTarget::Local,
                    crate::ui::MachineTarget::Ssh,
                ),
                cx,
            )
        });
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

    let new_window = opened(Some(destination("bravo")), cx);
    let open_window = opened(None, cx);

    assert_eq!(
        new_window,
        (
            SettingsSectionId::Servers,
            Some(ServerChoice::Ssh(destination("bravo")))
        )
    );
    assert_eq!(
        open_window,
        (SettingsSectionId::Servers, Some(ServerChoice::Local))
    );
    assert_eq!(computers.remote_calls(), 0);
}

#[gpui::test]
fn remove_forgets_the_connection_at_once_and_selects_the_local_server(cx: &mut TestAppContext) {
    let servers = open_servers(true, &["build.local", "penguin"], cx);
    select("penguin", servers.cx);

    click("settings-ssh-server-remove", servers.cx);

    assert_eq!(
        listed(servers.cx),
        vec![
            (local_name().to_owned(), true),
            ("build.local".to_owned(), false)
        ],
        "removal takes effect without a confirmation, and the local server is selected"
    );
    let saved = servers.saved.clone();
    settle(servers.cx, |_| saved.destinations() == ["build.local"]);
    assert_eq!(
        servers.computers.remote_calls(),
        0,
        "removing reaches no computer, so SSH access and the server stay as they were"
    );
}

#[gpui::test]
fn without_a_local_server_remove_selects_the_next_saved_server(cx: &mut TestAppContext) {
    let servers = open_servers(false, &["alpha", "bravo", "charlie"], cx);
    select("bravo", servers.cx);

    click("settings-ssh-server-remove", servers.cx);

    assert_eq!(
        listed(servers.cx),
        vec![("alpha".to_owned(), false), ("charlie".to_owned(), true)]
    );
}

#[gpui::test]
fn rendering_the_servers_section_reaches_no_computer(cx: &mut TestAppContext) {
    let servers = open_servers(true, &["build.local", "penguin"], cx);
    select("penguin", servers.cx);
    select("build.local", servers.cx);
    for _ in 0..3 {
        servers.cx.update(|window, _| window.refresh());
        servers.cx.run_until_parked();
    }

    assert_eq!(servers.computers.remote_calls(), 0);
}

#[gpui::test]
fn searching_for_a_server_by_name_reveals_and_selects_it(cx: &mut TestAppContext) {
    let servers = open_servers(true, &["build.local", "penguin"], cx);
    click("settings-navigation-settings-section-interface", servers.cx);

    let search = servers
        .window
        .read_with(servers.cx, |settings, _| settings.search.clone());
    servers.cx.update(|_, cx| {
        search.update(cx, |search, cx| search.set_value("PENG".to_owned(), cx));
    });
    servers.cx.run_until_parked();

    servers.window.read_with(servers.cx, |settings, _| {
        assert_eq!(settings.active_section, SettingsSectionId::Servers);
        assert_eq!(
            settings.revealed,
            Some(super::super::SettingsRowId::SshServerStatus)
        );
    });
    assert_eq!(listed(servers.cx)[2], ("penguin".to_owned(), true));
    assert!(
        headings(servers.cx)
            .iter()
            .any(|heading| heading == "Connection to penguin")
    );
    assert_eq!(servers.computers.remote_calls(), 0);
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

/// A word that only appears inside a host's name answers the Settings it names, not that host.
#[gpui::test]
fn a_query_inside_a_host_name_keeps_the_local_server_and_its_rows(cx: &mut TestAppContext) {
    let mut servers = open_servers(true, &["build.local"], cx);
    search_for("local", &mut servers);

    assert_eq!(listed(servers.cx)[0], (local_name().to_owned(), true));
    assert!(rendered("settings-row-local-server", servers.cx));
    servers.window.read_with(servers.cx, |settings, _| {
        assert_eq!(
            settings.revealed,
            Some(super::super::SettingsRowId::LocalServer)
        );
    });
}

/// A Setting the query names by its own words comes before a host that happens to share it.
#[gpui::test]
fn a_setting_named_by_the_query_ranks_before_a_host_with_that_name(cx: &mut TestAppContext) {
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

/// A host whose requested Connect failed is still tried quietly when SpaceTerm reopens, so its
/// detail offers ending that without removing the saved connection.
#[gpui::test]
fn a_failed_connection_offers_to_stop_reconnecting_and_stays_saved(cx: &mut TestAppContext) {
    let servers = open_servers(true, &[], cx);
    servers
        .computers
        .unreachable
        .lock()
        .unwrap()
        .push("nohost".to_owned());
    servers.connections.connect(destination("nohost"));
    let connections = servers.connections.clone();
    settle(servers.cx, |_| {
        matches!(
            connections.status(&destination("nohost")),
            RemoteConnectionStatus::Unavailable { .. }
        )
    });
    select("nohost", servers.cx);
    let records = servers.saved.clone();
    let saved = move || {
        records
            .records
            .lock()
            .unwrap()
            .iter()
            .find(|record| record.destination == destination("nohost"))
            .map(|record| record.active)
    };
    assert_eq!(saved(), Some(true));

    click("settings-ssh-server-stop-reconnecting", servers.cx);
    settle(servers.cx, |_| saved() == Some(false));
    assert!(!rendered(
        "settings-ssh-server-stop-reconnecting",
        servers.cx
    ));
    assert_eq!(listed(servers.cx)[1].0, "nohost");
}

/// A removal SpaceTerm couldn't save would bring the connection back at relaunch, so Settings
/// names it and offers to remove it again.
#[gpui::test]
fn a_removal_that_could_not_be_saved_names_the_host_and_can_be_retried(cx: &mut TestAppContext) {
    let servers = open_servers(true, &["penguin"], cx);
    servers.saved.removal_fails.store(true, Ordering::SeqCst);
    select("penguin", servers.cx);
    click(super::super::ssh_servers::REMOVE_SELECTOR, servers.cx);
    let connections = servers.connections.clone();
    settle(servers.cx, |_| !connections.unsaved_removals().is_empty());

    let notice = A11yTree::read(servers.cx).node(super::super::ssh_servers::UNSAVED_REMOVAL_NAME)
        ["aria"]["description"]
        .as_str()
        .unwrap_or_default()
        .to_owned();
    assert!(notice.contains("\u{201C}penguin\u{201D}"), "got {notice:?}");
    assert!(notice.contains("reopens"), "got {notice:?}");

    servers.saved.removal_fails.store(false, Ordering::SeqCst);
    click("settings-ssh-server-remove-again", servers.cx);
    settle(servers.cx, |_| connections.unsaved_removals().is_empty());
    assert!(servers.saved.destinations().is_empty());
    let connections = servers.connections.clone();
    settle(servers.cx, |_| connections.persistence_error().is_none());
    assert!(!rendered("settings-ssh-server-remove-again", servers.cx));
    assert!(
        !rendered(
            super::super::ssh_servers::UNSAVED_REMOVAL_SELECTOR,
            servers.cx
        ) && !rendered(super::super::ssh_servers::PERSISTENCE_SELECTOR, servers.cx),
        "once the removal saves, no notice remains"
    );
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
    let mut servers = open_servers(true, &["build.local", "penguin"], cx);
    select("penguin", servers.cx);
    search_for("build", &mut servers);
    assert_eq!(listed(servers.cx)[1], ("build.local".to_owned(), true));

    search_for("", &mut servers);
    assert_eq!(listed(servers.cx)[2], ("penguin".to_owned(), true));
}

#[gpui::test]
fn a_query_for_a_local_only_row_selects_the_local_server(cx: &mut TestAppContext) {
    let servers = open_servers(true, &["penguin"], cx);
    select("penguin", servers.cx);

    let search = servers
        .window
        .read_with(servers.cx, |settings, _| settings.search.clone());
    servers.cx.update(|_, cx| {
        search.update(cx, |search, cx| {
            search.set_value("start server".to_owned(), cx)
        });
    });
    servers.cx.run_until_parked();

    assert_eq!(listed(servers.cx)[0], (local_name().to_owned(), true));
    assert!(rendered("settings-row-local-server", servers.cx));
}

/// Two saved SSH servers, each connected and presented with its own scripted writer, as the
/// Workspace Window presents them.
fn present_both<'a>(
    cx: &'a mut TestAppContext,
) -> (
    Servers<'a>,
    Rc<crate::ui::shared_workspaces::testing::ScriptedEdits>,
    Rc<crate::ui::shared_workspaces::testing::ScriptedEdits>,
) {
    use crate::ui::server_worktrees::testing::{ScriptedReads, present_ssh};
    use crate::ui::shared_workspaces::testing::ScriptedEdits;

    let servers = open_servers(false, &["alpha", "bravo"], cx);
    for name in ["alpha", "bravo"] {
        servers.connections.connect(destination(name));
    }
    let connections = servers.connections.clone();
    settle(servers.cx, |_| {
        connections.server(&destination("alpha")).is_some()
            && connections.server(&destination("bravo")).is_some()
    });
    // Each writer wraps its connection's server, as the Workspace Window's does.
    let writers: Vec<_> = ["alpha", "bravo"]
        .into_iter()
        .map(|name| {
            let edits = Rc::new(ScriptedEdits::new(None));
            edits.set_worktree_writes(true);
            let reads = ScriptedReads::new(&format!("~/{name}/{{repository}}/{{branch}}"));
            let edits_for_window = edits.clone();
            servers.cx.update(|_, cx| {
                present_ssh(&destination(name), edits_for_window, reads, cx);
            });
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

#[gpui::test]
fn server_settings_are_unavailable_until_the_selected_server_is_connected(cx: &mut TestAppContext) {
    let servers = open_servers(false, &["penguin"], cx);

    assert!(rendered(
        "settings-row-server-worktree-location",
        servers.cx
    ));
    let caption = servers.window.read_with(servers.cx, |settings, cx| {
        settings.server_location_caption(cx).0
    });
    assert_eq!(caption, "Connect to the server on penguin to change it.");
    assert!(
        headings(servers.cx)
            .iter()
            .any(|heading| heading == "Server Settings on penguin")
    );
}

#[gpui::test]
fn a_server_settings_change_reaches_only_the_selected_server(cx: &mut TestAppContext) {
    use crate::ui::shared_workspaces::testing::ScriptedEdits;
    use spaceterm_server::hierarchy::Mutation;
    use spaceterm_server::worktree::Operation;

    let (servers, alpha, bravo) = present_both(cx);
    select("bravo", servers.cx);
    settle(servers.cx, |cx| {
        location_field(cx) == "~/bravo/{repository}/{branch}"
    });
    let caption = servers.window.read_with(servers.cx, |settings, cx| {
        settings.server_location_caption(cx).0
    });
    assert!(
        caption.contains("on bravo"),
        "the caption names the machine, got {caption:?}"
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
    // The list selects alpha first; choosing bravo and back reads alpha through its writer.
    select("bravo", servers.cx);
    select("alpha", servers.cx);
    settle(servers.cx, |cx| {
        location_field(cx) == "~/alpha/{repository}/{branch}"
    });
    click("settings-server-worktree-location", servers.cx);
    servers.cx.simulate_keystrokes("cmd-a");
    servers.cx.simulate_input("~/draft/{repository}/{branch}");

    select("bravo", servers.cx);
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

/// Answers the open alert with the action `selector` names.
fn answer_alert(selector: &str, cx: &mut VisualTestContext) {
    click(&format!("modal-action-{selector}"), cx);
}

/// Reset All and Import replace only the Settings Document. Saved connections and their intent
/// live in their own record, and Server Settings belong to each server, so neither changes.
#[gpui::test]
fn reset_all_and_import_leave_server_connections_and_server_settings_alone(
    cx: &mut TestAppContext,
) {
    let (servers, alpha, bravo) = present_both(cx);
    select("bravo", servers.cx);
    settle(servers.cx, |cx| {
        location_field(cx) == "~/bravo/{repository}/{branch}"
    });
    let saved_before = servers.saved.destinations();
    click("settings-navigation-settings-section-advanced", servers.cx);
    let detail = servers
        .window
        .read_with(servers.cx, |settings, _| settings.reset_all_detail());
    assert!(
        detail.contains("Server connections and Server Settings are not affected."),
        "the confirmation says what Reset All leaves alone, got {detail:?}"
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
    assert_eq!(servers.saved.destinations(), saved_before);
    let listed: Vec<_> = servers
        .connections
        .connections()
        .into_iter()
        .map(|(destination, status)| {
            (
                destination.as_str().to_owned(),
                matches!(status, RemoteConnectionStatus::Connected { .. }),
            )
        })
        .collect();
    assert_eq!(
        listed,
        vec![("alpha".to_owned(), true), ("bravo".to_owned(), true)],
        "both servers stay saved and connected"
    );
    assert!(alpha.submitted().is_empty() && bravo.submitted().is_empty());
    click(NAVIGATION_SELECTOR, servers.cx);
    settle(servers.cx, |cx| {
        location_field(cx) == "~/bravo/{repository}/{branch}"
    });
}

#[gpui::test]
fn connecting_a_typed_host_lists_and_selects_it(cx: &mut TestAppContext) {
    let servers = open_servers(true, &["penguin"], cx);

    click("settings-ssh-server-destination", servers.cx);
    servers.cx.simulate_input("me@build.local");
    servers.cx.simulate_keystrokes("enter");
    let computers = servers.computers.clone();
    settle(servers.cx, |_| {
        computers.connects.load(Ordering::SeqCst) == 1
    });

    assert_eq!(
        listed(servers.cx),
        vec![
            (local_name().to_owned(), false),
            ("me@build.local".to_owned(), true),
            ("penguin".to_owned(), false),
        ]
    );
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
            boot: [boot; 32],
            sessions,
        }),
    }
}

fn end_all_sessions_caption(servers: &Servers<'_>) -> String {
    servers.window.read_with(servers.cx, |settings, _| {
        settings.end_all_sessions_caption().0.to_string()
    })
}

/// The confirmation names the selected server, and only that server's current run is asked to
/// end its Sessions, once. Cancelling asks nothing.
#[gpui::test]
fn end_all_sessions_confirms_and_reaches_only_the_selected_server(cx: &mut TestAppContext) {
    use super::super::end_sessions::{END_CANCEL_SELECTOR, END_CONFIRM_SELECTOR, END_SELECTOR};
    use spaceterm_server::hierarchy::{Mutation, Outcome};

    let (servers, alpha, bravo) = present_both(cx);
    alpha.set_inventory(running_sessions(1, 3));
    bravo.set_inventory(running_sessions(2, 2));
    select("bravo", servers.cx);
    servers.cx.run_until_parked();
    assert!(rendered("settings-row-end-all-sessions", servers.cx));
    assert_eq!(
        end_all_sessions_caption(&servers),
        "Ends all 2 running Terminal Sessions on bravo for every client and clears their output. \
         The server keeps running, and its Workspaces, Tabs, and Panes remain."
    );

    click(END_SELECTOR, servers.cx);
    let tree = A11yTree::read(servers.cx);
    assert!(
        tree.find("End All Sessions on bravo?").is_some(),
        "the confirmation names the selected server"
    );
    answer_alert(END_CANCEL_SELECTOR, servers.cx);
    assert!(bravo.submitted().is_empty(), "Cancel asks nothing");

    click(END_SELECTOR, servers.cx);
    answer_alert(END_CONFIRM_SELECTOR, servers.cx);
    assert_eq!(
        bravo.submitted(),
        vec![Mutation::EndAllSessions {
            confirmation: running_sessions(2, 2).close.unwrap()
        }],
        "the request names the exact Sessions bravo's confirmation showed"
    );
    assert!(
        alpha.submitted().is_empty(),
        "the other server is untouched"
    );

    bravo.answer(Outcome::Accepted { target: [2; 32] });
    let caption = |cx: &mut VisualTestContext| {
        servers
            .window
            .read_with(cx, |settings, _| settings.end_all_sessions_caption().0)
    };
    settle(servers.cx, |cx| {
        cx.executor().advance_clock(Duration::from_millis(100));
        caption(cx).starts_with("Every Terminal Session on bravo ended.")
    });
    assert_eq!(bravo.submitted().len(), 1, "nothing asks again");
}

/// A confirmation answers for the server run it showed. A restart since then submits nothing,
/// and a server's stale answer says nothing ended.
#[gpui::test]
fn end_all_sessions_refuses_a_confirmation_for_an_earlier_server_run(cx: &mut TestAppContext) {
    use super::super::end_sessions::{END_CONFIRM_SELECTOR, END_SELECTOR};
    use spaceterm_server::hierarchy::{Mutation, Outcome};

    let (servers, _alpha, bravo) = present_both(cx);
    bravo.set_inventory(running_sessions(2, 1));
    select("bravo", servers.cx);
    servers.cx.run_until_parked();

    click(END_SELECTOR, servers.cx);
    bravo.set_inventory(running_sessions(3, 1));
    answer_alert(END_CONFIRM_SELECTOR, servers.cx);
    assert!(
        bravo.submitted().is_empty(),
        "a restarted server's Sessions were never confirmed"
    );
    assert!(end_all_sessions_caption(&servers).starts_with("bravo restarted after you confirmed"));

    // The server itself answers a confirmation it outran with the same explanation.
    servers
        .cx
        .executor()
        .advance_clock(Duration::from_millis(600));
    servers.cx.run_until_parked();
    click(END_SELECTOR, servers.cx);
    answer_alert(END_CONFIRM_SELECTOR, servers.cx);
    assert_eq!(
        bravo.submitted(),
        vec![Mutation::EndAllSessions {
            confirmation: running_sessions(3, 1).close.unwrap()
        }]
    );
    // The server's stale answer can't say whether it restarted, so the row claims only a change.
    bravo.answer(Outcome::StaleConfirmation);
    let caption = |cx: &mut VisualTestContext| {
        servers
            .window
            .read_with(cx, |settings, _| settings.end_all_sessions_caption().0)
    };
    settle(servers.cx, |cx| {
        cx.executor().advance_clock(Duration::from_millis(100));
        caption(cx).starts_with("The Terminal Sessions on bravo changed after you confirmed")
    });
}

/// A confirmation names the exact Sessions it showed. Another client starting or ending one in
/// the same server run makes it stale: nothing is submitted, and the row doesn't claim the server
/// restarted.
#[gpui::test]
fn end_all_sessions_refuses_a_confirmation_whose_sessions_changed(cx: &mut TestAppContext) {
    use super::super::end_sessions::{END_CONFIRM_SELECTOR, END_SELECTOR};

    let (servers, _alpha, bravo) = present_both(cx);
    bravo.set_inventory(running_sessions(2, 1));
    select("bravo", servers.cx);
    servers.cx.run_until_parked();

    click(END_SELECTOR, servers.cx);
    // Another client ends the confirmed Session and starts a replacement in the same run.
    let mut replaced = running_sessions(2, 1);
    replaced.close.as_mut().unwrap().sessions[0].session = Some([200; 32]);
    bravo.set_inventory(replaced);
    answer_alert(END_CONFIRM_SELECTOR, servers.cx);

    assert!(
        bravo.submitted().is_empty(),
        "a replacement Session was never confirmed"
    );
    let caption = end_all_sessions_caption(&servers);
    assert!(
        caption.starts_with("The Terminal Sessions on bravo changed after you confirmed"),
        "got {caption:?}"
    );
    assert!(!caption.contains("restarted"), "got {caption:?}");
}

/// A connected server that doesn't offer End All Sessions says so and asks nothing.
#[gpui::test]
fn end_all_sessions_is_unavailable_on_a_server_without_it(cx: &mut TestAppContext) {
    use super::super::end_sessions::END_SELECTOR;

    let (servers, _alpha, bravo) = present_both(cx);
    bravo.set_inventory(running_sessions(2, 2));
    bravo.set_end_all_sessions(false);
    select("bravo", servers.cx);
    servers.cx.run_until_parked();

    assert_eq!(
        end_all_sessions_caption(&servers),
        "bravo doesn\u{2019}t support ending all its Terminal Sessions. Update the server to use \
         End All Sessions."
    );
    click(END_SELECTOR, servers.cx);
    assert!(
        A11yTree::read(servers.cx)
            .find("End All Sessions on bravo?")
            .is_none(),
        "nothing asks to confirm an action the server doesn't offer"
    );
    assert!(bravo.submitted().is_empty());
}
