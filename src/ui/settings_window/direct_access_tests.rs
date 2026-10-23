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

use gpui::{Entity, Modifiers, TestAppContext, VisualTestContext};
use spaceterm_ui::a11y_testing::A11yTree;
use zeroize::Zeroizing;

use super::super::direct_routes::{UNPAIR_SELF_CONFIRM_SELECTOR, UNSAVED_CHANGES};
use super::super::notices::NOTICE_SELECTOR;
use super::super::server_card::MENU_SELECTOR;
use super::super::server_list::LIST_SELECTOR;
use super::super::servers::testing::connected;
use super::super::servers::{LocalServerControl, OfferedServer, ServerConnection};
use super::{
    ACCESS_SELECTOR, ADDRESS_SELECTOR, DEVICE_SELECTOR, INVITE_SELECTOR, REMOVE_CANCEL_SELECTOR,
    REMOVE_CONFIRM_SELECTOR, TURN_OFF_CONFIRM_SELECTOR,
};
use crate::appearance::Appearance;
use crate::direct_connections::{DirectConnections, DirectRouteStatus, UnpairState};
use crate::local_server::ServerError;
use crate::local_server::direct::{ClaimId, NetworkAddresses, NetworkPolicy, PairingClaim};
use crate::platform::appearance::testing::RecordingAppearancePlatform;
use crate::platform::window_movement::RecordingOperatingSystemWindowDragPlatform;
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
    claims: Mutex<Vec<Claim>>,
    devices: Mutex<Vec<Device>>,
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
            listener,
            addresses,
        })
    }

    fn with_state(state: AccessState) -> Arc<Self> {
        Arc::new(Self {
            status: Mutex::new(Ok(state)),
            refusal: Mutex::default(),
            policies: Mutex::default(),
            claims: Mutex::default(),
            devices: Mutex::default(),
        })
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

    fn set_policy(&self, policy: Option<NetworkPolicy>) -> Result<AccessState, AccessFailure> {
        self.policies.lock().expect("policies").push(policy.clone());
        if let Some(refusal) = *self.refusal.lock().expect("refusal") {
            return Err(refusal);
        }
        let mut status = self.status.lock().expect("status");
        let state = status.as_mut().map_err(|failure| *failure)?;
        state.policy = policy.as_ref().map(|policy| policy.bind);
        state.listener = policy.map_or(Listener::Off, |policy| Listener::Listening(policy.bind));
        Ok(state.clone())
    }

    fn invite(&self) -> Result<OpenInvitation, AccessFailure> {
        Ok(OpenInvitation {
            id: INVITATION_ID,
            text: Zeroizing::new(INVITATION.to_owned()),
            expires_at: unix_now() + 300,
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

    fn refresh(&self) -> Result<ServerConnection, ServerError> {
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
    Some(NetworkPolicy::new(address(text)).expect("a concrete address"))
}

fn device(id: u8, name: &str, last_connected_at: Option<u64>) -> Device {
    Device {
        pairing: [id; 32],
        name: name.to_owned().into(),
        fingerprint: format!("FP{id}").into(),
        last_connected_at,
    }
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
    assert!(reads("couldn\u{2019}t save the change", cx));
}

#[gpui::test]
fn a_server_without_a_reachable_address_cant_be_turned_on_and_says_so(cx: &mut TestAppContext) {
    let access = ScriptedAccess::new(Listener::Off, Some(addresses(&[], None)));
    let (_, cx) = open_servers(&access, cx);

    click(ACCESS_SELECTOR, cx);

    assert!(access.policies().is_empty(), "nothing reaches the server");
    assert!(reads(
        "studio has no network address other devices can reach.",
        cx
    ));
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
    assert!(reads(
        "studio couldn\u{2019}t read its network addresses.",
        cx
    ));
}

#[gpui::test]
fn a_listening_address_that_disappeared_asks_for_another_and_keeps_the_port(
    cx: &mut TestAppContext,
) {
    let access = ScriptedAccess::with_state(AccessState {
        server_name: SERVER.into(),
        policy: Some(address("192.168.1.20:9300")),
        listener: Listener::AddressGone,
        addresses: Some(addresses(
            &["192.168.1.21", "192.168.1.22"],
            Some("192.168.1.21"),
        )),
    });
    let (_, cx) = open_servers(&access, cx);
    assert!(reads(
        "192.168.1.20 is no longer on studio. Choose another address.",
        cx
    ));

    click(ADDRESS_SELECTOR, cx);
    click("192.168.1.22", cx);

    assert_eq!(access.policies(), vec![policy("192.168.1.22:9300")]);
    assert!(reads("Listening on 192.168.1.22:9300.", cx));
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
fn a_server_spaceterm_cant_reach_dims_network_access_and_says_what_to_do(cx: &mut TestAppContext) {
    let access = ScriptedAccess::off();
    *access.status.lock().expect("status") = Err(AccessFailure::Unreachable);
    let (_, cx) = open_servers(&access, cx);

    click(ACCESS_SELECTOR, cx);

    assert!(access.policies().is_empty());
    assert!(reads("to change its network access.", cx));
}

#[gpui::test]
fn a_paired_server_offers_no_network_access(cx: &mut TestAppContext) {
    cx.executor().allow_parking();
    let access = ScriptedAccess::listening();
    let (direct, _) = crate::direct_connections::tests::offline("studio");
    let (_, cx) = open_servers_with(&access, Some(direct), cx);
    assert!(
        open(ACCESS_SELECTOR, cx),
        "this computer's server offers it"
    );

    select_paired_server(cx);

    assert!(
        !open(ACCESS_SELECTOR, cx),
        "a paired connection carries no authority over its server's network"
    );
    assert!(!open(INVITE_SELECTOR, cx));
}

#[gpui::test]
fn invite_device_waits_until_the_server_listens(cx: &mut TestAppContext) {
    let access = ScriptedAccess::off();
    let (_, cx) = open_servers(&access, cx);

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
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
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
    assert!(reads(
        "can\u{2019}t reach the server on \u{201C}studio\u{201D}",
        cx
    ));
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
    settle(cx, |cx| {
        reads("Unpair from studio isn\u{2019}t confirmed yet.", cx)
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
    assert!(
        !open(&dismiss, cx),
        "a request that needs the person can't be dismissed out of reach"
    );

    credentials.set_locked(false);
    click(&resume, cx);
    settle(cx, |_| {
        state(&direct) != Some(UnpairState::NeedsInteraction)
    });
    settle(cx, |cx| !open(&notice, cx));
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
