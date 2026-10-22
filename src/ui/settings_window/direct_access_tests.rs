//! Direct network access and pairing in the Servers section, through a scripted server.
//!
//! The scripted server stands in for the network boundary: it answers each direct network
//! operation as the test sets and records what the Settings window asked of it.

use std::net::SocketAddr;
use std::rc::Rc;
use std::sync::{Arc, Mutex};

use gpui::{Entity, Modifiers, TestAppContext, VisualTestContext};
use spaceterm_ui::a11y_testing::A11yTree;
use zeroize::Zeroizing;

use super::super::servers::testing::connected;
use super::super::servers::{LocalServerControl, OfferedServer, ServerConnection};
use super::{ADDRESS_CANCEL_SELECTOR, ADDRESS_CONFIRM_SELECTOR, PAIR_SELECTOR, TURN_ON_SELECTOR};
use crate::appearance::Appearance;
use crate::direct_connections::{DirectConnections, DirectRouteStatus, UnpairState};
use crate::local_server::ServerError;
use crate::local_server::direct::{ClaimId, NetworkPolicy, PairingClaim};
use crate::platform::appearance::testing::RecordingAppearancePlatform;
use crate::platform::window_movement::RecordingOperatingSystemWindowDragPlatform;
use crate::settings::SettingsDocument;
use crate::settings::storage::testing::MemoryStorage;
use crate::ui::appearance_runtime;
use crate::ui::direct_access::{
    AccessFailure, AccessState, Claim, Device, DirectAccessControl, Listener, OpenInvitation,
    unix_now,
};
use crate::ui::direct_pairing::{APPROVE_SELECTOR, COPY_SELECTOR, DONE_SELECTOR};
use crate::ui::settings_window::SettingsWindow;
use crate::ui::settings_window::direct_routes::{
    PENDING_SELECTOR, UNPAIR_SELF_CONFIRM_SELECTOR, UNPAIR_SELF_SELECTOR,
};

/// The direct route's place in the server list, after the local server.
const DIRECT_ROUTE_ITEM: &str = "settings-server-list-1";

const NAVIGATION_SELECTOR: &str = "settings-navigation-settings-section-servers";
const SERVER: &str = "studio";
const INVITATION: &str = "spaceterm-invitation-for-test";
const INVITATION_ID: ClaimId = [7; 32];

/// A server's direct network operations as the test scripts them.
struct ScriptedAccess {
    state: Mutex<AccessState>,
    /// The refusal the next policy change returns, leaving the state unchanged.
    refusal: Mutex<Option<AccessFailure>>,
    policies: Mutex<Vec<Option<NetworkPolicy>>>,
    claims: Mutex<Vec<Claim>>,
    approved: Mutex<Vec<ClaimId>>,
    withdrawn: Mutex<Vec<ClaimId>>,
}

impl ScriptedAccess {
    fn new(listener: Listener) -> Arc<Self> {
        let policy = match listener {
            Listener::Listening(address) => Some(address),
            _ => None,
        };
        Arc::new(Self {
            state: Mutex::new(AccessState {
                server_name: SERVER.into(),
                policy,
                listener,
                server_fingerprint: "AAAA BBBB".into(),
            }),
            refusal: Mutex::default(),
            policies: Mutex::default(),
            claims: Mutex::default(),
            approved: Mutex::default(),
            withdrawn: Mutex::default(),
        })
    }

    fn listening() -> Arc<Self> {
        Self::new(Listener::Listening(address("192.168.1.20:9231")))
    }

    fn claim(&self, id: u8, name: &str) {
        let claim = PairingClaim {
            id: [id; 32],
            server: [1; 32],
            device_public_key: [id; 32],
            device_name: name.into(),
            expires_at_unix_seconds: unix_now() + 300,
        };
        self.claims.lock().expect("claims").push(Claim {
            id: claim.id,
            name: name.to_owned().into(),
            fingerprint: format!("FP{id}").into(),
            expires_at: claim.expires_at_unix_seconds,
            claim,
        });
    }
}

impl DirectAccessControl for ScriptedAccess {
    fn status(&self) -> Result<AccessState, AccessFailure> {
        Ok(self.state.lock().expect("state").clone())
    }

    fn set_policy(&self, policy: Option<NetworkPolicy>) -> Result<AccessState, AccessFailure> {
        self.policies.lock().expect("policies").push(policy.clone());
        if let Some(refusal) = *self.refusal.lock().expect("refusal") {
            return Err(refusal);
        }
        let mut state = self.state.lock().expect("state");
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

    fn withdraw(&self, invitation: ClaimId) -> Result<(), AccessFailure> {
        self.withdrawn.lock().expect("withdrawn").push(invitation);
        Ok(())
    }

    fn claims(&self) -> Result<Vec<Claim>, AccessFailure> {
        Ok(self.claims.lock().expect("claims").clone())
    }

    fn approve(&self, claim: &PairingClaim) -> Result<Device, AccessFailure> {
        self.approved.lock().expect("approved").push(claim.id);
        Ok(Device {
            pairing: [9; 32],
            name: claim.device_name.as_str().to_owned().into(),
            fingerprint: "FP".into(),
        })
    }

    fn reject(&self, _: ClaimId) -> Result<(), AccessFailure> {
        Ok(())
    }

    fn devices(&self) -> Result<Vec<Device>, AccessFailure> {
        Ok(Vec::new())
    }
}

/// The account-local server, whose direct network access the scripted operations answer.
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

/// Replaces the focused field's text with `text`.
fn retype(text: &str, cx: &mut VisualTestContext) {
    cx.simulate_keystrokes("end shift-home");
    cx.simulate_input(text);
    cx.run_until_parked();
}

/// Repaints until `done` holds, while the registry's worker runs on its own threads.
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

/// Whether any Label the window publishes contains `text`.
fn says(text: &str, cx: &mut VisualTestContext) -> bool {
    A11yTree::read(cx).with_role("Label").iter().any(|node| {
        node["aria"]["value"]
            .as_str()
            .is_some_and(|value| value.contains(text))
    })
}

#[gpui::test]
fn turn_on_keeps_an_unusable_address_in_the_dialog_and_saves_only_the_confirmed_one(
    cx: &mut TestAppContext,
) {
    let access = ScriptedAccess::new(Listener::Off);
    let (_, cx) = open_servers(&access, cx);

    click(TURN_ON_SELECTOR, cx);
    retype("0.0.0.0", cx);
    click(&modal_action(ADDRESS_CONFIRM_SELECTOR), cx);

    assert!(
        open(&modal_action(ADDRESS_CONFIRM_SELECTOR), cx),
        "an address naming every interface stays in the dialog for the person to correct"
    );
    assert!(
        access.policies.lock().expect("policies").is_empty(),
        "nothing reaches the server until the address is one it can listen at"
    );
    assert!(
        says("own IP addresses", cx),
        "the dialog says what to enter"
    );

    retype("192.168.1.20", cx);
    click(&modal_action(ADDRESS_CONFIRM_SELECTOR), cx);

    assert_eq!(
        *access.policies.lock().expect("policies"),
        vec![Some(
            NetworkPolicy::new(address("192.168.1.20:9231")).expect("a concrete address")
        )],
        "the server saves exactly the confirmed address at SpaceTerm's port"
    );
    assert!(!open(&modal_action(ADDRESS_CONFIRM_SELECTOR), cx));
}

#[gpui::test]
fn a_policy_the_server_couldnt_save_keeps_the_dialog_open_with_its_reason(cx: &mut TestAppContext) {
    let access = ScriptedAccess::new(Listener::Off);
    *access.refusal.lock().expect("refusal") = Some(AccessFailure::Storage);
    let (window, cx) = open_servers(&access, cx);

    click(TURN_ON_SELECTOR, cx);
    retype("192.168.1.20", cx);
    click(&modal_action(ADDRESS_CONFIRM_SELECTOR), cx);

    assert!(
        open(&modal_action(ADDRESS_CONFIRM_SELECTOR), cx),
        "the dialog stays until the server confirms the save"
    );
    assert!(says("couldn\u{2019}t save the change", cx));
    let listening = window.read_with(cx, |settings, _| settings.direct_access.listening());
    assert!(!listening, "the row keeps reporting the server as it is");

    click(&modal_action(ADDRESS_CANCEL_SELECTOR), cx);
    assert!(!open(&modal_action(ADDRESS_CONFIRM_SELECTOR), cx));
}

#[gpui::test]
fn pair_device_copies_the_invitation_only_when_the_person_asks(cx: &mut TestAppContext) {
    let access = ScriptedAccess::listening();
    let (_, cx) = open_servers(&access, cx);

    click(PAIR_SELECTOR, cx);

    assert!(open(COPY_SELECTOR, cx), "the invitation is ready to copy");
    assert_eq!(
        cx.read_from_clipboard().and_then(|item| item.text()),
        None,
        "showing an invitation never overwrites the clipboard"
    );

    click(COPY_SELECTOR, cx);

    assert_eq!(
        cx.read_from_clipboard().and_then(|item| item.text()),
        Some(INVITATION.to_owned())
    );
}

#[gpui::test]
fn approve_pairs_exactly_the_claim_the_person_saw(cx: &mut TestAppContext) {
    let access = ScriptedAccess::listening();
    access.claim(3, "MacBook");
    access.claim(4, "Unexpected");
    let (_, cx) = open_servers(&access, cx);

    click(PAIR_SELECTOR, cx);
    click(&format!("{APPROVE_SELECTOR}-0"), cx);

    assert_eq!(*access.approved.lock().expect("approved"), vec![[3; 32]]);
    assert!(
        says(
            "\u{201C}MacBook\u{201D} is paired with \u{201C}studio\u{201D}.",
            cx
        ),
        "the dialog names the paired device and server"
    );
}

#[gpui::test]
fn closing_pair_device_withdraws_its_invitation(cx: &mut TestAppContext) {
    let access = ScriptedAccess::listening();
    let (_, cx) = open_servers(&access, cx);

    click(PAIR_SELECTOR, cx);
    assert!(access.withdrawn.lock().expect("withdrawn").is_empty());

    click(&modal_action(DONE_SELECTOR), cx);

    assert_eq!(
        *access.withdrawn.lock().expect("withdrawn"),
        vec![INVITATION_ID],
        "no device can use an invitation after its dialog closes"
    );
    assert!(!open(COPY_SELECTOR, cx));
}

#[gpui::test]
fn unpairing_this_device_while_offline_removes_it_at_once_and_says_revocation_is_unconfirmed(
    cx: &mut TestAppContext,
) {
    // The registry's worker tries the offline server on its own thread and wakes the window from
    // there.
    cx.executor().allow_parking();
    let access = ScriptedAccess::new(Listener::Off);
    let (direct, route) = crate::direct_connections::tests::offline("studio");
    let (_, cx) = open_servers_with(&access, Some(direct.clone()), cx);

    click(DIRECT_ROUTE_ITEM, cx);
    click(UNPAIR_SELF_SELECTOR, cx);
    click(&modal_action(UNPAIR_SELF_CONFIRM_SELECTOR), cx);

    assert_eq!(
        direct.status(&route),
        None,
        "the route is gone from this computer"
    );
    assert!(
        !open(DIRECT_ROUTE_ITEM, cx),
        "the server list no longer offers the unpaired server"
    );
    assert!(
        says(
            "is removed here, but the server hasn\u{2019}t confirmed",
            cx
        ),
        "the notice says what is done and what isn\u{2019}t"
    );
}

#[gpui::test]
fn connect_on_a_disconnected_direct_route_reaches_only_that_route(cx: &mut TestAppContext) {
    // Connect starts the registry's worker, which tries the offline server on its own thread.
    cx.executor().allow_parking();
    let access = ScriptedAccess::new(Listener::Off);
    let (direct, route) = crate::direct_connections::tests::offline("studio");
    direct.disconnect(&route);
    let (_, cx) = open_servers_with(&access, Some(direct.clone()), cx);

    click(DIRECT_ROUTE_ITEM, cx);
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
fn a_dismissed_unpair_returns_with_resume_once_only_the_person_can_continue_it(
    cx: &mut TestAppContext,
) {
    // The registry's worker tries the offline server on its own thread and wakes the window from
    // there.
    cx.executor().allow_parking();
    let access = ScriptedAccess::new(Listener::Off);
    let (direct, _, credentials) = crate::direct_connections::tests::offline_with_lock("studio");
    let (_, cx) = open_servers_with(&access, Some(direct.clone()), cx);
    let notice = format!("{PENDING_SELECTOR}-0");
    let dismiss = format!("{PENDING_SELECTOR}-0-dismiss");
    let resume = format!("{PENDING_SELECTOR}-0-resume");
    let state = |direct: &DirectConnections| {
        direct
            .pending_unpairs()
            .first()
            .map(|pending| pending.state)
    };

    click(DIRECT_ROUTE_ITEM, cx);
    click(UNPAIR_SELF_SELECTOR, cx);
    click(&modal_action(UNPAIR_SELF_CONFIRM_SELECTOR), cx);
    // The offline server never confirms, and SpaceTerm keeps trying quietly.
    settle(cx, |cx| {
        state(&direct) == Some(UnpairState::Unconfirmed) && open(&dismiss, cx)
    });
    click(&dismiss, cx);
    assert!(
        !open(&notice, cx),
        "Dismiss hides a request SpaceTerm can still finish"
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
