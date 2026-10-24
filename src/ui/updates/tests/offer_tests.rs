use super::*;
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::local_server::service::maintenance::{MaintenanceFacts, RemoteBuild};
use crate::local_server::service::{ServiceStartup, ServiceStatus};
use crate::platform::appearance::testing::RecordingAppearancePlatform;
use crate::settings::storage::testing::MemoryStorage;
use crate::ssh::cancellation::SshCancellationToken;
use crate::ssh::server_connection::{
    ConnectMode, ConnectProgress, ConnectedSshServer, RemoteConnectionStatus,
    RetainedSshConnection, ServerAvailability, ServerCandidate, SshConnectBackend,
    SshConnectFailure, SshConnectionIntentStore, SshIntentStoreError, SshServerConnections,
    SshServerContinuity, SshServerFacts, SshServerObservation,
};
use crate::ssh::server_operations::{
    RemoteMaintenanceRequest, RemoteOperationEntry, RemoteOperationHandle, RemoteOperationState,
    RemoteServerOperations, RemoteUpdateOffer,
};
use crate::ui::appearance::gpui_color;
use gpui::prelude::*;
use gpui::{Modifiers, MouseButton, TestAppContext, VisualTestContext, point, px};

#[test]
fn genuine_offers_raise_local_and_ssh_pills_without_changing_their_targets() {
    for action in [
        ServerUpdateAction::Local,
        ServerUpdateAction::Ssh(ssh("penguin")),
    ] {
        assert_eq!(
            pill(None, None, &[server("machine", action.clone())]),
            Some((
                "Update Server".into(),
                PillTone::Primary,
                None,
                PillCommand::Server(action)
            ))
        );
    }
    assert_eq!(
        pill(
            None,
            None,
            &[
                server("local", ServerUpdateAction::Local),
                server("penguin", ServerUpdateAction::Ssh(ssh("penguin"))),
            ]
        ),
        Some((
            "Update 2 Servers".into(),
            PillTone::Primary,
            None,
            PillCommand::Menu
        ))
    );
}

#[test]
fn only_a_genuine_server_offer_raises_client_progress_and_retry() {
    let offer = server("penguin", ServerUpdateAction::Ssh(ssh("penguin")));
    let attention = server("local", ServerUpdateAction::Review(MachineTarget::Local));
    for state in [
        UpdateState::Downloading {
            version: version(),
            received: 1,
            total: 4,
        },
        UpdateState::Verifying { version: version() },
        UpdateState::Failed {
            error: UpdateError::Download,
        },
    ] {
        let baseline = pill(Some(state.clone()), None, &[]).unwrap();
        for (rows, tone) in [
            (quiet_servers(), PillTone::Neutral),
            (vec![attention.clone()], PillTone::Neutral),
            (vec![offer.clone()], PillTone::Primary),
            (vec![attention.clone(), offer.clone()], PillTone::Primary),
        ] {
            let actual = pill(Some(state.clone()), None, &rows).unwrap();
            assert_eq!(
                actual,
                (baseline.0.clone(), tone, baseline.2, baseline.3.clone()),
                "{state:?}"
            );
            assert_eq!(
                UpdatePill::resolve(resolve(state.clone()), &rows)
                    .unwrap()
                    .marked(),
                rows.iter().any(|row| row
                    .action
                    .as_ref()
                    .and_then(ServerUpdateAction::need)
                    .is_some())
            );
        }
    }
}

#[test]
fn server_attention_counts_preserve_existing_warning_precedence() {
    let attention = server("local", ServerUpdateAction::Review(MachineTarget::Local));
    let other_attention = server(
        "penguin",
        ServerUpdateAction::Review(MachineTarget::Ssh(ssh("penguin"))),
    );
    let offer = server("macbookpro", ServerUpdateAction::Ssh(ssh("macbookpro")));
    assert_eq!(
        pill(None, None, std::slice::from_ref(&attention))
            .unwrap()
            .1,
        PillTone::Warning
    );
    assert_eq!(
        pill(None, None, &[attention.clone(), other_attention]).unwrap(),
        (
            "Update 2 Servers".into(),
            PillTone::Neutral,
            None,
            PillCommand::Menu
        )
    );
    assert_eq!(
        pill(None, None, &[attention.clone(), offer.clone()]).unwrap(),
        (
            "Update 2 Servers".into(),
            PillTone::Primary,
            None,
            PillCommand::Menu
        )
    );
    assert_eq!(
        pill(
            Some(UpdateState::Ready { version: version() }),
            Some(UpdateStage::Overdue),
            &[attention, offer]
        ),
        Some((
            "Install 0.4.2".into(),
            PillTone::Warning,
            Some(ControlGlyph::Warning),
            PillCommand::Client(Some(ControlCommand::RequestInstall))
        ))
    );
}

// These adapters answer only the existing SSH boundaries. They never create a service, start a
// connection, or touch the host's retained server files.
struct Computers;

fn continuity(name: &str) -> SshServerContinuity {
    SshServerContinuity {
        route: [9; 32],
        account: 501,
        build: "spaceterm-preflight".into(),
        server: [if name == "penguin" { 3 } else { 4 }; 32],
    }
}

impl SshConnectBackend for Computers {
    fn verified_candidate(&self, _: &(String, String)) -> Option<ServerCandidate> {
        Some(ServerCandidate {
            version: "1.5.0".into(),
            digest: "b".repeat(64),
        })
    }

    fn connect(
        &self,
        _: &crate::domain::SshDestination,
        _: ConnectMode,
        _: Option<&SshServerContinuity>,
        _: &SshCancellationToken,
        _: &ConnectProgress<'_>,
    ) -> Result<ConnectedSshServer, SshConnectFailure> {
        panic!("an Updates inventory must never connect to a server")
    }

    fn observe(
        &self,
        destination: &crate::domain::SshDestination,
        _: Option<&SshServerContinuity>,
        _: &SshCancellationToken,
        _: &ConnectProgress<'_>,
    ) -> Result<SshServerObservation, SshConnectFailure> {
        Ok(SshServerObservation {
            continuity: Some(continuity(destination.as_str())),
            availability: ServerAvailability::Running,
            facts: SshServerFacts {
                platform: ("Example OS".into(), "x86_64".into()),
                version: Some("1.4.0".into()),
                starts_automatically: true,
            },
        })
    }
}

struct Saved(Vec<&'static str>);

impl SshConnectionIntentStore for Saved {
    fn load(&self) -> Result<Vec<RetainedSshConnection>, SshIntentStoreError> {
        Ok(self
            .0
            .iter()
            .map(|name| RetainedSshConnection {
                destination: ssh(name),
                continuity: None,
                active: false,
            })
            .collect())
    }
    fn retain(&self, _: RetainedSshConnection) -> Result<(), SshIntentStoreError> {
        Ok(())
    }
    fn remove(&self, _: &crate::domain::SshDestination) -> Result<(), SshIntentStoreError> {
        Ok(())
    }
}

struct ObservedOperations {
    entries: Mutex<Vec<RemoteOperationEntry>>,
    changes: (async_channel::Sender<()>, async_channel::Receiver<()>),
}

impl ObservedOperations {
    fn new(names: &[&str]) -> Arc<Self> {
        Arc::new(Self {
            entries: Mutex::new(
                names
                    .iter()
                    .map(|name| RemoteOperationEntry {
                        destination: ssh(name),
                        removed: false,
                        operation: None,
                        state: RemoteOperationState::Read(Box::new(MaintenanceFacts {
                            server: continuity(name).server,
                            account: 501,
                            build: "spaceterm-preflight".into(),
                            status: ServiceStatus {
                                installed: true,
                                enabled: true,
                                running: true,
                                stopped: false,
                            },
                            startup: ServiceStartup::AtLogin,
                            approved: Some(RemoteBuild {
                                version: "1.4.0".into(),
                                commit: "a".repeat(40),
                                digest: "a".repeat(64),
                            }),
                            activation: None,
                        })),
                        observed_at: Some(Instant::now()),
                        update_offer: RemoteUpdateOffer::Unchecked,
                    })
                    .collect(),
            ),
            changes: async_channel::unbounded(),
        })
    }

    fn completed(&self, name: &str) {
        let mut entries = self.entries.lock().unwrap();
        let entry = entries
            .iter_mut()
            .find(|entry| entry.destination == ssh(name))
            .unwrap();
        let RemoteOperationState::Read(facts) = &mut entry.state else {
            panic!("the fixture holds a service read")
        };
        facts.approved = Some(RemoteBuild {
            version: "1.5.0".into(),
            commit: "b".repeat(40),
            digest: "b".repeat(64),
        });
        entry.observed_at = Some(Instant::now());
        self.changes.0.try_send(()).unwrap();
    }
}

impl RemoteServerOperations for ObservedOperations {
    fn subscribe(&self) -> async_channel::Receiver<()> {
        self.changes.1.clone()
    }
    fn entries(&self) -> Vec<RemoteOperationEntry> {
        self.entries.lock().unwrap().clone()
    }
    fn state(&self, destination: &crate::domain::SshDestination) -> RemoteOperationState {
        self.entries()
            .into_iter()
            .find(|entry| entry.destination == *destination)
            .map_or(RemoteOperationState::Unread, |entry| entry.state)
    }
    fn refresh(&self, _: &crate::domain::SshDestination) {
        panic!("rendering must not request maintenance")
    }
    fn set_automatic_start(&self, _: &crate::domain::SshDestination, _: bool) {
        panic!("inventory is passive")
    }
    fn describe(&self, _: &crate::domain::SshDestination, _: RemoteMaintenanceRequest) {
        panic!("inventory cannot prepare an update")
    }
    fn confirm(&self, _: RemoteOperationHandle) {
        panic!("inventory cannot confirm an update")
    }
    fn start(&self, _: &crate::domain::SshDestination) {
        panic!("inventory cannot start a server")
    }
    fn cancel(&self, _: &crate::domain::SshDestination) {
        panic!("inventory is passive")
    }
    fn resume(&self, _: &crate::domain::SshDestination) {
        panic!("inventory cannot recover an update")
    }
    fn acknowledge(&self, _: RemoteOperationHandle) {
        panic!("inventory is passive")
    }
    fn discard(&self, _: [u8; 32]) {
        panic!("inventory is passive")
    }
    fn dismiss(&self, _: [u8; 32]) {
        panic!("inventory is passive")
    }
    fn resume_retained(&self) {
        panic!("inventory is passive")
    }
}

fn recorded_servers(names: Vec<&'static str>, cx: &mut TestAppContext) -> Arc<ObservedOperations> {
    let operations = ObservedOperations::new(&names);
    let connections =
        SshServerConnections::new(Arc::new(Computers), Arc::new(Saved(names.clone())));
    connections.resume_retained();
    let deadline = Instant::now() + Duration::from_secs(3);
    while !connections.retained_inventory_loaded() {
        assert!(
            Instant::now() < deadline,
            "saved SSH inventory did not load"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
    for name in &names {
        connections.check(&ssh(name));
    }
    while names.iter().any(|name| {
        !matches!(
            connections.status(&ssh(name)),
            RemoteConnectionStatus::Checked { .. }
        )
    }) {
        assert!(
            Instant::now() < deadline,
            "scripted Check Status did not finish"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
    cx.update(|cx| {
        crate::ui::ssh_servers::install(connections, None, cx);
        crate::ssh::server_operations::install(operations.clone(), cx);
    });
    operations
}

fn appearance(appearance: Appearance, cx: &mut TestAppContext) {
    let mut document = crate::settings::SettingsDocument::default();
    document.appearance.mode = appearance.into();
    let settings = crate::settings::Settings::load(MemoryStorage::with_document(&document));
    let platform = RecordingAppearancePlatform::default();
    platform.set_system_appearance(Some(appearance));
    platform.set_reduced_motion(true);
    cx.update(|cx| {
        crate::ui::appearance_runtime::install(settings, Rc::new(platform), cx).unwrap();
        assert_eq!(crate::ui::appearance::chrome(cx).appearance, appearance);
    });
}

struct TitleBarControl(Entity<UpdateControl>);

impl Render for TitleBarControl {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        spaceterm_ui::ControlHost::TitleBar.mount(gpui::div().flex().child(self.0.clone()))
    }
}

fn open_control(cx: &mut TestAppContext) -> &mut VisualTestContext {
    let (_, visual) = cx
        .add_window_view(|window, cx| TitleBarControl(cx.new(|cx| UpdateControl::new(window, cx))));
    visual.update(|window, _| window.activate_window());
    visual.run_until_parked();
    visual
}

#[derive(Clone, Copy, Debug)]
enum PointerState {
    Normal,
    Hovered,
    Pressed,
    Disabled,
}

fn primary_fill(selector: &'static str, state: PointerState, cx: &mut VisualTestContext) {
    let bounds = cx
        .debug_bounds(selector)
        .expect("the update pill segment renders");
    cx.simulate_mouse_move(point(px(700.0), px(500.0)), None, Modifiers::none());
    if matches!(state, PointerState::Hovered | PointerState::Pressed) {
        cx.simulate_mouse_move(bounds.center(), None, Modifiers::none());
    }
    if matches!(state, PointerState::Pressed) {
        cx.simulate_mouse_down(bounds.center(), MouseButton::Left, Modifiers::none());
    }
    cx.run_until_parked();
    cx.update(|window, cx| {
        let colors = &crate::ui::appearance::chrome(cx).title_bar_controls.colors;
        let (fill, border) = match state {
            PointerState::Normal => (colors.primary_background, colors.primary_border),
            PointerState::Hovered => (colors.primary_hover_background, colors.primary_hover_border),
            PointerState::Pressed => (
                colors.primary_pressed_background,
                colors.primary_pressed_border,
            ),
            PointerState::Disabled => (
                colors.primary_disabled_background,
                colors.primary_disabled_border,
            ),
        };
        let painted_bounds = bounds.scale(window.scale_factor());
        let quads = window.painted_quads();
        let quad = quads
            .iter()
            .find(|quad| quad.bounds == painted_bounds && !quad.background.is_transparent())
            .expect("the capsule segment paints a fill");
        assert_eq!(
            quad.background,
            gpui_color(fill).into(),
            "{selector} {state:?}"
        );
        // GPUI splits an unfilled border into strips around its empty interior.
        let expected_border = gpui_color(border);
        let border_bounds = quads
            .iter()
            .filter(|quad| {
                if expected_border.a == 0.0 {
                    quad.border_color.a > 0.0
                } else {
                    quad.border_color == expected_border.into()
                }
            })
            .map(|quad| {
                quad.bounds
                    .intersect(&quad.content_mask.bounds)
                    .intersect(&painted_bounds)
            })
            .filter(|bounds| !bounds.is_empty())
            .reduce(|bounds, next| bounds.union(&next));
        assert_eq!(
            border_bounds,
            (expected_border.a > 0.0).then_some(painted_bounds),
            "{selector} {state:?}"
        );
    });
    if matches!(state, PointerState::Pressed) {
        // Release outside so the paint probe dispatches neither an update nor a menu action.
        cx.simulate_mouse_move(
            point(px(700.0), px(500.0)),
            MouseButton::Left,
            Modifiers::none(),
        );
        cx.simulate_mouse_up(
            point(px(700.0), px(500.0)),
            MouseButton::Left,
            Modifiers::none(),
        );
        cx.run_until_parked();
    }
}

#[gpui::test]
fn server_offer_capsule_and_chevron_paint_authored_primary_states(cx: &mut TestAppContext) {
    appearance(Appearance::Light, cx);
    manual_updates(cx);
    recorded_servers(vec!["penguin"], cx);
    let visual = open_control(cx);
    for state in [
        PointerState::Normal,
        PointerState::Hovered,
        PointerState::Pressed,
    ] {
        primary_fill("update-control", state, visual);
        primary_fill("update-control-menu", state, visual);
    }
}

#[gpui::test]
fn dark_server_offer_capsule_and_chevron_paint_authored_primary_states(cx: &mut TestAppContext) {
    appearance(Appearance::Dark, cx);
    manual_updates(cx);
    recorded_servers(vec!["penguin"], cx);
    let visual = open_control(cx);
    for state in [
        PointerState::Normal,
        PointerState::Hovered,
        PointerState::Pressed,
    ] {
        primary_fill("update-control", state, visual);
        primary_fill("update-control-menu", state, visual);
    }
}

#[gpui::test]
fn multiple_offer_menu_paints_primary_and_completion_keeps_the_other_server(
    cx: &mut TestAppContext,
) {
    use spaceterm_ui::a11y_testing::A11yTree;
    appearance(Appearance::Dark, cx);
    manual_updates(cx);
    let operations = recorded_servers(vec!["penguin", "macbookpro"], cx);
    let visual = open_control(cx);
    A11yTree::read(visual).node("Update 2 Servers");
    assert!(visual.debug_bounds("update-control").is_none());
    primary_fill("update-control-menu", PointerState::Normal, visual);

    // A completed operation publishes fresh approved facts through the real subscription. The
    // closed menu and active window must update without another click or activation event.
    operations.completed("penguin");
    visual.run_until_parked();
    primary_fill("update-control", PointerState::Normal, visual);
    A11yTree::read(visual).node("Update Server");
    let menu = visual.debug_bounds("update-control-menu").unwrap().center();
    visual.simulate_click(menu, Modifiers::none());
    visual.run_until_parked();
    A11yTree::read(visual).node("Up to Date");
    A11yTree::read(visual).node("Update Available");
    A11yTree::read(visual).node("Update Server…");
    operations.completed("macbookpro");
    visual.run_until_parked();
    assert!(visual.debug_bounds("update-control").is_none());
    assert!(visual.debug_bounds("update-control-menu").is_none());
}

#[gpui::test]
fn server_offer_behind_client_progress_retry_and_checking_paints_primary(cx: &mut TestAppContext) {
    use crate::updates::UpdateEvent;
    use spaceterm_ui::a11y_testing::A11yTree;
    appearance(Appearance::Light, cx);
    let (adapter, updates) = manual_updates(cx);
    recorded_servers(vec!["penguin"], cx);
    updates.update(cx, |updates, cx| updates.check(false, cx));
    adapter.emit(UpdateEvent::Available(version()));
    cx.run_until_parked();
    updates.update(cx, |updates, cx| updates.download(cx));
    let visual = open_control(cx);
    for (label, disabled, progress) in [
        ("Downloading 0.4.2", false, true),
        ("Retry 0.4.2", false, false),
        ("Checking 0.4.2", true, true),
    ] {
        match label {
            "Retry 0.4.2" => {
                adapter.emit(UpdateEvent::Failed(UpdateError::Download));
                adapter.emit(UpdateEvent::Finished);
            }
            "Checking 0.4.2" => updates.update(visual, |updates, cx| updates.retry_download(cx)),
            _ => {}
        }
        visual.run_until_parked();
        let tree = A11yTree::read(visual);
        assert_eq!(
            tree.node(label)["aria"]["disabled"] == true,
            disabled,
            "{label}"
        );
        assert_ne!(
            tree.node("Updates")["aria"]["disabled"],
            true,
            "the chevron stays enabled during {label}"
        );
        assert_eq!(
            visual
                .debug_bounds("update-control-progress-track")
                .is_some(),
            progress,
            "{label}"
        );
        assert!(visual.debug_bounds("update-control-servers-dot").is_some());
        primary_fill(
            "update-control",
            if disabled {
                PointerState::Disabled
            } else {
                PointerState::Normal
            },
            visual,
        );
        primary_fill("update-control-menu", PointerState::Normal, visual);
    }
    assert_eq!(adapter.downloads.get(), 1);
}
