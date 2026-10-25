//! The Add Server sheet, driven through a scripted SSH setup backend that records every setup it
//! is asked for, and a real direct connections registry whose network is unavailable.

use std::collections::VecDeque;
use std::net::SocketAddr;
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use gpui::{Entity, TestAppContext, VisualTestContext};
use spaceterm_ui::a11y_testing::{A11yTree, node_id};
use zeroize::Zeroizing;

use super::super::servers::testing::{ScriptedServer, connected};
use super::super::servers::{LocalServerControl, OfferedServer};
use super::*;
use crate::appearance::Appearance;
use crate::platform::app_paths::AppPaths;
use crate::platform::appearance::testing::RecordingAppearancePlatform;
use crate::platform::window_movement::RecordingOperatingSystemWindowDragPlatform;
use crate::settings::SettingsDocument;
use crate::settings::storage::testing::MemoryStorage;
use crate::ssh::askpass::{AskPassAttempt, AskPassAttemptFactory, AskPassUnavailable};
use crate::ssh::provisioning::testing::{
    installation_approval, network_approval, startup_approval,
};
use crate::ssh::provisioning::{SetupOperation, SshSetupBackend, UnavailableKind};
use crate::ui::appearance_runtime;

/// What the scripted computer does with one setup.
enum Outcome {
    Finish(Result<DirectRouteId, SetupFailure>),
    /// Runs until the sheet cancels it, then reports the cancellation.
    UntilCancelled,
}

/// One setup the sheet asked for.
#[derive(Clone, Debug)]
struct Asked {
    destination: String,
    endpoint: SocketAddr,
    approvals: SetupApprovals,
}

/// The SSH computers the test reaches. Each setup takes the next scripted outcome.
#[derive(Default)]
struct Computers {
    outcomes: Mutex<VecDeque<Outcome>>,
    asked: Mutex<Vec<Asked>>,
}

impl Computers {
    fn scripted(outcomes: impl IntoIterator<Item = Outcome>) -> Arc<Self> {
        Arc::new(Self {
            outcomes: Mutex::new(outcomes.into_iter().collect()),
            asked: Mutex::default(),
        })
    }

    fn asked(&self) -> Vec<Asked> {
        self.asked.lock().unwrap().clone()
    }
}

impl SshSetupBackend for Computers {
    fn setup(
        &self,
        request: SetupRequest,
        cancellation: &CancellationToken,
        _: &SetupProgress,
    ) -> Result<DirectRouteId, SetupFailure> {
        self.asked.lock().unwrap().push(Asked {
            destination: request.destination.as_str().to_owned(),
            endpoint: request.endpoint,
            approvals: request.approvals.clone(),
        });
        let outcome = self.outcomes.lock().unwrap().pop_front();
        match outcome {
            Some(Outcome::Finish(result)) => result,
            Some(Outcome::UntilCancelled) | None => {
                let deadline = Instant::now() + Duration::from_secs(5);
                while !cancellation.is_cancelled() {
                    assert!(Instant::now() < deadline, "the sheet cancels the setup");
                    std::thread::sleep(Duration::from_millis(2));
                }
                Err(SetupFailure::Cancelled)
            }
        }
    }
}

/// OpenSSH's prompts, which no scripted setup asks for.
struct NoPrompts;
impl AskPassAttemptFactory for NoPrompts {
    fn start_attempt(&self, _: &AppPaths) -> Result<AskPassAttempt, AskPassUnavailable> {
        Err(AskPassUnavailable)
    }
}

struct Setup(Arc<Computers>);
impl SshSetupFactory for Setup {
    fn backend(&self) -> Option<Arc<dyn SshSetupBackend>> {
        Some(Arc::clone(&self.0) as Arc<dyn SshSetupBackend>)
    }
    fn prompts(&self, _: &Window, _: &mut App) -> Option<Arc<dyn AskPassAttemptFactory>> {
        Some(Arc::new(NoPrompts))
    }
    fn hosts(&self) -> Vec<SshDestination> {
        Vec::new()
    }
}

struct Sheet<'a> {
    window: Entity<SettingsWindow>,
    computers: Arc<Computers>,
    direct: DirectConnections,
    cx: &'a mut VisualTestContext,
}

/// Opens Settings on Servers with the local server, the scripted setup and a direct registry
/// holding one saved route, `studio`, which a scripted setup can return.
fn open_settings(
    outcomes: impl FnOnce(DirectRouteId) -> Vec<Outcome>,
    cx: &mut TestAppContext,
) -> (Sheet<'_>, DirectRouteId) {
    let (direct, route) = crate::direct_connections::tests::offline("studio");
    let computers = Computers::scripted(outcomes(route));
    (open_settings_using(computers, direct, cx), route)
}

/// Opens Settings as [`open_settings`] does, with the direct registry `direct` and no scripted
/// setups.
fn open_settings_with(direct: DirectConnections, cx: &mut TestAppContext) -> Sheet<'_> {
    open_settings_using(Arc::default(), direct, cx)
}

fn open_settings_using(
    computers: Arc<Computers>,
    direct: DirectConnections,
    cx: &mut TestAppContext,
) -> Sheet<'_> {
    // Setup and the registry's workers run on their own threads.
    cx.executor().allow_parking();
    let settings =
        crate::settings::Settings::load(MemoryStorage::with_document(&SettingsDocument::default()));
    let platform = RecordingAppearancePlatform::default();
    platform.set_system_appearance(Some(Appearance::Dark));
    let installed = direct.clone();
    let factory = Rc::new(Setup(Arc::clone(&computers)));
    cx.update(|cx| {
        appearance_runtime::install(settings, Rc::new(platform), cx)
            .expect("appearance runtime should install");
        crate::ui::init(cx).expect("UI initialization should succeed");
        crate::ui::ssh_setup::install(factory, cx);
        crate::ui::direct_servers::install(installed, cx);
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
        direct,
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

    /// Clicks the sheet's field whose selector is `selector` and types `text` into it, as the
    /// person does.
    fn type_into(&mut self, selector: &'static str, text: &str) {
        let position = self
            .cx
            .debug_bounds(selector)
            .unwrap_or_else(|| panic!("{selector} was not rendered"))
            .center();
        self.cx.simulate_click(position, gpui::Modifiers::none());
        self.cx.simulate_input(text);
        self.cx.run_until_parked();
    }

    /// Types `host` and `address` into the SSH method's empty fields.
    fn fill(&mut self, host: &str, address: &str) {
        self.type_into(HOST_SELECTOR, host);
        self.type_into(ADDRESS_SELECTOR, address);
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

    /// The sheet's node named `label`, if it offers one.
    fn find(&mut self, label: &str) -> Option<serde_json::Value> {
        let tree = A11yTree::read(self.cx);
        let dialog = *tree
            .with_role("Dialog")
            .first()
            .expect("the sheet is presented");
        tree.descendants(dialog)
            .into_iter()
            .find(|node| node["aria"]["label"] == label)
            .cloned()
    }

    /// The sheet's node named `label`, as assistive technology reads it.
    fn node(&mut self, label: &str) -> serde_json::Value {
        self.find(label)
            .unwrap_or_else(|| panic!("the sheet offers {label}"))
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

    /// Repaints until `done` holds, while setup and the registry's workers run on their own
    /// threads.
    fn settle(&mut self, mut done: impl FnMut(&mut Self) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            self.cx.run_until_parked();
            if done(self) {
                return;
            }
            assert!(Instant::now() < deadline, "the sheet settles");
            std::thread::sleep(Duration::from_millis(2));
        }
    }
}

fn endpoint(text: &str) -> SocketAddr {
    text.parse().unwrap()
}

#[gpui::test]
fn set_up_stays_unavailable_until_a_host_and_an_address_are_typed(cx: &mut TestAppContext) {
    let (mut sheet, _) = open_settings(|_| Vec::new(), cx);
    sheet.begin(AddServerRequest::Ssh(None));

    assert!(sheet.disabled("Set Up"), "an empty sheet sets up nothing");
    sheet.type_into(HOST_SELECTOR, "penguin");
    assert!(
        sheet.disabled("Set Up"),
        "a host alone has no network address"
    );
    sheet.type_into(ADDRESS_SELECTOR, "192.168.1.20");
    assert!(!sheet.disabled("Set Up"));
}

/// An SSH alias names how SSH reaches the computer, not an address other devices reach over the
/// network, so it is refused before SSH runs.
#[gpui::test]
fn an_address_that_isnt_an_ip_address_is_refused_before_setup_runs(cx: &mut TestAppContext) {
    let (mut sheet, _) = open_settings(|_| Vec::new(), cx);
    sheet.begin(AddServerRequest::Ssh(None));
    sheet.fill("penguin", "penguin.local");

    sheet.choose(SheetAction::SetUp.selector());

    assert!(sheet.says("Enter an IP address of that computer"));
    assert_eq!(sheet.title(), "Add Server");
    assert!(
        sheet.computers.asked().is_empty(),
        "SSH runs only for a usable address"
    );
}

#[gpui::test]
fn setup_selects_the_paired_server_it_added_and_closes_the_sheet(cx: &mut TestAppContext) {
    let (mut sheet, route) = open_settings(|route| vec![Outcome::Finish(Ok(route))], cx);
    sheet.begin(AddServerRequest::Ssh(None));
    sheet.fill("me@build.local", "192.168.1.20");

    sheet.cx.simulate_keystrokes("enter");
    sheet.settle(|sheet| !sheet.open() && sheet.selected() == Some(ServerChoice::Direct(route)));

    let asked = sheet.computers.asked();
    assert_eq!(asked.len(), 1);
    assert_eq!(asked[0].destination, "me@build.local");
    assert_eq!(
        asked[0].endpoint,
        endpoint("192.168.1.20:9231"),
        "an address without a port uses SpaceTerm's"
    );
    assert!(asked[0].approvals.install.is_none());
    assert!(asked[0].approvals.start.is_none());
    assert!(asked[0].approvals.expose_network.is_none());
}

/// Installing waits for the person, and the setup that installs carries the approval of the exact
/// offer the person reviewed.
#[gpui::test]
fn an_installation_offer_runs_again_only_with_its_own_approval(cx: &mut TestAppContext) {
    let (mut sheet, route) = open_settings(
        |route| {
            vec![
                Outcome::Finish(Err(SetupFailure::InstallationOffered {
                    version: "1.4.0".into(),
                    platform: ("Linux".into(), "x86_64".into()),
                    approval: installation_approval("penguin"),
                })),
                Outcome::Finish(Ok(route)),
            ]
        },
        cx,
    );
    sheet.begin(AddServerRequest::Ssh(None));
    sheet.fill("penguin", "192.168.1.20:9300");
    sheet.choose(SheetAction::SetUp.selector());
    sheet.settle(|sheet| sheet.title() == "Install SpaceTerm Server on \u{201C}penguin\u{201D}?");

    assert!(sheet.says("SpaceTerm may ask for your sudo password on that computer."));
    assert_eq!(
        sheet.computers.asked().len(),
        1,
        "nothing installs before Install"
    );

    sheet.choose(SheetAction::Install.selector());
    sheet.settle(|sheet| !sheet.open() && sheet.selected() == Some(ServerChoice::Direct(route)));

    let asked = sheet.computers.asked();
    assert_eq!(asked.len(), 2);
    assert_eq!(asked[1].endpoint, endpoint("192.168.1.20:9300"));
    assert_eq!(
        asked[1].approvals.install,
        Some(installation_approval("penguin"))
    );
    assert!(
        asked[1].approvals.start.is_none(),
        "installing approves no other step"
    );
}

#[gpui::test]
fn a_stopped_server_starts_only_after_the_person_chooses_start_server(cx: &mut TestAppContext) {
    let (mut sheet, route) = open_settings(
        |route| {
            vec![
                Outcome::Finish(Err(SetupFailure::StartupRequired {
                    approval: startup_approval("penguin"),
                })),
                Outcome::Finish(Ok(route)),
            ]
        },
        cx,
    );
    sheet.begin(AddServerRequest::Ssh(None));
    sheet.fill("penguin", "192.168.1.20");
    sheet.choose(SheetAction::SetUp.selector());
    sheet.settle(|sheet| sheet.title() == "Start SpaceTerm Server on \u{201C}penguin\u{201D}?");

    sheet.choose(SheetAction::Start.selector());
    sheet.settle(|sheet| !sheet.open() && sheet.selected() == Some(ServerChoice::Direct(route)));

    let asked = sheet.computers.asked();
    assert_eq!(asked[1].approvals.start, Some(startup_approval("penguin")));
    assert!(asked[1].approvals.install.is_none());
}

/// Network access moves from the address the server listens at only after the person allows it,
/// and the sheet says which address it leaves.
#[gpui::test]
fn network_access_changes_only_after_the_person_allows_it(cx: &mut TestAppContext) {
    let (mut sheet, route) = open_settings(
        |route| {
            vec![
                Outcome::Finish(Err(SetupFailure::NetworkAuthorizationRequired {
                    current: Some(endpoint("10.0.0.4:9231")),
                    approval: network_approval("penguin"),
                })),
                Outcome::Finish(Ok(route)),
            ]
        },
        cx,
    );
    sheet.begin(AddServerRequest::Ssh(None));
    sheet.fill("penguin", "192.168.1.20");
    sheet.choose(SheetAction::SetUp.selector());
    sheet.settle(|sheet| sheet.title() == "Allow Network Access on \u{201C}penguin\u{201D}?");

    assert!(sheet.says("at 192.168.1.20:9231"));
    assert!(sheet.says("It stops listening at 10.0.0.4:9231."));
    assert!(
        sheet.computers.asked()[0]
            .approvals
            .expose_network
            .is_none()
    );

    sheet.choose(SheetAction::AllowNetwork.selector());
    sheet.settle(|sheet| !sheet.open() && sheet.selected() == Some(ServerChoice::Direct(route)));

    assert_eq!(
        sheet.computers.asked()[1].approvals.expose_network,
        Some(network_approval("penguin")),
        "the setup that changes network access carries the approval of the offer the person saw"
    );
}

/// Cancel waits until setup has closed what it opened: the sheet says it is cancelling until
/// then, and adds no server.
#[gpui::test]
fn cancelling_setup_keeps_the_sheet_until_setup_stops(cx: &mut TestAppContext) {
    let (mut sheet, _) = open_settings(|_| vec![Outcome::UntilCancelled], cx);
    let saved = |sheet: &Sheet<'_>| {
        sheet
            .direct
            .routes()
            .into_iter()
            .map(|(route, _)| route)
            .collect::<Vec<_>>()
    };
    let routes = saved(&sheet);
    sheet.begin(AddServerRequest::Ssh(None));
    sheet.fill("penguin", "192.168.1.20");
    sheet.choose(SheetAction::SetUp.selector());
    sheet.settle(|sheet| sheet.says("Setting up \u{201C}penguin\u{201D}"));

    sheet.choose(SheetAction::Cancel.selector());
    sheet.settle(|sheet| !sheet.open());

    assert_eq!(sheet.computers.asked().len(), 1);
    assert_eq!(saved(&sheet), routes, "a cancelled setup adds no server");
    assert_eq!(sheet.selected(), Some(ServerChoice::Local));
}

/// A server this computer can't reach over the network says so, and Try Again repeats the same
/// setup without approvals it never asked for.
#[gpui::test]
fn an_unreachable_address_says_so_and_try_again_repeats_the_setup(cx: &mut TestAppContext) {
    let (mut sheet, route) = open_settings(
        |route| {
            vec![
                Outcome::Finish(Err(SetupFailure::Unavailable(
                    UnavailableKind::EndpointUnreachable,
                ))),
                Outcome::Finish(Ok(route)),
            ]
        },
        cx,
    );
    sheet.begin(AddServerRequest::Ssh(None));
    sheet.fill("penguin", "192.168.1.20");
    sheet.choose(SheetAction::SetUp.selector());
    sheet.settle(|sheet| sheet.title() == "Couldn\u{2019}t Add \u{201C}penguin\u{201D}");

    assert!(sheet.says("couldn\u{2019}t reach it at 192.168.1.20:9231"));
    assert_ne!(
        sheet.selected(),
        Some(ServerChoice::Direct(route)),
        "a server SpaceTerm can't reach isn't added"
    );

    sheet.choose(SheetAction::TryAgain.selector());
    sheet.settle(|sheet| !sheet.open() && sheet.selected() == Some(ServerChoice::Direct(route)));
    let asked = sheet.computers.asked();
    assert_eq!(asked[1].endpoint, asked[0].endpoint);
}

/// A save SpaceTerm couldn't confirm may already be listed, so the sheet offers no Try Again that
/// could pair this computer twice.
#[gpui::test]
fn an_unconfirmed_save_offers_no_try_again(cx: &mut TestAppContext) {
    let (mut sheet, _) = open_settings(
        |_| {
            vec![Outcome::Finish(Err(SetupFailure::Unconfirmed(
                SetupOperation::Save,
            )))]
        },
        cx,
    );
    sheet.begin(AddServerRequest::Ssh(None));
    sheet.fill("penguin", "192.168.1.20");
    sheet.choose(SheetAction::SetUp.selector());
    sheet.settle(|sheet| sheet.title() == "Couldn\u{2019}t Add \u{201C}penguin\u{201D}");

    assert!(sheet.says("couldn\u{2019}t confirm it saved the connection"));
    assert!(sheet.find("Try Again").is_none());
}

#[gpui::test]
fn escape_closes_a_filled_in_sheet_without_setting_up(cx: &mut TestAppContext) {
    let (mut sheet, _) = open_settings(|_| Vec::new(), cx);
    sheet.begin(AddServerRequest::Ssh(Some("penguin".into())));
    assert!(sheet.open());

    sheet.cx.simulate_keystrokes("escape");
    sheet.cx.run_until_parked();

    assert!(!sheet.open(), "Escape cancels the sheet");
    assert!(
        sheet.computers.asked().is_empty(),
        "a filled-in host sets up only after Set Up"
    );
}

/// Typing a host's first character makes Set Up available on the same sheet: the presentation,
/// the field's focus and Set Up's accessibility node all stay.
#[gpui::test]
fn typing_the_first_host_character_makes_set_up_available_in_place(cx: &mut TestAppContext) {
    let (mut sheet, _) = open_settings(|_| Vec::new(), cx);
    sheet.begin(AddServerRequest::Ssh(None));
    sheet.type_into(ADDRESS_SELECTOR, "192.168.1.20");
    let (before, _) = sheet.presented();
    let set_up = sheet.node("Set Up");

    sheet.type_into(HOST_SELECTOR, "p");

    let (after, focused) = sheet.presented();
    let available = sheet.node("Set Up");
    assert!(before.is_some());
    assert_eq!(after, before, "the sheet stays the same presentation");
    assert!(focused, "the host field keeps keyboard focus");
    assert_eq!(set_up["aria"]["disabled"], true);
    assert_ne!(available["aria"]["disabled"], true);
    assert_eq!(node_id(&set_up), node_id(&available));
}

/// Set Up can't be chosen while its values are missing, by pointer or by Return.
#[gpui::test]
fn an_unavailable_set_up_sets_up_nothing(cx: &mut TestAppContext) {
    let (mut sheet, _) = open_settings(|_| Vec::new(), cx);
    sheet.begin(AddServerRequest::Ssh(None));

    sheet.choose(SheetAction::SetUp.selector());
    sheet.cx.simulate_keystrokes("enter");
    sheet.cx.run_until_parked();

    assert!(sheet.open());
    assert_eq!(sheet.title(), "Add Server");
    assert!(sheet.computers.asked().is_empty());
}

#[test]
fn parse_endpoint_accepts_only_usable_ip_addresses() {
    assert_eq!(
        parse_endpoint("192.168.1.20"),
        Some(endpoint("192.168.1.20:9231"))
    );
    assert_eq!(
        parse_endpoint(" 192.168.1.20:9300 "),
        Some(endpoint("192.168.1.20:9300"))
    );
    assert_eq!(
        parse_endpoint("[fd00::4]"),
        Some(endpoint("[fd00::4]:9231"))
    );
    for refused in [
        "penguin.local",
        "127.0.0.1",
        "0.0.0.0",
        "192.168.1.20:0",
        "224.0.0.1",
        "",
    ] {
        assert_eq!(parse_endpoint(refused), None, "{refused}");
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
    let pin = crate::server_connection::direct::ServerPin {
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
fn a_delivered_invitation_names_its_server_and_address_and_waits_for_join(cx: &mut TestAppContext) {
    let (mut sheet, _) = open_settings(|_| Vec::new(), cx);
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
    let (mut sheet, _) = open_settings(|_| Vec::new(), cx);
    sheet.begin(AddServerRequest::Invitation(Some(Zeroizing::new(
        "spaceterm://pair/not-an-invitation".to_owned(),
    ))));

    assert!(sheet.says("Invalid SpaceTerm invitation link."));
    assert!(sheet.disabled("Join"));
}

#[gpui::test]
fn a_join_whose_save_is_unconfirmed_still_selects_its_server_and_closes_the_sheet(
    cx: &mut TestAppContext,
) {
    let (direct, join, route) = crate::direct_connections::tests::uncertain_join("studio");
    let mut sheet = open_settings_with(direct, cx);
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
    let mut sheet = open_settings_with(direct.clone(), cx);
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
    let mut sheet = open_settings_with(direct.clone(), cx);
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
    let mut sheet = open_settings_with(direct.clone(), cx);
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
    let mut sheet = open_settings_with(direct.clone(), cx);
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
    let mut sheet = open_settings_with(direct, cx);
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
