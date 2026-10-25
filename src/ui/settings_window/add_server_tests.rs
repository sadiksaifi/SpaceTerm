//! The Add Server sheet, driven through a registry of SSH connections whose backend counts every
//! remote call and a real direct connections registry whose network is unavailable.

use std::rc::Rc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use gpui::{Entity, TestAppContext, VisualTestContext};
use spaceterm_ui::a11y_testing::{A11yTree, node_id};
use zeroize::Zeroizing;

use super::super::servers::testing::{ScriptedServer, connected};
use super::super::servers::{LocalServerControl, OfferedServer};
use super::*;
use crate::appearance::Appearance;
use crate::platform::appearance::testing::RecordingAppearancePlatform;
use crate::platform::window_movement::RecordingOperatingSystemWindowDragPlatform;
use crate::settings::SettingsDocument;
use crate::settings::storage::testing::MemoryStorage;
use crate::ssh::cancellation::SshCancellationToken;
use crate::ssh::server_connection::{
    ConnectMode, ConnectProgress, ConnectedSshServer, RetainedSshConnection, ServerAvailability,
    SshConnectBackend, SshConnectFailure, SshConnectionIntentStore, SshIntentStoreError,
    SshServerContinuity, SshServerFacts, SshServerLease, SshServerObservation,
};
use crate::ui::appearance_runtime;

/// The SSH computers the test reaches, counting every connection that would leave this computer.
/// A host named `offline` never answers.
#[derive(Default)]
struct Computers {
    connects: AtomicUsize,
}

const OFFLINE: &str = "offline";

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
        if destination.as_str() == OFFLINE {
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
        Ok(SshServerObservation {
            continuity: None,
            availability: ServerAvailability::Running,
            facts: facts(),
        })
    }
}

/// Saved connections kept in memory. While `held`, loading waits, as a slow disk does.
#[derive(Default)]
struct SavedConnections {
    records: Mutex<Vec<RetainedSshConnection>>,
    held: Mutex<bool>,
    released: Condvar,
}

impl SavedConnections {
    /// A record of `destination`, saved before this launch, that loads only once released.
    fn holding(destination: &str) -> Arc<Self> {
        Arc::new(Self {
            records: Mutex::new(vec![RetainedSshConnection {
                destination: SshDestination::new(destination.to_owned()).unwrap(),
                continuity: None,
                active: false,
            }]),
            held: Mutex::new(true),
            released: Condvar::new(),
        })
    }

    fn release(&self) {
        *self.held.lock().unwrap() = false;
        self.released.notify_all();
    }
}

impl SshConnectionIntentStore for SavedConnections {
    fn load(&self) -> Result<Vec<RetainedSshConnection>, SshIntentStoreError> {
        let held = self.held.lock().unwrap();
        drop(self.released.wait_while(held, |held| *held).unwrap());
        Ok(self.records.lock().unwrap().clone())
    }
    fn retain(&self, record: RetainedSshConnection) -> Result<(), SshIntentStoreError> {
        let mut records = self.records.lock().unwrap();
        records.retain(|other| other.destination != record.destination);
        records.push(record);
        Ok(())
    }
    fn remove(&self, destination: &SshDestination) -> Result<(), SshIntentStoreError> {
        self.records
            .lock()
            .unwrap()
            .retain(|record| record.destination != *destination);
        Ok(())
    }
}

struct Sheet<'a> {
    window: Entity<SettingsWindow>,
    computers: Arc<Computers>,
    connections: SshServerConnections,
    cx: &'a mut VisualTestContext,
}

/// Opens Settings on Servers with the local server, an SSH registry and a direct registry.
fn open_settings(cx: &mut TestAppContext) -> Sheet<'_> {
    let (direct, _) = crate::direct_connections::tests::offline("studio");
    let sheet = open_settings_with(Arc::new(SavedConnections::default()), direct, cx);
    let deadline = Instant::now() + Duration::from_secs(5);
    while !sheet.connections.retained_inventory_loaded() {
        assert!(Instant::now() < deadline, "the saved connections load");
        std::thread::sleep(Duration::from_millis(2));
    }
    sheet
}

/// Opens Settings as [`open_settings`] does, with SSH connections saved in `saved` and the direct
/// registry `direct`, without waiting for the SSH connections to load.
fn open_settings_with(
    saved: Arc<SavedConnections>,
    direct: DirectConnections,
    cx: &mut TestAppContext,
) -> Sheet<'_> {
    // The registries' workers run on their own threads.
    cx.executor().allow_parking();
    let computers = Arc::new(Computers::default());
    let connections = SshServerConnections::new(computers.clone(), saved);
    connections.resume_retained();
    let settings =
        crate::settings::Settings::load(MemoryStorage::with_document(&SettingsDocument::default()));
    let platform = RecordingAppearancePlatform::default();
    platform.set_system_appearance(Some(Appearance::Dark));
    cx.update(|cx| {
        appearance_runtime::install(settings, Rc::new(platform), cx)
            .expect("appearance runtime should install");
        crate::ui::init(cx).expect("UI initialization should succeed");
        crate::ui::ssh_servers::install(connections.clone(), None, cx);
        crate::ui::direct_servers::install(direct, cx);
    });
    let local = Some(OfferedServer {
        connection: Ok(ScriptedServer::new(Ok(connected(1))) as Arc<dyn LocalServerControl>),
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
    Sheet {
        window,
        computers,
        connections,
        cx,
    }
}

impl Sheet<'_> {
    fn begin(&mut self, request: AddServerRequest) {
        let settings = self.window.clone();
        self.cx.update(|window, cx| {
            settings.update(cx, |settings, cx| {
                settings.begin_add_server(request, window, cx);
            });
        });
        self.cx.run_until_parked();
    }

    /// Shows `join` in the sheet as if the sheet had started it with an invitation from
    /// `server`, then follows it.
    fn adopt(&mut self, join: crate::direct_connections::JoinId, server: &str) {
        let sheet = self
            .cx
            .update(|window, cx| open_sheet(window, cx))
            .expect("the sheet is open");
        let server = server.to_owned();
        self.cx.update(|window, cx| {
            sheet.update(cx, |sheet, cx| {
                sheet.stage = Stage::Joining {
                    join: Some(join),
                    server,
                };
                sheet.observe(window, cx);
            });
        });
        self.cx.run_until_parked();
    }

    /// Activates the sheet's action whose selector is `selector`.
    fn choose(&mut self, selector: &str) {
        let selector: &'static str = Box::leak(format!("modal-action-{selector}").into_boxed_str());
        let position = self
            .cx
            .debug_bounds(selector)
            .unwrap_or_else(|| panic!("{selector} was not rendered"))
            .center();
        self.cx
            .simulate_mouse_move(position, None, gpui::Modifiers::none());
        self.cx.simulate_click(position, gpui::Modifiers::none());
        self.cx.run_until_parked();
    }

    fn selected(&mut self) -> Option<ServerChoice> {
        self.window
            .read_with(self.cx, |settings, _| settings.selected_server())
    }

    /// The sheet's current presentation and whether its host field has keyboard focus.
    fn presented(&mut self) -> (Option<spaceterm_ui::ModalPresentationId>, bool) {
        self.cx.update(|window, cx| {
            let Some(sheet) = open_sheet(window, cx) else {
                return (None, false);
            };
            let sheet = sheet.read(cx);
            (
                sheet
                    .presentation
                    .as_ref()
                    .map(DialogCompletion::presentation_id),
                sheet.host.read(cx).focus_handle().is_focused(window),
            )
        })
    }

    fn open(&mut self) -> bool {
        self.cx
            .update(|window, cx| spaceterm_ui::window_modal_is_open(window, cx))
    }

    /// The sheet's visible title, as assistive technology reads its dialog.
    fn title(&mut self) -> String {
        let tree = A11yTree::read(self.cx);
        let dialogs = tree.with_role("Dialog");
        let dialog = dialogs.first().expect("the sheet is presented");
        dialog["aria"]["label"]
            .as_str()
            .unwrap_or_default()
            .to_owned()
    }

    /// The sheet's node named `label`, as assistive technology reads it.
    fn node(&mut self, label: &str) -> serde_json::Value {
        let tree = A11yTree::read(self.cx);
        let dialog = *tree
            .with_role("Dialog")
            .first()
            .expect("the sheet is presented");
        tree.descendants(dialog)
            .into_iter()
            .find(|node| node["aria"]["label"] == label)
            .unwrap_or_else(|| panic!("the sheet offers {label}"))
            .clone()
    }

    /// Whether the sheet's button named `label` is unavailable.
    fn disabled(&mut self, label: &str) -> bool {
        self.node(label)["aria"]["disabled"] == true
    }

    /// Whether any Label the window publishes says `text`.
    fn says(&mut self, text: &str) -> bool {
        A11yTree::read(self.cx)
            .with_role("Label")
            .iter()
            .any(|node| {
                node["aria"]["value"]
                    .as_str()
                    .is_some_and(|value| value.contains(text))
            })
    }

    /// Repaints until `done` holds, while the registries' workers run on their own threads.
    fn settle(&mut self, mut done: impl FnMut(&mut Self) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            self.cx.run_until_parked();
            if done(self) {
                return;
            }
            assert!(Instant::now() < deadline, "the registry settles");
            std::thread::sleep(Duration::from_millis(2));
        }
    }
}

/// A real invitation link for a server named `server`, as Invite Device creates one.
fn invitation(server: &str) -> Zeroizing<String> {
    use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
    let expires = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
        + 60;
    let pin = crate::local_server::direct::ServerPin {
        build: "spaceterm-development".into(),
        server: [1; 32],
        public_key: [3; 32],
        certificate: [4; 32],
    };
    let payload = Zeroizing::new(
        serde_json::to_vec(&serde_json::json!({
            "schema": 1, "id": ([7_u8; 32]), "endpoint": "wss://127.0.0.1:9231/spaceterm",
            "pin": pin, "server_name": server, "token": ([8_u8; 32]), "expires": expires,
        }))
        .unwrap(),
    );
    Zeroizing::new(format!(
        "spaceterm://pair/{}",
        URL_SAFE_NO_PAD.encode(&*payload)
    ))
}

#[gpui::test]
fn connect_stays_unavailable_until_a_host_is_typed(cx: &mut TestAppContext) {
    let mut sheet = open_settings(cx);
    sheet.begin(AddServerRequest::Ssh(None));

    assert!(sheet.disabled("Connect"), "an empty host connects nowhere");
    sheet.cx.simulate_input("penguin");
    sheet.cx.run_until_parked();
    assert!(!sheet.disabled("Connect"), "a typed host can connect");
}

#[gpui::test]
fn return_connects_to_the_typed_host_then_selects_it_and_closes_the_sheet(cx: &mut TestAppContext) {
    let mut sheet = open_settings(cx);
    sheet.begin(AddServerRequest::Ssh(None));

    sheet.cx.simulate_input("me@build.local");
    sheet.cx.run_until_parked();
    sheet.cx.simulate_keystrokes("enter");
    let window = sheet.window.clone();
    sheet.settle(|sheet| {
        !sheet.open()
            && window.read_with(sheet.cx, |settings, _| settings.selected_server())
                == Some(ServerChoice::Ssh(
                    SshDestination::new("me@build.local".to_owned()).unwrap(),
                ))
    });

    assert_eq!(sheet.computers.connects.load(Ordering::SeqCst), 1);
}

#[gpui::test]
fn escape_closes_a_filled_in_sheet_without_connecting(cx: &mut TestAppContext) {
    let mut sheet = open_settings(cx);
    sheet.begin(AddServerRequest::Ssh(Some("penguin".into())));
    assert!(sheet.open());

    sheet.cx.simulate_keystrokes("escape");
    sheet.cx.run_until_parked();

    assert!(!sheet.open(), "Escape cancels the sheet");
    assert_eq!(
        sheet.computers.connects.load(Ordering::SeqCst),
        0,
        "a filled-in host connects only after Connect"
    );
}

#[gpui::test]
fn a_delivered_invitation_names_its_server_and_address_and_waits_for_join(cx: &mut TestAppContext) {
    let mut sheet = open_settings(cx);
    sheet.begin(AddServerRequest::Invitation(Some(invitation("studio"))));

    assert!(
        sheet.says("studio"),
        "the sheet names the invitation's server"
    );
    assert!(sheet.says("127.0.0.1:9231"), "and the address it reaches");
    assert!(!sheet.disabled("Join"), "the person can join it");
    assert_eq!(
        sheet.title(),
        "Add Server",
        "a delivered link joins nothing until the person chooses Join"
    );
}

#[gpui::test]
fn a_link_that_isnt_an_invitation_says_so_and_cant_join(cx: &mut TestAppContext) {
    let mut sheet = open_settings(cx);
    sheet.begin(AddServerRequest::Invitation(Some(Zeroizing::new(
        "spaceterm://pair/not-an-invitation".to_owned(),
    ))));

    assert!(sheet.says("Invalid SpaceTerm invitation link."));
    assert!(sheet.disabled("Join"));
}

#[gpui::test]
fn cancelling_after_a_new_host_fails_to_connect_adds_no_server(cx: &mut TestAppContext) {
    let mut sheet = open_settings(cx);
    sheet.begin(AddServerRequest::Ssh(None));

    sheet.cx.simulate_input(OFFLINE);
    sheet.cx.run_until_parked();
    sheet.cx.simulate_keystrokes("enter");
    sheet.settle(|sheet| sheet.title().starts_with("Couldn\u{2019}t Connect"));
    sheet.cx.simulate_keystrokes("escape");
    sheet.cx.run_until_parked();

    assert!(!sheet.open(), "Escape cancels the sheet");
    assert!(
        sheet.connections.connections().is_empty(),
        "a host the person never added stays out of the server list"
    );
}

#[gpui::test]
fn cancelling_keeps_a_saved_host_whose_record_has_not_loaded_yet(cx: &mut TestAppContext) {
    let saved = SavedConnections::holding(OFFLINE);
    let (direct, _) = crate::direct_connections::tests::offline("studio");
    let mut sheet = open_settings_with(saved.clone(), direct, cx);
    assert!(
        !sheet.connections.retained_inventory_loaded(),
        "the saved connections are still loading"
    );
    sheet.begin(AddServerRequest::Ssh(None));

    sheet.cx.simulate_input(OFFLINE);
    sheet.cx.run_until_parked();
    sheet.cx.simulate_keystrokes("enter");
    sheet.settle(|sheet| sheet.title().starts_with("Couldn\u{2019}t Connect"));
    sheet.cx.simulate_keystrokes("escape");
    sheet.cx.run_until_parked();
    let kept = sheet
        .connections
        .connections()
        .iter()
        .any(|(destination, _)| destination.as_str() == OFFLINE);
    saved.release();

    assert!(!sheet.open(), "Escape cancels the sheet");
    assert!(
        kept,
        "a host the person saved before stays, although its record hadn't loaded when they typed it"
    );
}

#[gpui::test]
fn a_join_whose_save_is_unconfirmed_still_selects_its_server_and_closes_the_sheet(
    cx: &mut TestAppContext,
) {
    let (direct, join, route) = crate::direct_connections::tests::uncertain_join("studio");
    let mut sheet = open_settings_with(Arc::new(SavedConnections::default()), direct, cx);
    sheet.begin(AddServerRequest::Invitation(None));

    sheet.adopt(join, "studio");

    sheet.settle(|sheet| !sheet.open() && sheet.selected() == Some(ServerChoice::Direct(route)));
}

/// A join the network interrupted keeps its request and asks again only when the person chooses
/// Try Again, without claiming why the server was out of reach.
#[gpui::test]
fn an_interrupted_join_waits_for_try_again_with_the_same_request(cx: &mut TestAppContext) {
    let (direct, join) = crate::direct_connections::tests::retryable_join(
        "studio",
        crate::direct_connections::EnrollmentStage::Claim,
    );
    let mut sheet = open_settings_with(Arc::new(SavedConnections::default()), direct.clone(), cx);
    sheet.begin(AddServerRequest::Invitation(None));

    sheet.adopt(join, "studio");
    sheet.settle(|sheet| {
        sheet.says("SpaceTerm couldn't reach the server. Check your network, then try again.")
    });

    assert_eq!(sheet.title(), "Pairing Paused");
    assert!(
        !sheet.says("denied") && !sheet.says("permission"),
        "a network failure names no cause SpaceTerm didn't observe"
    );
    assert!(
        direct.pending_unpairs().is_empty(),
        "an interrupted join withdraws nothing"
    );

    sheet.choose(SheetAction::TryAgain.selector());

    sheet.settle(|sheet| !sheet.open() && sheet.selected() == Some(ServerChoice::Direct(join)));
    assert!(direct.pending_unpairs().is_empty());
}

/// The fixture's storage can't confirm any further save after an unknown publication, so the
/// person's withdrawal is requested but not retained, and the sheet says exactly that.
#[gpui::test]
fn withdrawing_a_join_whose_save_is_unknown_says_the_withdrawal_isnt_saved(
    cx: &mut TestAppContext,
) {
    let (direct, join) = crate::direct_connections::tests::unconfirmed_join("studio");
    let mut sheet = open_settings_with(Arc::new(SavedConnections::default()), direct.clone(), cx);
    sheet.begin(AddServerRequest::Invitation(None));

    sheet.adopt(join, "studio");
    sheet.settle(|sheet| {
        sheet.says("studio approved this device, but SpaceTerm couldn't save the connection.")
    });

    assert!(
        direct.routes().is_empty(),
        "SpaceTerm uses no connection it may not have saved"
    );
    assert!(
        direct.pending_unpairs().is_empty(),
        "nothing is withdrawn until the person asks"
    );

    sheet.choose(SheetAction::Withdraw.selector());
    sheet.settle(|sheet| sheet.says("Keep SpaceTerm open until the cancellation is saved."));

    assert!(
        sheet.open(),
        "the sheet stays to say the withdrawal isn't saved"
    );
    assert!(
        direct.routes().is_empty(),
        "withdrawing presents no connection"
    );
}

/// A stop that couldn't be saved keeps saying so through the registry's unsaved changes, and
/// changes once the registry saves again, so the sheet follows recovery instead of outliving it.
#[gpui::test]
fn an_unsaved_stop_says_so_until_the_registry_saves_again(cx: &mut TestAppContext) {
    let (direct, route, storage) = crate::direct_connections::tests::offline_with_storage("studio");
    let mut sheet = open_settings_with(Arc::new(SavedConnections::default()), direct.clone(), cx);
    sheet.begin(AddServerRequest::Invitation(None));
    // This computer can't save the registry, as when the stop itself failed to save.
    storage.set_failing(true);
    direct.disconnect(&route);
    assert!(direct.persistence_error(), "the registry couldn't save");
    let body = sheet
        .cx
        .update(|window, cx| open_sheet(window, cx))
        .expect("the sheet is open");
    sheet.cx.update(|window, cx| {
        body.update(cx, |sheet, cx| {
            sheet.stage = Stage::StopUnsaved {
                server: "studio".to_owned(),
            };
            sheet.show(window, cx);
        });
    });
    sheet.cx.run_until_parked();
    assert!(sheet.says("Keep SpaceTerm open until the cancellation is saved."));

    // Changes the registry announces but can't save leave the stop unsaved.
    direct.connect(&route);
    direct.disconnect(&route);
    sheet.cx.run_until_parked();
    assert!(direct.persistence_error());
    assert!(sheet.says("Keep SpaceTerm open until the cancellation is saved."));

    // A change the registry saves and announces, as a quiet retry's save would.
    storage.set_failing(false);
    direct.disconnect(&route);
    assert!(!direct.persistence_error(), "the registry saved its change");

    sheet.settle(|sheet| sheet.says("SpaceTerm is cancelling the pairing with studio."));
}

/// Typing a host's first character makes Connect available on the same sheet: the presentation,
/// the field's focus and Connect's accessibility node all stay.
#[gpui::test]
fn typing_the_first_host_character_makes_connect_available_in_place(cx: &mut TestAppContext) {
    let mut sheet = open_settings(cx);
    sheet.begin(AddServerRequest::Ssh(None));
    let (before, _) = sheet.presented();
    let connect = sheet.node("Connect");

    sheet.cx.simulate_input("p");
    sheet.cx.run_until_parked();

    let (after, focused) = sheet.presented();
    let available = sheet.node("Connect");
    assert!(before.is_some());
    assert_eq!(after, before, "the sheet stays the same presentation");
    assert!(focused, "the host field keeps keyboard focus");
    assert_eq!(connect["aria"]["disabled"], true);
    assert_ne!(available["aria"]["disabled"], true);
    assert_eq!(node_id(&connect), node_id(&available));
}

/// Connect can't be chosen while the host is empty, by pointer or by Return.
#[gpui::test]
fn an_unavailable_connect_connects_nothing(cx: &mut TestAppContext) {
    let mut sheet = open_settings(cx);
    sheet.begin(AddServerRequest::Ssh(None));

    sheet.choose(SheetAction::Connect.selector());
    sheet.cx.simulate_keystrokes("enter");
    sheet.cx.run_until_parked();

    assert!(sheet.open());
    assert_eq!(sheet.title(), "Add Server");
    assert_eq!(sheet.computers.connects.load(Ordering::SeqCst), 0);
}

/// Joining shows the code the authenticated exchange produced, which assistive technology reads
/// digit by digit, while the server's owner decides. Escape stops that exact enrollment.
#[gpui::test]
fn joining_shows_the_comparison_code_until_escape_stops_it(cx: &mut TestAppContext) {
    let (direct, link, code) = crate::direct_connections::tests::waiting_invitation("studio");
    assert_eq!(
        code.to_string(),
        "000007",
        "the fixture's exchange produces this code"
    );
    let mut sheet = open_settings_with(Arc::new(SavedConnections::default()), direct.clone(), cx);
    sheet.begin(AddServerRequest::Invitation(Some(link)));

    sheet.cx.simulate_keystrokes("enter");
    sheet.settle(|sheet| {
        A11yTree::read(sheet.cx)
            .find("Code")
            .is_some_and(|node| node["aria"]["value"] == "0 0 0 0 0 7")
    });

    assert_eq!(sheet.title(), "Join studio");
    assert!(sheet.says("Check that studio shows this code"));
    assert!(sheet.says("Waiting for approval…"));

    sheet.cx.simulate_keystrokes("escape");
    sheet.settle(|sheet| !sheet.open());
    assert!(direct.routes().is_empty(), "a stopped join adds no server");
}

/// A link delivered while a join waits doesn't replace it. The sheet says it arrived, and Add
/// Server shows it once the current join ends.
#[gpui::test]
fn a_link_delivered_during_a_join_waits_for_it_to_end(cx: &mut TestAppContext) {
    let (direct, link, _) = crate::direct_connections::tests::waiting_invitation("studio");
    let (_, next, _) = crate::direct_connections::tests::waiting_invitation("annex");
    let mut sheet = open_settings_with(Arc::new(SavedConnections::default()), direct, cx);
    sheet.begin(AddServerRequest::Invitation(Some(link)));
    sheet.cx.simulate_keystrokes("enter");
    sheet.settle(|sheet| A11yTree::read(sheet.cx).find("Code").is_some());

    sheet.begin(AddServerRequest::Invitation(Some(next)));

    assert_eq!(
        sheet.title(),
        "Join studio",
        "the join underway keeps its place"
    );
    assert!(sheet.says("Another invitation is next."));

    sheet.cx.simulate_keystrokes("escape");
    sheet.settle(|sheet| sheet.open() && sheet.title() == "Add Server" && sheet.says("annex"));
    assert!(!sheet.disabled("Join"), "the delivered link waits for Join");
}
