//! Network Access, paired devices and unpair notices in the Servers section, through a scripted
//! server and a real offline registry of direct connections.
//!
//! The scripted server stands in for the network boundary: it answers each direct network
//! operation as the test sets and records what the Settings window asked of it. Invite Device's
//! own journey is tested with its sheet in `ui::direct_pairing`; these tests cover what Servers
//! does around it.

use std::net::{IpAddr, SocketAddr};
use std::rc::Rc;
use std::sync::{Arc, Mutex};

use gpui::{Entity, KeyUpEvent, Keystroke, Modifiers, TestAppContext, VisualTestContext};
use spaceterm_ui::a11y_testing::A11yTree;
use zeroize::Zeroizing;

use super::super::direct_routes::{UNPAIR_SELF_CONFIRM_SELECTOR, UNSAVED_CHANGES};
use super::super::notices::NOTICE_SELECTOR;
use super::super::server_card::MENU_SELECTOR;
use super::super::server_list::LIST_SELECTOR;
use super::super::server_selection;
use super::super::servers::testing::connected;
use super::super::servers::{ConnectedServer, LocalServerControl, OfferedServer};
use super::{
    ACCESS_SELECTOR, ADDRESS_SELECTOR, DEVICE_SELECTOR, INVITE_SELECTOR, REMOVE_CANCEL_SELECTOR,
    REMOVE_CONFIRM_SELECTOR, TURN_OFF_CONFIRM_SELECTOR,
};
use crate::appearance::Appearance;
use crate::direct_connections::{DirectConnections, DirectRouteStatus, UnpairState};
use crate::platform::appearance::testing::RecordingAppearancePlatform;
use crate::platform::window_movement::RecordingOperatingSystemWindowDragPlatform;
use crate::server_connection::ServerError;
use crate::server_connection::direct::{ClaimId, NetworkAddresses, NetworkPolicy, PairingClaim};
use crate::server_context::{
    LastKnownFacts, NetworkAccessFacts, NetworkListenerFacts, PairedDeviceFacts,
    PairedDevicesFacts, ServerKey,
};
use crate::settings::SettingsDocument;
use crate::settings::storage::testing::MemoryStorage;
use crate::ui::appearance_runtime;
use crate::ui::direct_access::{
    AccessFailure, AccessState, Claim, Device, DirectAccessControl, Listener, OpenInvitation,
    unix_now,
};
use crate::ui::settings_window::SettingsWindow;

const NAVIGATION_SELECTOR: &str = "settings-navigation-settings-section-servers";
const SERVER: &str = "studio";
const INVITATION: &str = "spaceterm://pair/test-invitation";
const INVITATION_ID: ClaimId = [7; 32];

/// A server's direct network operations as the test scripts them.
struct ScriptedAccess {
    /// What Status answers. A failure stands for a server SpaceTerm can't reach.
    status: Mutex<Result<AccessState, AccessFailure>>,
    /// The refusal the next policy change returns, leaving the state unchanged.
    refusal: Mutex<Option<AccessFailure>>,
    policies: Mutex<Vec<Option<NetworkPolicy>>>,
    /// Every policy change exactly as asked: the address, if any, and whether access is on.
    requests: Mutex<Vec<(Option<NetworkPolicy>, bool)>>,
    claims: Mutex<Vec<Claim>>,
    devices: Mutex<Vec<Device>>,
    /// Why the server can't list its devices, when it can't.
    devices_failure: Mutex<Option<AccessFailure>>,
}

impl ScriptedAccess {
    fn new(listener: Listener, addresses: Option<NetworkAddresses>) -> Arc<Self> {
        let policy = match listener {
            Listener::Listening(address) => Some(address),
            _ => None,
        };
        Self::with_state(AccessState {
            server_name: SERVER.into(),
            policy,
            configured: policy,
            listener,
            addresses,
        })
    }

    fn with_state(state: AccessState) -> Arc<Self> {
        Arc::new(Self {
            status: Mutex::new(Ok(state)),
            refusal: Mutex::default(),
            policies: Mutex::default(),
            requests: Mutex::default(),
            claims: Mutex::default(),
            devices: Mutex::default(),
            devices_failure: Mutex::default(),
        })
    }

    /// SpaceTerm can no longer reach the server, as after a disconnect or a lost connection. The
    /// server itself keeps its access and devices.
    fn become_unreachable(&self) {
        *self.status.lock().expect("status") = Err(AccessFailure::Unreachable);
    }

    /// Off, with two addresses on the server and the routing table preferring the second.
    fn off() -> Arc<Self> {
        Self::new(
            Listener::Off,
            Some(addresses(
                &["192.168.1.20", "192.168.1.21"],
                Some("192.168.1.21"),
            )),
        )
    }

    fn listening() -> Arc<Self> {
        Self::new(
            Listener::Listening(address("192.168.1.20:9231")),
            Some(addresses(&["192.168.1.20"], Some("192.168.1.20"))),
        )
    }

    fn policies(&self) -> Vec<Option<NetworkPolicy>> {
        self.policies.lock().expect("policies").clone()
    }

    fn requests(&self) -> Vec<(Option<NetworkPolicy>, bool)> {
        self.requests.lock().expect("requests").clone()
    }

    fn pair(&self, id: u8, name: &str, last_connected_at: Option<u64>) {
        self.devices
            .lock()
            .expect("devices")
            .push(device(id, name, last_connected_at));
    }

    fn unpair(&self, name: &str) {
        self.devices
            .lock()
            .expect("devices")
            .retain(|device| device.name.as_ref() != name);
    }

    /// A device named `name` claims the invitation Invite Device opened.
    fn claim(&self, id: u8, name: &str) {
        let claim = PairingClaim {
            id: [id; 32],
            invitation_id: INVITATION_ID,
            server: [1; 32],
            device_public_key: [id; 32],
            device_name: name.into(),
            expires_at_unix_seconds: unix_now() + 300,
            exchange_hash: [0; 32],
        };
        self.claims.lock().expect("claims").push(Claim {
            id: claim.id,
            name: name.to_owned().into(),
            code: claim.comparison_code(),
            claim,
        });
    }
}

impl DirectAccessControl for ScriptedAccess {
    fn status(&self) -> Result<AccessState, AccessFailure> {
        self.status.lock().expect("status").clone()
    }

    fn set_policy(
        &self,
        policy: Option<NetworkPolicy>,
        enabled: bool,
    ) -> Result<AccessState, AccessFailure> {
        self.requests
            .lock()
            .expect("requests")
            .push((policy.clone(), enabled));
        // Records what the person turned on, or `None` for off, as the server applies it.
        self.policies
            .lock()
            .expect("policies")
            .push(policy.clone().filter(|_| enabled));
        if let Some(refusal) = *self.refusal.lock().expect("refusal") {
            return Err(refusal);
        }
        let mut status = self.status.lock().expect("status");
        let state = status.as_mut().map_err(|failure| *failure)?;
        if let Some(policy) = &policy {
            state.configured = Some(policy.bind);
        }
        state.policy = state.configured.filter(|_| enabled);
        state.listener = state.policy.map_or(Listener::Off, Listener::Listening);
        Ok(state.clone())
    }

    fn invite(&self) -> Result<OpenInvitation, AccessFailure> {
        Ok(OpenInvitation {
            id: INVITATION_ID,
            text: Zeroizing::new(INVITATION.to_owned()),
            deadline: std::time::Instant::now() + std::time::Duration::from_secs(300),
        })
    }

    fn withdraw(&self, _: ClaimId) -> Result<(), AccessFailure> {
        Ok(())
    }

    fn claims(&self) -> Result<Vec<Claim>, AccessFailure> {
        Ok(self.claims.lock().expect("claims").clone())
    }

    fn approve(&self, claim: &PairingClaim) -> Result<Device, AccessFailure> {
        let paired = device(claim.id[0], claim.device_name.as_str(), None);
        self.devices.lock().expect("devices").push(paired.clone());
        Ok(paired)
    }

    fn reject(&self, _: ClaimId) -> Result<(), AccessFailure> {
        Ok(())
    }

    fn devices(&self) -> Result<Vec<Device>, AccessFailure> {
        if let Some(failure) = *self.devices_failure.lock().expect("devices failure") {
            return Err(failure);
        }
        Ok(self.devices.lock().expect("devices").clone())
    }
}

/// The account-local server, whose direct network access the scripted operations answer. It
/// offers no account connection, so nothing here reaches a real server.
struct ScriptedLocal(Arc<ScriptedAccess>);

impl LocalServerControl for ScriptedLocal {
    fn direct_access(&self) -> Option<Arc<dyn DirectAccessControl>> {
        Some(Arc::clone(&self.0) as Arc<dyn DirectAccessControl>)
    }

    fn refresh(&self) -> Result<ConnectedServer, ServerError> {
        Ok(connected(2))
    }
}

fn address(text: &str) -> SocketAddr {
    text.parse().expect("a test address")
}

fn ip(text: &str) -> IpAddr {
    text.parse().expect("a test address")
}

fn addresses(available: &[&str], suggested: Option<&str>) -> NetworkAddresses {
    NetworkAddresses {
        available: available.iter().map(|text| ip(text)).collect(),
        suggested: suggested.map(ip),
    }
}

fn policy(text: &str) -> Option<NetworkPolicy> {
    Some(NetworkPolicy::new(address(text)).expect("an address the server can listen at"))
}

fn device(id: u8, name: &str, last_connected_at: Option<u64>) -> Device {
    Device {
        pairing: [id; 32],
        name: name.to_owned().into(),
        fingerprint: fingerprint(id).into(),
        last_connected_at,
    }
}

/// A device fingerprint in the form the server writes: eight groups of eight hex digits.
fn fingerprint(id: u8) -> String {
    vec![format!("{id:02X}").repeat(4); 8].join(" ")
}

/// What a previous launch retained about a server that let MacBook connect at 192.168.1.20, an
/// hour ago. It carries no general observation of the server.
fn retained_on() -> LastKnownFacts {
    let observed_at = unix_now() - 3600;
    LastKnownFacts {
        network_access: Some(NetworkAccessFacts {
            observed_at,
            server_name: SERVER.to_owned(),
            policy: Some(address("192.168.1.20:9231")),
            listener: NetworkListenerFacts::Listening(address("192.168.1.20:9231")),
            addresses: None,
        }),
        paired_devices: Some(PairedDevicesFacts {
            observed_at,
            devices: vec![PairedDeviceFacts {
                pairing: [3; 32],
                name: "MacBook".to_owned(),
                fingerprint: fingerprint(3),
                last_connected_at: None,
            }],
        }),
        ..LastKnownFacts::default()
    }
}

/// Retains `facts` for `key`, as a previous launch's record does once it loads.
fn retain(key: ServerKey, facts: LastKnownFacts, cx: &mut gpui::App) {
    server_selection::remember(key, facts, cx);
}

/// Saved paired servers named `labels`, disconnected, so nothing reaches any computer.
fn paired(
    labels: &[&str],
) -> (
    DirectConnections,
    Vec<crate::direct_connections::DirectRouteId>,
) {
    let routes: Vec<(&str, &str)> = labels
        .iter()
        .map(|label| (*label, "wss://192.168.1.30:9231"))
        .collect();
    crate::direct_connections::tests::disconnected_routes_at(&routes)
}

/// What SpaceTerm retains about this computer's server for the next launch.
fn retained_here(cx: &mut VisualTestContext) -> LastKnownFacts {
    cx.update(|_, cx| server_selection::facts(&ServerKey::Local, cx))
        .unwrap_or_default()
}

/// Opens Settings on the Servers section, presenting the local server's direct network access.
fn open_servers<'a>(
    access: &Arc<ScriptedAccess>,
    cx: &'a mut TestAppContext,
) -> (Entity<SettingsWindow>, &'a mut VisualTestContext) {
    open_servers_with(access, None, cx)
}

/// Opens Settings on the Servers section with `direct` as the desktop's direct registry.
fn open_servers_with<'a>(
    access: &Arc<ScriptedAccess>,
    direct: Option<DirectConnections>,
    cx: &'a mut TestAppContext,
) -> (Entity<SettingsWindow>, &'a mut VisualTestContext) {
    let settings =
        crate::settings::Settings::load(MemoryStorage::with_document(&SettingsDocument::default()));
    let platform = RecordingAppearancePlatform::default();
    platform.set_system_appearance(Some(Appearance::Dark));
    cx.update(|cx| {
        appearance_runtime::install(settings, Rc::new(platform), cx)
            .expect("appearance runtime should install");
        crate::ui::init(cx).expect("UI initialization should succeed");
        if let Some(direct) = direct {
            crate::ui::direct_servers::install(direct, cx);
        }
    });
    let local = Some(OfferedServer {
        connection: Ok(Arc::new(ScriptedLocal(Arc::clone(access))) as Arc<dyn LocalServerControl>),
        service: None,
        update: None,
    });
    let (window, cx) = cx.add_window_view(|window, cx| {
        SettingsWindow::new_with_capabilities(
            Rc::new(RecordingOperatingSystemWindowDragPlatform::default()),
            Default::default(),
            None,
            local,
            window,
            cx,
        )
    });
    cx.update(|window, _| window.activate_window());
    cx.run_until_parked();
    click(NAVIGATION_SELECTOR, cx);
    (window, cx)
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

/// Presses Return the way a keyboard does. A focused button acts when the key comes back up, so
/// a key-down alone leaves the focused action unpressed.
fn press_return(cx: &mut VisualTestContext) {
    cx.simulate_keystrokes("enter");
    cx.simulate_event(KeyUpEvent {
        keystroke: Keystroke::parse("enter").expect("Return parses"),
    });
    cx.run_until_parked();
}

/// The selector the modal gives an action.
fn modal_action(selector: &str) -> String {
    format!("modal-action-{selector}")
}

fn open(selector: &str, cx: &mut VisualTestContext) -> bool {
    let selector: &'static str = Box::leak(selector.to_owned().into_boxed_str());
    cx.debug_bounds(selector).is_some()
}

/// Lets the rows read the server again, as they do every two seconds.
fn reread(cx: &mut VisualTestContext) {
    cx.executor()
        .advance_clock(std::time::Duration::from_secs(2));
    cx.run_until_parked();
}

/// Repaints until `done` holds, while the registry's worker runs on its own threads.
#[track_caller]
fn settle(cx: &mut VisualTestContext, mut done: impl FnMut(&mut VisualTestContext) -> bool) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        cx.run_until_parked();
        if done(cx) {
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the registry reaches the expected state"
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

/// Whether assistive technology reads `text` anywhere in the window: in a label, an error under
/// a control, a card's footnote or a notice.
fn reads(text: &str, cx: &mut VisualTestContext) -> bool {
    let tree = A11yTree::read(cx);
    ["Label", "Alert", "Note", "Status"].iter().any(|role| {
        tree.with_role(role).iter().any(|node| {
            ["label", "value"].iter().any(|field| {
                node["aria"][field]
                    .as_str()
                    .is_some_and(|value| value.contains(text))
            })
        })
    })
}

fn access_on(window: &Entity<SettingsWindow>, cx: &mut VisualTestContext) -> bool {
    window.read_with(cx, |settings, _| settings.network_access_on())
}

/// The notice for the registry's first unpair request.
fn unpair_notice(direct: &DirectConnections) -> String {
    let key = direct
        .pending_unpairs()
        .first()
        .expect("an unpair request")
        .key;
    let hex: String = key.iter().map(|byte| format!("{byte:02x}")).collect();
    format!("{NOTICE_SELECTOR}-unpair-{hex}")
}

/// Selects the paired server the offline registry saved, after this computer's server.
fn select_paired_server(cx: &mut VisualTestContext) {
    let paired = format!("{LIST_SELECTOR}-1");
    settle(cx, |cx| open(&paired, cx));
    click(&paired, cx);
}

#[gpui::test]
fn turning_on_listens_at_the_address_the_server_suggests_on_spaceterms_port(
    cx: &mut TestAppContext,
) {
    let access = ScriptedAccess::off();
    let (window, cx) = open_servers(&access, cx);

    click(ACCESS_SELECTOR, cx);

    assert_eq!(access.policies(), vec![policy("192.168.1.21:9231")]);
    assert!(access_on(&window, cx));
    assert!(reads("Listening on 192.168.1.21:9231.", cx));
}

#[gpui::test]
fn a_change_the_server_couldnt_save_leaves_access_off_and_says_why(cx: &mut TestAppContext) {
    let access = ScriptedAccess::off();
    *access.refusal.lock().expect("refusal") = Some(AccessFailure::Storage);
    let (window, cx) = open_servers(&access, cx);

    click(ACCESS_SELECTOR, cx);

    assert!(
        !access_on(&window, cx),
        "the row reports the server as it is"
    );
    assert!(reads("\"studio\" couldn't save the change. Try again.", cx));
}

#[gpui::test]
fn a_server_without_a_reachable_address_cant_be_turned_on_and_says_so(cx: &mut TestAppContext) {
    let access = ScriptedAccess::new(Listener::Off, Some(addresses(&[], None)));
    let (_, cx) = open_servers(&access, cx);

    click(ACCESS_SELECTOR, cx);

    assert!(access.policies().is_empty(), "nothing reaches the server");
    assert!(reads("No reachable network address on studio.", cx));
}

#[gpui::test]
fn addresses_the_server_couldnt_read_offer_no_guess_and_say_so(cx: &mut TestAppContext) {
    let access = ScriptedAccess::new(Listener::Off, None);
    let (_, cx) = open_servers(&access, cx);

    click(ACCESS_SELECTOR, cx);

    assert!(
        access.policies().is_empty(),
        "SpaceTerm never substitutes an address the server didn't report"
    );
    assert!(reads("Couldn't read studio's network addresses.", cx));
}

#[gpui::test]
fn a_listening_address_that_disappeared_asks_for_another_and_keeps_the_port(
    cx: &mut TestAppContext,
) {
    let access = ScriptedAccess::with_state(AccessState {
        server_name: SERVER.into(),
        policy: Some(address("192.168.1.20:9300")),
        configured: Some(address("192.168.1.20:9300")),
        listener: Listener::AddressGone,
        addresses: Some(addresses(
            &["192.168.1.21", "192.168.1.22"],
            Some("192.168.1.21"),
        )),
    });
    let (_, cx) = open_servers(&access, cx);
    assert!(reads(
        "192.168.1.20 is no longer on studio. Choose another.",
        cx
    ));

    click(ADDRESS_SELECTOR, cx);
    click("192.168.1.22", cx);

    assert_eq!(access.policies(), vec![policy("192.168.1.22:9300")]);
    assert!(reads("Listening on 192.168.1.22:9300.", cx));
}

/// An address chosen while access is off keeps access off and keeps the port chosen before, and
/// turning access on later listens there.
#[gpui::test]
fn an_address_chosen_while_off_keeps_its_port_for_turning_on(cx: &mut TestAppContext) {
    let access = ScriptedAccess::with_state(AccessState {
        server_name: SERVER.into(),
        policy: None,
        configured: Some(address("192.168.1.20:9300")),
        listener: Listener::Off,
        addresses: Some(addresses(
            &["192.168.1.20", "192.168.1.21"],
            Some("192.168.1.20"),
        )),
    });
    let (_, cx) = open_servers(&access, cx);

    click(ADDRESS_SELECTOR, cx);
    click("192.168.1.21", cx);

    assert_eq!(
        access.requests(),
        vec![(policy("192.168.1.21:9300"), false)],
        "choosing an address keeps access off and the port chosen before"
    );

    click(ACCESS_SELECTOR, cx);

    assert_eq!(
        access.requests().last(),
        Some(&(policy("192.168.1.21:9300"), true))
    );
    assert!(reads("Listening on 192.168.1.21:9300.", cx));
}

/// Access off with every interface kept, as `0.0.0.0:9300` or `[::]:9300`: turning it on again
/// without choosing an address listens on every interface again, at the same port.
fn turning_on_keeps_a_wildcard_listener(kept: &str, cx: &mut TestAppContext) {
    let access = ScriptedAccess::with_state(AccessState {
        server_name: SERVER.into(),
        policy: None,
        configured: Some(address(kept)),
        listener: Listener::Off,
        addresses: Some(addresses(
            &["192.168.1.20", "192.168.1.21"],
            Some("192.168.1.20"),
        )),
    });
    let (_, cx) = open_servers(&access, cx);

    click(ACCESS_SELECTOR, cx);

    assert_eq!(
        access.requests(),
        vec![(policy(kept), true)],
        "turning access on keeps the listener on every interface"
    );
}

#[gpui::test]
fn turning_on_keeps_an_ipv4_wildcard_listener(cx: &mut TestAppContext) {
    turning_on_keeps_a_wildcard_listener("0.0.0.0:9300", cx);
}

#[gpui::test]
fn turning_on_keeps_an_ipv6_wildcard_listener(cx: &mut TestAppContext) {
    turning_on_keeps_a_wildcard_listener("[::]:9300", cx);
}

#[gpui::test]
fn turning_off_asks_first_and_escape_changes_nothing(cx: &mut TestAppContext) {
    let access = ScriptedAccess::listening();
    let (window, cx) = open_servers(&access, cx);

    click(ACCESS_SELECTOR, cx);
    assert!(open(&modal_action(TURN_OFF_CONFIRM_SELECTOR), cx));
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();

    assert!(access.policies().is_empty());
    assert!(access_on(&window, cx));

    click(ACCESS_SELECTOR, cx);
    click(&modal_action(TURN_OFF_CONFIRM_SELECTOR), cx);

    assert_eq!(access.policies(), vec![None]);
    assert!(!access_on(&window, cx));
}

#[gpui::test]
fn a_server_spaceterm_has_never_read_shows_its_access_as_unknown(cx: &mut TestAppContext) {
    let access = ScriptedAccess::off();
    access.become_unreachable();
    let (window, cx) = open_servers(&access, cx);

    assert!(
        !open(ACCESS_SELECTOR, cx),
        "no switch claims the server is off when nothing says so"
    );
    assert!(!access_on(&window, cx));
    assert!(reads("Unknown", cx));
    assert!(reads("Can't reach the server.", cx));
    assert!(
        !reads(
            "SpaceTerm can't reach This Computer. Last known Network Access settings:",
            cx
        ),
        "nothing was read, so nothing is shown as last known"
    );
}

#[gpui::test]
fn navigation_keeps_network_access_live_until_a_read_actually_fails(cx: &mut TestAppContext) {
    let access = ScriptedAccess::listening();
    access.pair(3, "MacBook", None);
    let (window, cx) = open_servers(&access, cx);

    click("settings-navigation-settings-section-interface", cx);
    assert!(
        window.read_with(cx, |settings, _| settings.network_access_live()),
        "leaving Servers suspends observation without discarding the confirmed reading"
    );

    click(NAVIGATION_SELECTOR, cx);
    assert!(reads("Listening on 192.168.1.20:9231.", cx));
    assert!(reads("Devices: 1 paired", cx));
    assert!(!reads("to change its network access.", cx));

    click("settings-navigation-settings-section-interface", cx);
    access.become_unreachable();
    click(NAVIGATION_SELECTOR, cx);
    assert!(!window.read_with(cx, |settings, _| settings.network_access_live()));
    assert!(access_on(&window, cx));
    assert!(reads(
        "SpaceTerm can't reach This Computer. Last known Network Access settings:",
        cx
    ));
}

#[gpui::test]
fn a_server_spaceterm_loses_keeps_its_last_known_access_and_devices(cx: &mut TestAppContext) {
    let access = ScriptedAccess::listening();
    access.pair(3, "MacBook", None);
    let (window, cx) = open_servers(&access, cx);
    assert!(reads("Devices: 1 paired", cx));

    access.become_unreachable();
    reread(cx);

    assert!(
        access_on(&window, cx),
        "losing the connection changes nothing on the server, so access still reads as on"
    );
    assert!(open(ACCESS_SELECTOR, cx));
    assert!(reads("Devices: 1 paired", cx));
    assert!(open(&format!("{DEVICE_SELECTOR}-0"), cx));
    assert!(reads(
        "SpaceTerm can't reach This Computer. Last known Network Access settings:",
        cx
    ));
    assert!(
        !reads("Listening on", cx),
        "a server SpaceTerm can't reach isn't reported as listening now"
    );
}

#[gpui::test]
fn a_server_last_read_as_off_stays_off_and_says_when(cx: &mut TestAppContext) {
    let access = ScriptedAccess::off();
    let (window, cx) = open_servers(&access, cx);

    access.become_unreachable();
    reread(cx);

    assert!(open(ACCESS_SELECTOR, cx), "the last reading still shows");
    assert!(!access_on(&window, cx));
    assert!(reads(
        "SpaceTerm can't reach This Computer. Last known Network Access settings:",
        cx
    ));
}

#[gpui::test]
fn network_access_controls_change_nothing_while_the_server_is_out_of_reach(
    cx: &mut TestAppContext,
) {
    let access = ScriptedAccess::listening();
    access.pair(3, "MacBook", None);
    let (_, cx) = open_servers(&access, cx);
    access.become_unreachable();
    reread(cx);

    click(ACCESS_SELECTOR, cx);
    assert!(
        !open(&modal_action(TURN_OFF_CONFIRM_SELECTOR), cx),
        "Turn Off isn't offered"
    );
    click(INVITE_SELECTOR, cx);
    assert!(
        !open(crate::ui::direct_pairing::COPY_SELECTOR, cx),
        "no invitation opens"
    );
    click(&format!("{DEVICE_SELECTOR}-0-remove"), cx);
    assert!(
        !open(&modal_action(REMOVE_CONFIRM_SELECTOR), cx),
        "Remove isn't offered"
    );
    assert!(access.policies().is_empty());
}

#[gpui::test]
fn turning_off_after_the_server_went_out_of_reach_is_refused(cx: &mut TestAppContext) {
    let access = ScriptedAccess::listening();
    let (window, cx) = open_servers(&access, cx);

    click(ACCESS_SELECTOR, cx);
    access.become_unreachable();
    reread(cx);
    click(&modal_action(TURN_OFF_CONFIRM_SELECTOR), cx);

    assert!(
        access.policies().is_empty(),
        "nothing is sent to a server SpaceTerm can't reach"
    );
    assert!(access_on(&window, cx));
}

#[gpui::test]
fn a_reading_is_retained_for_the_next_launch(cx: &mut TestAppContext) {
    let access = ScriptedAccess::listening();
    access.pair(3, "MacBook", None);
    let (_, cx) = open_servers(&access, cx);

    let retained = retained_here(cx);

    let network = retained
        .network_access
        .expect("the access reading is retained");
    assert_eq!(network.policy, Some(address("192.168.1.20:9231")));
    assert_eq!(network.server_name, SERVER);
    let devices = retained
        .paired_devices
        .expect("the device list is retained");
    assert_eq!(
        devices
            .devices
            .iter()
            .map(|device| device.name.as_str())
            .collect::<Vec<_>>(),
        vec!["MacBook"]
    );
}

#[gpui::test]
fn losing_the_server_keeps_what_was_retained_about_it(cx: &mut TestAppContext) {
    let access = ScriptedAccess::listening();
    access.pair(3, "MacBook", None);
    let (_, cx) = open_servers(&access, cx);
    let before = retained_here(cx);

    access.become_unreachable();
    reread(cx);

    let after = retained_here(cx);
    assert_eq!(after.network_access, before.network_access);
    assert_eq!(after.paired_devices, before.paired_devices);
}

#[gpui::test]
fn a_device_list_the_server_couldnt_read_keeps_the_retained_one(cx: &mut TestAppContext) {
    let access = ScriptedAccess::listening();
    access.pair(3, "MacBook", None);
    let (_, cx) = open_servers(&access, cx);
    let before = retained_here(cx).paired_devices;

    access.unpair("MacBook");
    *access.devices_failure.lock().expect("devices failure") = Some(AccessFailure::Refused);
    reread(cx);

    assert_eq!(
        retained_here(cx).paired_devices,
        before,
        "a failed listing is no reading, so it neither empties nor re-dates the devices"
    );
}

#[gpui::test]
fn after_relaunch_a_server_out_of_reach_shows_its_retained_access(cx: &mut TestAppContext) {
    cx.update(|cx| retain(ServerKey::Local, retained_on(), cx));
    let access = ScriptedAccess::listening();
    access.become_unreachable();
    let (window, cx) = open_servers(&access, cx);

    assert!(access_on(&window, cx));
    assert!(open(ACCESS_SELECTOR, cx));
    assert!(reads("Devices: 1 paired", cx));
    assert!(open(&format!("{DEVICE_SELECTOR}-0"), cx));
    assert!(reads(
        "SpaceTerm can't reach This Computer. Last known Network Access settings:",
        cx
    ));
    assert!(reads("Last known address: 192.168.1.20:9231.", cx));
}

#[gpui::test]
fn retained_access_that_loads_after_settings_opens_replaces_unknown(cx: &mut TestAppContext) {
    let access = ScriptedAccess::listening();
    access.become_unreachable();
    let (window, cx) = open_servers(&access, cx);
    assert!(reads("Unknown", cx));

    cx.update(|_, cx| retain(ServerKey::Local, retained_on(), cx));
    reread(cx);

    assert!(access_on(&window, cx));
    assert!(reads(
        "SpaceTerm can't reach This Computer. Last known Network Access settings:",
        cx
    ));
}

#[gpui::test]
fn another_servers_retained_access_never_shows_for_this_one(cx: &mut TestAppContext) {
    let (direct, routes) = paired(&["penguin"]);
    let penguin = ServerKey::Direct(routes[0]);
    cx.update(|cx| retain(penguin.clone(), retained_on(), cx));
    let access = ScriptedAccess::listening();
    access.become_unreachable();
    let (window, cx) = open_servers_with(&access, Some(direct), cx);

    assert!(
        cx.update(|_, cx| server_selection::facts(&penguin, cx))
            .is_some(),
        "penguin's record is still there to leak"
    );
    assert!(!access_on(&window, cx));
    assert!(reads("Unknown", cx));
    assert!(!reads("Shown values are from", cx));
}

#[gpui::test]
fn a_retained_empty_device_list_reads_as_none(cx: &mut TestAppContext) {
    let mut facts = retained_on();
    if let Some(devices) = facts.paired_devices.as_mut() {
        devices.devices.clear();
    }
    cx.update(|cx| retain(ServerKey::Local, facts, cx));
    let access = ScriptedAccess::listening();
    access.become_unreachable();
    let (_, cx) = open_servers(&access, cx);

    assert!(reads("Devices: None", cx));
}

#[gpui::test]
fn devices_the_server_couldnt_list_read_as_unknown_rather_than_none(cx: &mut TestAppContext) {
    let access = ScriptedAccess::listening();
    *access.devices_failure.lock().expect("devices failure") = Some(AccessFailure::Refused);
    let (_, cx) = open_servers(&access, cx);

    assert!(reads("Devices: Unknown", cx));
    assert!(!reads("Devices: None", cx));
}

/// Whether any node publishes `text` as its name or value.
fn names(text: &str, cx: &mut VisualTestContext) -> bool {
    A11yTree::read(cx)
        .in_order()
        .iter()
        .any(|node| node["aria"]["label"] == text || node["aria"]["value"] == text)
}

/// Opens Servers on the paired server studio, disconnected, with what it last reported: access
/// on and two devices, one of them this computer's own pairing.
fn open_paired_studio(
    cx: &mut TestAppContext,
) -> (
    Entity<SettingsWindow>,
    DirectConnections,
    &mut VisualTestContext,
) {
    let (direct, routes) = paired(&["studio"]);
    let own = direct
        .route(&routes[0])
        .expect("studio is saved")
        .paired
        .pairing;
    let mut facts = retained_on();
    if let Some(known) = facts.paired_devices.as_mut() {
        known.devices.push(PairedDeviceFacts {
            pairing: own,
            name: "Studio Client".to_owned(),
            fingerprint: fingerprint(4),
            last_connected_at: None,
        });
    }
    cx.update(|cx| retain(ServerKey::Direct(routes[0]), facts, cx));
    let access = ScriptedAccess::off();
    let (window, cx) = open_servers_with(&access, Some(direct.clone()), cx);
    select_paired_server(cx);
    (window, direct, cx)
}

/// A paired server is administered on its own computer, so Servers shows none of its network
/// access or devices, even what it last reported, and offers no step that would change them.
#[gpui::test]
fn a_paired_server_presents_no_network_access_or_devices(cx: &mut TestAppContext) {
    let (window, direct, cx) = open_paired_studio(cx);

    assert!(!open(ACCESS_SELECTOR, cx), "no Network Access switch");
    assert!(!open(INVITE_SELECTOR, cx), "no Invite Device");
    assert!(
        !open(&format!("{DEVICE_SELECTOR}-0-remove"), cx),
        "no device to remove"
    );
    assert!(!names("MacBook", cx), "no devices listed");
    assert!(!access_on(&window, cx));
    assert!(direct.pending_unpairs().is_empty());
    assert_eq!(direct.routes().len(), 1, "studio stays saved");
}

#[gpui::test]
fn invite_device_waits_while_the_server_cant_listen(cx: &mut TestAppContext) {
    // Access is on, so Devices shows, but the server couldn't listen at the chosen address.
    let access = ScriptedAccess::with_state(AccessState {
        server_name: SERVER.into(),
        policy: Some(address("192.168.1.20:9231")),
        configured: Some(address("192.168.1.20:9231")),
        listener: Listener::Failed,
        addresses: Some(addresses(&["192.168.1.20"], Some("192.168.1.20"))),
    });
    let (_, cx) = open_servers(&access, cx);
    assert!(open(INVITE_SELECTOR, cx), "Devices offers Invite Device");

    click(INVITE_SELECTOR, cx);

    assert!(
        !open(crate::ui::direct_pairing::COPY_SELECTOR, cx),
        "no invitation opens on a server that isn't listening"
    );
}

#[gpui::test]
fn a_device_allowed_in_invite_device_joins_the_devices_list_at_once(cx: &mut TestAppContext) {
    let access = ScriptedAccess::listening();
    let (_, cx) = open_servers(&access, cx);
    assert!(reads("Devices: None", cx));

    click(INVITE_SELECTOR, cx);
    access.claim(3, "MacBook");
    // Invite Device looks for claims every second, sooner than the rows read the server.
    cx.executor()
        .advance_clock(std::time::Duration::from_secs(1));
    cx.run_until_parked();
    press_return(cx);
    // Done closes the confirmation, uncovering the rows.
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();

    assert!(reads("Devices: 1 paired", cx));
    assert!(open(&format!("{DEVICE_SELECTOR}-0"), cx));
    assert!(reads("Never connected", cx));
}

#[gpui::test]
fn a_device_removed_elsewhere_leaves_the_list_at_the_next_reading(cx: &mut TestAppContext) {
    let access = ScriptedAccess::listening();
    access.pair(3, "MacBook", Some(unix_now()));
    access.pair(4, "iPad", None);
    let (_, cx) = open_servers(&access, cx);
    assert!(reads("Devices: 2 paired", cx));

    access.unpair("iPad");
    reread(cx);

    assert!(reads("Devices: 1 paired", cx));
    assert!(!open(&format!("{DEVICE_SELECTOR}-1"), cx));
}

#[gpui::test]
fn cancelling_a_device_removal_keeps_the_device_and_records_nothing(cx: &mut TestAppContext) {
    cx.executor().allow_parking();
    let access = ScriptedAccess::listening();
    access.pair(3, "MacBook", None);
    let (direct, _) = crate::direct_connections::tests::offline("studio");
    let (_, cx) = open_servers_with(&access, Some(direct.clone()), cx);

    click(&format!("{DEVICE_SELECTOR}-0-remove"), cx);
    click(&modal_action(REMOVE_CANCEL_SELECTOR), cx);

    assert!(direct.pending_unpairs().is_empty());
    assert!(open(&format!("{DEVICE_SELECTOR}-0-remove"), cx));
    assert!(!open(&format!("{DEVICE_SELECTOR}-0-progress"), cx));
}

#[gpui::test]
fn a_removal_spaceterm_cant_record_keeps_the_device_and_says_why(cx: &mut TestAppContext) {
    cx.executor().allow_parking();
    let access = ScriptedAccess::listening();
    access.pair(3, "MacBook", None);
    let (direct, _) = crate::direct_connections::tests::offline("studio");
    let (_, cx) = open_servers_with(&access, Some(direct.clone()), cx);

    click(&format!("{DEVICE_SELECTOR}-0-remove"), cx);
    click(&modal_action(REMOVE_CONFIRM_SELECTOR), cx);

    assert!(
        direct.pending_unpairs().is_empty(),
        "no removal is recorded without the account connection that authorizes it"
    );
    assert!(
        open(&format!("{DEVICE_SELECTOR}-0-remove"), cx),
        "the device stays removable rather than waiting on a removal nobody records"
    );
    assert!(reads("SpaceTerm can't reach \"studio\". Try again.", cx));
}

#[gpui::test]
fn unpairing_this_device_while_offline_removes_it_at_once_and_says_revocation_is_unconfirmed(
    cx: &mut TestAppContext,
) {
    // The registry's worker tries the offline server on its own thread and wakes the window from
    // there.
    cx.executor().allow_parking();
    let access = ScriptedAccess::new(Listener::Off, None);
    let (direct, route) = crate::direct_connections::tests::offline("studio");
    let (_, cx) = open_servers_with(&access, Some(direct.clone()), cx);

    select_paired_server(cx);
    click(MENU_SELECTOR, cx);
    click("settings-server-unpair", cx);
    click(&modal_action(UNPAIR_SELF_CONFIRM_SELECTOR), cx);

    assert_eq!(
        direct.status(&route),
        None,
        "the route is gone from this computer"
    );
    assert!(
        !open(&format!("{LIST_SELECTOR}-1"), cx),
        "All Servers no longer offers the unpaired server"
    );
    // The offline server's first revocation attempt fails, which leaves the unpair unconfirmed
    // until the person retries it.
    settle(cx, |cx| {
        reads("Unpair from studio isn't confirmed. Choose Retry.", cx)
    });
}

#[gpui::test]
fn connect_on_a_disconnected_paired_server_reaches_only_that_route(cx: &mut TestAppContext) {
    // Connect starts the registry's worker, which tries the offline server on its own thread.
    cx.executor().allow_parking();
    let access = ScriptedAccess::new(Listener::Off, None);
    let (direct, route) = crate::direct_connections::tests::offline("studio");
    direct.disconnect(&route);
    let (_, cx) = open_servers_with(&access, Some(direct.clone()), cx);

    select_paired_server(cx);
    // Connect lives in the Server row's ⋯ menu.
    click(MENU_SELECTOR, cx);
    click(
        crate::ui::direct_servers::DirectServerAction::Connect.selector(),
        cx,
    );

    assert_ne!(
        direct.status(&route),
        Some(DirectRouteStatus::Disconnected),
        "Connect asks the direct registry to reach the selected route"
    );
}

#[gpui::test]
fn a_dismissed_unpair_keeps_trying_and_returns_with_resume_once_only_the_person_can_continue_it(
    cx: &mut TestAppContext,
) {
    // The registry's worker tries the offline server on its own thread and wakes the window from
    // there.
    cx.executor().allow_parking();
    let access = ScriptedAccess::new(Listener::Off, None);
    let (direct, _, credentials) = crate::direct_connections::tests::offline_with_lock("studio");
    let (_, cx) = open_servers_with(&access, Some(direct.clone()), cx);
    let state = |direct: &DirectConnections| {
        direct
            .pending_unpairs()
            .first()
            .map(|pending| pending.state)
    };

    select_paired_server(cx);
    click(MENU_SELECTOR, cx);
    click("settings-server-unpair", cx);
    click(&modal_action(UNPAIR_SELF_CONFIRM_SELECTOR), cx);
    settle(cx, |_| !direct.pending_unpairs().is_empty());
    let notice = unpair_notice(&direct);
    let dismiss = format!("{notice}-dismiss");
    let resume = format!("{notice}-resume");
    // The offline server never confirms, and SpaceTerm keeps trying quietly.
    settle(cx, |cx| {
        state(&direct) == Some(UnpairState::Unconfirmed) && open(&dismiss, cx)
    });
    click(&dismiss, cx);
    assert!(
        !open(&notice, cx),
        "Dismiss hides a request SpaceTerm can still finish"
    );
    assert_eq!(
        direct.pending_unpairs().len(),
        1,
        "hiding the notice cancels nothing behind it"
    );

    // The key the request was made with now needs the person's permission.
    credentials.set_locked(true);
    settle(cx, |_| {
        state(&direct) == Some(UnpairState::NeedsInteraction)
    });

    assert!(
        open(&resume, cx),
        "a dismissed request that only the person can continue shows again with Resume"
    );

    credentials.set_locked(false);
    click(&resume, cx);
    settle(cx, |_| {
        state(&direct) != Some(UnpairState::NeedsInteraction)
    });
    settle(cx, |cx| !open(&notice, cx));
}

#[gpui::test]
fn hiding_an_unpair_that_waits_for_the_person_hides_it_here_and_cancels_nothing(
    cx: &mut TestAppContext,
) {
    // The registry's worker tries the offline server on its own thread and wakes the window from
    // there.
    cx.executor().allow_parking();
    let access = ScriptedAccess::new(Listener::Off, None);
    let (direct, _, credentials) = crate::direct_connections::tests::offline_with_lock("studio");
    let (_, cx) = open_servers_with(&access, Some(direct.clone()), cx);
    let state = |direct: &DirectConnections| {
        direct
            .pending_unpairs()
            .first()
            .map(|pending| pending.state)
    };

    select_paired_server(cx);
    click(MENU_SELECTOR, cx);
    click("settings-server-unpair", cx);
    click(&modal_action(UNPAIR_SELF_CONFIRM_SELECTOR), cx);
    settle(cx, |_| state(&direct) == Some(UnpairState::Unconfirmed));
    let notice = unpair_notice(&direct);
    let dismiss = format!("{notice}-dismiss");
    let resume = format!("{notice}-resume");
    // The key the request was made with now needs the person's permission.
    credentials.set_locked(true);
    settle(cx, |cx| {
        state(&direct) == Some(UnpairState::NeedsInteraction) && open(&resume, cx)
    });

    click(&dismiss, cx);

    assert!(
        !open(&notice, cx),
        "Dismiss hides the notice in this window"
    );
    assert_eq!(
        state(&direct),
        Some(UnpairState::NeedsInteraction),
        "the request still waits for the person, uncancelled"
    );
}

#[gpui::test]
fn dismissing_the_unsaved_direct_record_notice_hides_it_without_claiming_a_save(
    cx: &mut TestAppContext,
) {
    cx.executor().allow_parking();
    let access = ScriptedAccess::new(Listener::Off, None);
    let (direct, route, storage) = crate::direct_connections::tests::offline_with_storage("studio");
    let (_, cx) = open_servers_with(&access, Some(direct.clone()), cx);
    let notice = format!("{NOTICE_SELECTOR}-direct-record");

    // This computer can't save the registry's change.
    storage.set_failing(true);
    direct.disconnect(&route);
    settle(cx, |cx| open(&notice, cx));
    assert!(reads(UNSAVED_CHANGES, cx));

    click(&format!("{notice}-dismiss"), cx);

    assert!(!open(&notice, cx));
    assert!(
        direct.persistence_error(),
        "the registry still reports the change it couldn't save"
    );
}
