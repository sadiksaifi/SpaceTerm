//! The server picker, All Servers and the selected server's card, driven through a scripted local
//! server and a registry of saved SSH connections whose backend counts every remote call.

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
use crate::local_server::service::maintenance::{MaintenanceFacts, RemoteBuild};
use crate::local_server::service::{ServiceStartup, ServiceStatus};
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
use crate::ssh::server_operations::{
    RemoteMaintenanceRequest, RemoteOperationEntry, RemoteOperationHandle, RemoteOperationState,
    RemoteServerOperations,
};
use crate::ui::appearance_runtime;
use crate::ui::settings_window::{SettingsSectionId, SettingsWindow};

const NAVIGATION_SELECTOR: &str = "settings-navigation-settings-section-servers";

/// The SSH computers the test reaches, counting every request that would leave this computer.
#[derive(Default)]
pub(in crate::ui::settings_window) struct Computers {
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
        mode: ConnectMode,
        _: Option<&SshServerContinuity>,
        _: &SshCancellationToken,
        _: &ConnectProgress<'_>,
    ) -> Result<ConnectedSshServer, SshConnectFailure> {
        self.connects.fetch_add(1, Ordering::SeqCst);
        // As after a relaunch, nothing proves a route signs in without a prompt.
        if mode == ConnectMode::QuietContinuation {
            return Err(SshConnectFailure::NeedsPerson(
                crate::ssh::server_connection::InteractionReason::FirstSignIn,
            ));
        }
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
pub(in crate::ui::settings_window) struct SavedConnections {
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

pub(in crate::ui::settings_window) fn destination(name: &str) -> SshDestination {
    SshDestination::new(name.to_owned()).expect("a valid SSH destination")
}

pub(in crate::ui::settings_window) struct Servers<'a> {
    pub(in crate::ui::settings_window) window: Entity<SettingsWindow>,
    pub(in crate::ui::settings_window) computers: Arc<Computers>,
    pub(in crate::ui::settings_window) saved: Arc<SavedConnections>,
    pub(in crate::ui::settings_window) connections: SshServerConnections,
    pub(in crate::ui::settings_window) storage: Arc<MemoryStorage>,
    pub(in crate::ui::settings_window) cx: &'a mut VisualTestContext,
}

/// Opens Settings on Servers with the local server when `local` is set and each `saved` SSH
/// connection listed as it is after a relaunch: saved, inactive, and never reached.
pub(in crate::ui::settings_window) fn open_servers<'a>(
    local: bool,
    saved: &[&str],
    cx: &'a mut TestAppContext,
) -> Servers<'a> {
    open_servers_saved(local, saved, false, None, None, cx)
}

/// Opens Settings on Servers as [`open_servers`] does, with each `saved` connection set to
/// reconnect when `active`, SSH maintenance through `operations` and paired servers through
/// `direct` when given.
fn open_servers_saved<'a>(
    local: bool,
    saved: &[&str],
    active: bool,
    operations: Option<Arc<dyn RemoteServerOperations>>,
    direct: Option<crate::direct_connections::DirectConnections>,
    cx: &'a mut TestAppContext,
) -> Servers<'a> {
    let computers = Arc::new(Computers::default());
    let records = Arc::new(SavedConnections::default());
    *records.records.lock().unwrap() = saved
        .iter()
        .map(|name| RetainedSshConnection {
            destination: destination(name),
            continuity: None,
            active,
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
        if let Some(operations) = operations {
            crate::ssh::server_operations::install(operations, cx);
        }
        if let Some(direct) = direct {
            crate::ui::direct_servers::install(direct, cx);
        }
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

pub(in crate::ui::settings_window) fn click(selector: &str, cx: &mut VisualTestContext) {
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
fn footnote(servers: &Servers<'_>) -> Option<String> {
    servers.window.read_with(servers.cx, |settings, cx| {
        settings
            .server_card_footnote(cx)
            .map(|footnote| footnote.to_string())
    })
}

#[gpui::test]
fn all_servers_names_this_computer_first_then_each_saved_ssh_server(cx: &mut TestAppContext) {
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
    assert!(
        tree.node(local_name())["aria"]["description"]
            .as_str()
            .is_some_and(|description| description.contains("Running \u{b7} this account")),
        "each server says how it reads now"
    );
    assert_eq!(
        tree.node("penguin")["aria"]["description"],
        "penguin, Not connected",
        "an SSH server names its machine and its short status"
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
    assert_eq!(picker["aria"]["description"], "Choose a server, or add one");
    assert_eq!(
        servers
            .cx
            .update(|_, cx| super::super::server_selection::selected(cx)),
        crate::server_context::ServerKey::Ssh("penguin".to_owned()),
        "the choice is retained for Git and the next launch"
    );
    assert_eq!(status(&servers), "Not connected");
    assert_eq!(
        servers.computers.remote_calls(),
        0,
        "choosing a server connects to, starts and installs nothing"
    );

    click("settings-navigation-settings-section-git", servers.cx);
    assert_eq!(
        A11yTree::read(servers.cx).node(super::PICKER_NAME)["aria"]["value"],
        "penguin",
        "Git presents the same server"
    );
}

#[gpui::test]
fn an_inactive_ssh_server_offers_check_status_and_connect_and_dims_its_settings(
    cx: &mut TestAppContext,
) {
    let servers = open_servers(true, &["penguin"], cx);
    select("penguin", servers.cx);

    for row in [
        "settings-row-server-status",
        "settings-row-server-version",
        "settings-row-server-auto-start",
        "settings-row-end-all-sessions",
        "settings-row-network-access",
        "settings-ssh-server-check",
        "settings-ssh-server-connect",
    ] {
        assert!(rendered(row, servers.cx), "{row} presents penguin");
    }
    assert_eq!(
        footnote(&servers).as_deref(),
        Some("Connect to penguin to change these settings.")
    );
    let network = servers
        .window
        .read_with(servers.cx, |settings, _| settings.network_access_footnote());
    assert_eq!(
        network.as_deref(),
        Some("Connect to penguin to change its network access.")
    );
    assert_eq!(servers.computers.remote_calls(), 0);
}

/// A build with no remote operations owner offers no remote maintenance, so the SSH server's
/// menu offers only forgetting it.
#[gpui::test]
fn without_an_operations_owner_an_ssh_server_offers_no_maintenance(cx: &mut TestAppContext) {
    let servers = open_servers(true, &["penguin"], cx);
    select("penguin", servers.cx);

    click(super::super::server_card::MENU_SELECTOR, servers.cx);

    let tree = A11yTree::read(servers.cx);
    for maintenance in ["Restart Server", "Stop Server", "Uninstall Server\u{2026}"] {
        assert_eq!(
            tree.node(maintenance)["aria"]["disabled"],
            true,
            "{maintenance} needs an operations owner this build has none of"
        );
    }
    assert_ne!(tree.node("Remove Server\u{2026}")["aria"]["disabled"], true);
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
    assert!(!rendered("settings-server-remove", servers.cx));
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
    assert_eq!(
        status(&servers),
        "Running \u{b7} not connected",
        "the check reports the server it read and connects nothing"
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
    assert_eq!(status(&servers), "Connected over SSH");
    assert_eq!(
        footnote(&servers),
        None,
        "a connected server's settings can change"
    );
    assert_eq!(computers.observations.load(Ordering::SeqCst), 1);
}

#[gpui::test]
fn after_disconnect_the_card_presents_the_facts_last_observed(cx: &mut TestAppContext) {
    let servers = open_servers(true, &["penguin"], cx);
    select("penguin", servers.cx);
    click("settings-ssh-server-connect", servers.cx);
    let connections = servers.connections.clone();
    settle(servers.cx, |_| {
        connections.server(&destination("penguin")).is_some()
    });
    servers.cx.run_until_parked();

    click("settings-ssh-server-disconnect", servers.cx);

    assert!(
        A11yTree::read(servers.cx).find_text("1.4.0").is_some(),
        "the version stays presented"
    );
    let footnote = footnote(&servers).unwrap_or_default();
    assert!(
        footnote.starts_with("Connect to penguin to change these settings. Shown values are from "),
        "the card says the values were observed earlier, got {footnote:?}"
    );
    assert_eq!(servers.computers.observations.load(Ordering::SeqCst), 0);
}

/// An operations owner holding at most one service read of penguin, as Check Now leaves it, and
/// when that read was observed. Every step it is asked for does nothing.
struct HeldRead {
    held: Mutex<Option<(MaintenanceFacts, std::time::Instant)>>,
    changes: (async_channel::Sender<()>, async_channel::Receiver<()>),
}

/// What a read of penguin reports: the service in `status`, with `version` approved.
fn read_facts(version: &str, status: ServiceStatus) -> MaintenanceFacts {
    MaintenanceFacts {
        server: [2; 32],
        account: 501,
        build: "spaceterm-development".to_owned(),
        status,
        startup: ServiceStartup::AtLogin,
        approved: Some(RemoteBuild {
            version: version.to_owned(),
            commit: "a".repeat(40),
            digest: "a".repeat(64),
        }),
        activation: None,
    }
}

const RUNNING_SERVICE: ServiceStatus = ServiceStatus {
    installed: true,
    enabled: true,
    running: true,
    stopped: false,
};

const STOPPED_SERVICE: ServiceStatus = ServiceStatus {
    installed: true,
    enabled: true,
    running: false,
    stopped: true,
};

impl HeldRead {
    /// An owner that has read nothing yet.
    fn unread() -> Self {
        Self {
            held: Mutex::new(None),
            changes: async_channel::unbounded(),
        }
    }

    fn approving(version: &str) -> Self {
        Self::reporting(version, RUNNING_SERVICE)
    }

    /// An owner whose read, observed now, found the service in `status`.
    fn reporting(version: &str, status: ServiceStatus) -> Self {
        let owner = Self::unread();
        owner.report(version, status);
        owner
    }

    /// A read observed now, which replaces the one held and tells the surface.
    fn report(&self, version: &str, status: ServiceStatus) {
        *self.held.lock().unwrap() = Some((read_facts(version, status), std::time::Instant::now()));
        let _ = self.changes.0.try_send(());
    }
}

impl RemoteServerOperations for HeldRead {
    fn subscribe(&self) -> async_channel::Receiver<()> {
        self.changes.1.clone()
    }

    fn entries(&self) -> Vec<RemoteOperationEntry> {
        self.held
            .lock()
            .unwrap()
            .iter()
            .map(|(facts, observed_at)| RemoteOperationEntry {
                destination: destination("penguin"),
                removed: false,
                operation: None,
                state: RemoteOperationState::Read(Box::new(facts.clone())),
                observed_at: Some(*observed_at),
            })
            .collect()
    }

    fn state(&self, server: &SshDestination) -> RemoteOperationState {
        match &*self.held.lock().unwrap() {
            Some((facts, _)) if *server == destination("penguin") => {
                RemoteOperationState::Read(Box::new(facts.clone()))
            }
            _ => RemoteOperationState::Unread,
        }
    }

    fn refresh(&self, _: &SshDestination) {}
    fn set_automatic_start(&self, _: &SshDestination, _: bool) {}
    fn describe(&self, _: &SshDestination, _: RemoteMaintenanceRequest) {}
    fn confirm(&self, _: RemoteOperationHandle) {}
    fn start(&self, _: &SshDestination) {}
    fn cancel(&self, _: &SshDestination) {}
    fn resume(&self, _: &SshDestination) {}
    fn acknowledge(&self, _: RemoteOperationHandle) {}
    fn discard(&self, _: [u8; 32]) {}
    fn dismiss(&self, _: [u8; 32]) {}
    fn resume_retained(&self) {}
}

/// Whether the Version row announces `version`.
fn version_reads(version: &str, cx: &mut VisualTestContext) -> bool {
    let exact = format!("Version {version}");
    let with_badge = format!("Version {version},");
    A11yTree::read(cx).with_role("Status").iter().any(|node| {
        node["aria"]["label"]
            .as_str()
            .is_some_and(|label| label == exact || label.starts_with(&with_badge))
    })
}

/// A service read, such as Check Now leaves, names the build approved when it ran. Once
/// SpaceTerm connects, the server reports the version it runs now, which the card shows instead.
#[gpui::test]
fn a_connected_server_shows_the_version_it_reports_over_an_earlier_read(cx: &mut TestAppContext) {
    let operations: Arc<dyn RemoteServerOperations> = Arc::new(HeldRead::approving("1.3.0"));
    let servers = open_servers_saved(true, &["penguin"], false, Some(operations), None, cx);
    select("penguin", servers.cx);
    assert!(
        version_reads("1.3.0", servers.cx),
        "before connecting, the read's approved build is what SpaceTerm knows"
    );

    click("settings-ssh-server-connect", servers.cx);
    let connections = servers.connections.clone();
    settle(servers.cx, |_| {
        connections.server(&destination("penguin")).is_some()
    });
    servers.cx.run_until_parked();

    assert!(
        version_reads("1.4.0", servers.cx),
        "the connected server's own version replaces the earlier read"
    );
    assert!(!version_reads("1.3.0", servers.cx));
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
    let (direct, routes) = crate::direct_connections::tests::offline_routes_at(&[
        ("studio", "wss://192.168.1.10:9231"),
        ("studio", "wss://192.168.1.11:9231"),
    ]);
    let servers = open_servers_saved(true, &[], false, None, Some(direct), cx);

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
        servers
            .cx
            .update(|_, cx| super::super::server_selection::selected(cx)),
        ServerChoice::Direct(routes[1]).key(),
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

/// A read found the server stopped. Someone started it elsewhere, and Check Status, asked for
/// later, finds it running. The later report is what the card says and what SpaceTerm keeps.
#[gpui::test]
fn a_later_check_status_replaces_an_earlier_read(cx: &mut TestAppContext) {
    let operations: Arc<dyn RemoteServerOperations> =
        Arc::new(HeldRead::reporting("1.3.0", STOPPED_SERVICE));
    let servers = open_servers_saved(true, &["penguin"], false, Some(operations), None, cx);
    select("penguin", servers.cx);
    assert!(
        A11yTree::read(servers.cx)
            .find("penguin: Stopped")
            .is_some(),
        "the read is all SpaceTerm knows at first"
    );

    click("settings-ssh-server-check", servers.cx);
    let connections = servers.connections.clone();
    settle(servers.cx, |_| {
        matches!(
            connections.status(&destination("penguin")),
            RemoteConnectionStatus::Checked { .. }
        )
    });
    servers.cx.run_until_parked();

    assert!(
        A11yTree::read(servers.cx)
            .find("penguin: Running \u{b7} not connected")
            .is_some(),
        "the later check says the server runs"
    );
    assert!(version_reads("1.4.0", servers.cx));
    let retained = servers.cx.update(|_, cx| {
        super::super::server_selection::facts(&ServerChoice::Ssh(destination("penguin")).key(), cx)
    });
    assert_eq!(
        retained.map(|facts| (facts.status, facts.version)),
        Some((
            crate::server_context::ObservedServerStatus::Running,
            Some("1.4.0".to_owned())
        )),
        "SpaceTerm keeps the later report"
    );
}

/// Check Status found the server running. A service read asked for later finds it stopped, as
/// after Stop Server from another device. The later read is what the card says and keeps.
#[gpui::test]
fn a_later_read_replaces_an_earlier_check_status(cx: &mut TestAppContext) {
    let owner = Arc::new(HeldRead::unread());
    let operations: Arc<dyn RemoteServerOperations> = owner.clone();
    let servers = open_servers_saved(true, &["penguin"], false, Some(operations), None, cx);
    select("penguin", servers.cx);
    click("settings-ssh-server-check", servers.cx);
    let connections = servers.connections.clone();
    settle(servers.cx, |_| {
        matches!(
            connections.status(&destination("penguin")),
            RemoteConnectionStatus::Checked { .. }
        )
    });
    servers.cx.run_until_parked();
    assert!(
        A11yTree::read(servers.cx)
            .find("penguin: Running \u{b7} not connected")
            .is_some(),
        "Check Status says the server runs"
    );

    owner.report("1.3.0", STOPPED_SERVICE);
    servers.cx.run_until_parked();

    assert!(
        A11yTree::read(servers.cx)
            .find("penguin: Stopped")
            .is_some(),
        "the later read says the server is stopped"
    );
    assert!(version_reads("1.3.0", servers.cx));
    let retained = servers.cx.update(|_, cx| {
        super::super::server_selection::facts(&ServerChoice::Ssh(destination("penguin")).key(), cx)
    });
    assert_eq!(
        retained.map(|facts| (facts.status, facts.version)),
        Some((
            crate::server_context::ObservedServerStatus::Stopped,
            Some("1.3.0".to_owned())
        )),
        "SpaceTerm keeps the later read"
    );
}

/// The Version row's announcement, such as "Version 1.4.0, 1.5.0 available".
fn version_announcement(cx: &mut VisualTestContext) -> Option<String> {
    A11yTree::read(cx)
        .with_role("Status")
        .into_iter()
        .filter_map(|node| node["aria"]["label"].as_str())
        .find(|label| label.starts_with("Version "))
        .map(str::to_owned)
}

/// A read names the build approved on penguin but no verified candidate, so SpaceTerm knows of
/// no update. The Version row neither says one is available nor offers Update Server.
#[gpui::test]
fn an_ssh_server_without_a_verified_candidate_claims_no_update(cx: &mut TestAppContext) {
    let operations: Arc<dyn RemoteServerOperations> = Arc::new(HeldRead::approving("1.3.0"));
    let servers = open_servers_saved(true, &["penguin"], false, Some(operations), None, cx);
    select("penguin", servers.cx);

    let announcement = version_announcement(servers.cx);
    assert!(
        announcement
            .as_deref()
            .is_some_and(|label| label.starts_with("Version 1.3.0")
                && !label.contains("available")),
        "the approved build shows with no update claimed, got {announcement:?}"
    );
    assert!(!rendered("settings-server-version-update", servers.cx));
}

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
fn removing_the_selected_server_forgets_it_and_presents_this_computer(cx: &mut TestAppContext) {
    let servers = open_servers(true, &["build.local", "penguin"], cx);
    select("penguin", servers.cx);

    click(super::super::server_card::MENU_SELECTOR, servers.cx);
    click("settings-server-remove", servers.cx);
    assert_eq!(
        servers.saved.destinations(),
        ["build.local", "penguin"],
        "Remove Server asks first"
    );
    answer_alert("settings-server-remove-confirm", servers.cx);

    assert_eq!(
        listed(servers.cx),
        vec![
            (local_name().to_owned(), true),
            ("build.local".to_owned(), false)
        ],
        "the picker moves to this computer's server"
    );
    let saved = servers.saved.clone();
    settle(servers.cx, |_| saved.destinations() == ["build.local"]);
    assert_eq!(
        servers
            .cx
            .update(|_, cx| super::super::server_selection::selected(cx)),
        crate::server_context::ServerKey::Local
    );
    assert_eq!(
        servers.computers.remote_calls(),
        0,
        "removing reaches no computer, so SSH access and the server stay as they were"
    );
}

#[gpui::test]
fn without_a_local_server_removing_presents_the_first_saved_server(cx: &mut TestAppContext) {
    let servers = open_servers(false, &["alpha", "bravo", "charlie"], cx);
    select("bravo", servers.cx);

    click(super::super::server_card::MENU_SELECTOR, servers.cx);
    click("settings-server-remove", servers.cx);
    answer_alert("settings-server-remove-confirm", servers.cx);

    assert_eq!(
        listed(servers.cx),
        vec![("alpha".to_owned(), true), ("charlie".to_owned(), false)]
    );
}

/// A choice retained from an earlier launch waits for the saved connections to load rather than
/// giving way to this computer's server while they do.
#[gpui::test]
fn a_retained_choice_returns_once_its_saved_connection_loads(cx: &mut TestAppContext) {
    cx.update(|cx| {
        super::super::server_selection::select(
            crate::server_context::ServerKey::Ssh("penguin".to_owned()),
            cx,
        )
    });

    let servers = open_servers(true, &["penguin"], cx);

    assert_eq!(listed(servers.cx)[1], ("penguin".to_owned(), true));
}

/// Loaded saved connections that no longer include the retained server are a real answer: the
/// choice and its facts go, and this computer's server is presented.
#[gpui::test]
fn a_retained_choice_whose_connection_is_gone_gives_way_once_loading_finishes(
    cx: &mut TestAppContext,
) {
    let gone = crate::server_context::ServerKey::Ssh("gone".to_owned());
    cx.update(|cx| {
        super::super::server_selection::select(gone.clone(), cx);
        super::super::server_selection::remember(
            gone.clone(),
            crate::server_context::LastKnownFacts {
                observed_at: 1,
                ..Default::default()
            },
            cx,
        );
    });

    let servers = open_servers(true, &[], cx);
    let connections = servers.connections.clone();
    settle(servers.cx, |_| connections.retained_inventory_loaded());
    servers.cx.run_until_parked();

    assert_eq!(
        servers
            .cx
            .update(|_, cx| super::super::server_selection::selected(cx)),
        crate::server_context::ServerKey::Local
    );
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
    let mut servers = open_servers(true, &["build.local", "penguin"], cx);
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
    assert_eq!(servers.computers.remote_calls(), 0);
}

/// A word that only appears inside a host's name answers the Settings it names, not that host.
/// "servers" names All Servers, so the list shows which server the search presents.
#[gpui::test]
fn a_query_inside_a_host_name_keeps_the_local_server(cx: &mut TestAppContext) {
    let mut servers = open_servers(true, &["build.servers"], cx);
    search_for("servers", &mut servers);

    assert_eq!(listed(servers.cx)[0], (local_name().to_owned(), true));
    assert_eq!(listed(servers.cx)[1], ("build.servers".to_owned(), false));
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

/// A host whose requested Connect failed stays saved to connect when SpaceTerm reopens, so its
/// Status offers ending that without removing the saved connection.
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
    assert_eq!(status(&servers), "Not reachable");
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

/// A saved connection that reopens waiting for the person waits for Connect.
#[gpui::test]
fn a_reopened_connection_waits_for_connect(cx: &mut TestAppContext) {
    let servers = open_servers_saved(true, &["penguin"], true, None, None, cx);
    let connections = servers.connections.clone();
    settle(servers.cx, |_| {
        matches!(
            connections.status(&destination("penguin")),
            RemoteConnectionStatus::NeedsPerson { .. }
        )
    });
    select("penguin", servers.cx);

    assert_eq!(status(&servers), "Waiting for you");
    assert!(rendered("settings-ssh-server-connect", servers.cx));
}

/// A removal SpaceTerm couldn't save would bring the connection back at relaunch, so a notice
/// names it and offers to remove it again.
#[gpui::test]
fn a_removal_that_could_not_be_saved_names_the_host_and_can_be_retried(cx: &mut TestAppContext) {
    let notice = "settings-server-notice-unsaved-removal";
    let servers = open_servers(true, &["penguin"], cx);
    servers.saved.removal_fails.store(true, Ordering::SeqCst);
    select("penguin", servers.cx);
    click(super::super::server_card::MENU_SELECTOR, servers.cx);
    click("settings-server-remove", servers.cx);
    answer_alert("settings-server-remove-confirm", servers.cx);
    let connections = servers.connections.clone();
    settle(servers.cx, |_| !connections.unsaved_removals().is_empty());
    servers.cx.run_until_parked();

    let text = A11yTree::read(servers.cx)
        .with_role("Status")
        .into_iter()
        .find_map(|node| {
            node["aria"]["label"]
                .as_str()
                .filter(|label| label.contains("couldn\u{2019}t save that you removed"))
                .map(str::to_owned)
        })
        .unwrap_or_default();
    assert!(text.contains("penguin"), "got {text:?}");
    assert!(text.contains("reopens"), "got {text:?}");

    servers.saved.removal_fails.store(false, Ordering::SeqCst);
    click(&format!("{notice}-remove-again"), servers.cx);
    settle(servers.cx, |_| connections.unsaved_removals().is_empty());
    assert!(servers.saved.destinations().is_empty());
    let connections = servers.connections.clone();
    settle(servers.cx, |_| connections.persistence_error().is_none());
    assert!(
        !rendered(notice, servers.cx),
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

    servers.window.update(servers.cx, |settings, cx| {
        settings.select_server(super::ServerChoice::Ssh(destination("bravo")), cx);
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
    click("settings-navigation-settings-section-git", servers.cx);
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
            controls: None,
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
