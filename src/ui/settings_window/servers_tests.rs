//! The Servers section, driven through published server snapshots and a recording request sink.

use std::cell::RefCell;
use std::rc::Rc;

use gpui::{
    App, Entity, KeyDownEvent, KeyUpEvent, Keystroke, Modifiers, TestAppContext, VisualTestContext,
};
use serde_json::Value;
use spaceterm_ui::a11y_testing::{A11yTree, perform};

use super::copy;
use super::model::{
    self, BindScope, IncompatibleSide, PendingUnpair, RemoteAccess, ServerKey, ServerPermission,
    ServerPermissionKind, ServerPermissionState, ServerPermissions, ServerRequest, ServerRequests,
    ServerRoute, ServerStatus, ServerSummary, ServerTextSetting, Servers, ServersSnapshot,
    SettingEdit, SettingEditState, SettingRejection,
};
use crate::appearance::Appearance;
use crate::platform::appearance::testing::RecordingAppearancePlatform;
use crate::platform::permission_access::AccessibilityNaming;
use crate::platform::window_movement::RecordingOperatingSystemWindowDragPlatform;
use crate::settings::SettingsDocument;
use crate::settings::storage::testing::MemoryStorage;
use crate::ui::appearance_runtime;
use crate::ui::settings_window::SettingsWindow;
use crate::ui::settings_window::catalog::{SettingsRowId, SettingsSectionId};
use crate::worktrees::path_template::WorktreePathTemplateError;

#[path = "servers/paired_devices/tests.rs"]
mod paired_devices_tests;

const LOCATION: &str = "~/Worktrees/{repository}/{branch}";

/// Records every request the section sends.
#[derive(Default)]
struct Recorder(RefCell<Vec<ServerRequest>>);

impl ServerRequests for Recorder {
    fn send(&self, request: model::BoundServerRequest, _: &mut App) {
        self.0.borrow_mut().push(request.request);
    }
}

impl Recorder {
    fn take(&self) -> Vec<ServerRequest> {
        self.0.borrow_mut().drain(..).collect()
    }
}

pub(in crate::ui::settings_window) struct Fixture<'a> {
    pub(in crate::ui::settings_window) window: Entity<SettingsWindow>,
    servers: Entity<Servers>,
    requests: Rc<Recorder>,
    storage: std::sync::Arc<MemoryStorage>,
    pub(in crate::ui::settings_window) cx: &'a mut VisualTestContext,
}

impl Fixture<'_> {
    fn publish(&mut self, snapshot: ServersSnapshot) {
        let servers = self.servers.clone();
        self.cx
            .update(|_, cx| servers.update(cx, |servers, cx| servers.publish(snapshot, cx)));
        self.cx.run_until_parked();
    }

    pub(in crate::ui::settings_window) fn click(&mut self, selector: &str) {
        let selector: &'static str = selector.to_owned().leak();
        let position = self
            .cx
            .debug_bounds(selector)
            .unwrap_or_else(|| panic!("{selector} was not rendered"))
            .center();
        self.cx
            .simulate_mouse_move(position, None, Modifiers::none());
        self.cx.simulate_click(position, Modifiers::none());
        self.cx.run_until_parked();
    }

    fn review_management(&mut self, selector: &str) {
        self.click(selector);
        let reads = self.requests.take();
        if reads.is_empty() {
            return;
        }
        assert_eq!(reads.len(), 1);
        let read = &reads[0];
        let mut snapshot = self
            .servers
            .read_with(self.cx, |source, _| source.snapshot().clone());
        let server = snapshot
            .servers
            .iter_mut()
            .find(|server| Some(&server.key) == read.server())
            .unwrap();
        match read {
            ServerRequest::AssessStop(_) => {
                server.management.stop = Some(host_assessment(
                    spaceterm_host::account_service::ServiceAction::Stop,
                    server.management.stop.as_ref().map_or(1, |value| {
                        if value.reference.as_str() == "host-1" {
                            2
                        } else {
                            1
                        }
                    }),
                ))
            }
            ServerRequest::AssessRestart(_) => {
                server.management.restart = Some(host_assessment(
                    spaceterm_host::account_service::ServiceAction::Restart,
                    1,
                ))
            }
            ServerRequest::AssessEndAllSessions(_) => {
                server.management.end_all = Some(end_all_assessment(server))
            }
            ServerRequest::AssessInstallUpdate(_) => {
                let mut assessment = staged_server(server.clone())
                    .installations
                    .activation
                    .unwrap();
                let token = if server
                    .installations
                    .activation
                    .as_ref()
                    .is_some_and(|value| value.reference.as_str() == "activation-1")
                {
                    "activation-2"
                } else {
                    "activation-1"
                };
                assessment.reference =
                    spaceterm_client::management_cli::AssessmentToken::new(token.into()).unwrap();
                server.installations.activation = Some(assessment);
            }
            _ => panic!("expected a read-only management assessment"),
        }
        self.publish(snapshot);
    }

    fn rendered(&mut self, selector: &str) -> bool {
        let selector: &'static str = selector.to_owned().leak();
        self.cx.debug_bounds(selector).is_some()
    }

    fn selected(&mut self) -> Option<ServerKey> {
        self.window.read_with(self.cx, |window, _| {
            window
                .servers
                .selected_server()
                .map(|server| server.key.clone())
        })
    }

    fn caption(&mut self, row: SettingsRowId) -> Option<String> {
        self.window.read_with(self.cx, |window, cx| {
            window
                .server_row_caption(row, cx)
                .map(|(text, _)| text.to_string())
        })
    }

    fn field(&mut self) -> String {
        self.window.read_with(self.cx, |window, cx| {
            window.servers.worktree.input.read(cx).value().to_owned()
        })
    }

    /// Presses and releases Return. A focused button activates on release, so a key-down alone
    /// would leave a focused modal action untouched.
    fn press_return(&mut self) {
        let keystroke = Keystroke::parse("enter").unwrap_or_default();
        self.cx.simulate_event(KeyDownEvent {
            keystroke: keystroke.clone(),
            prefer_character_input: false,
            is_held: false,
        });
        self.cx.simulate_event(KeyUpEvent { keystroke });
        self.cx.run_until_parked();
    }

    pub(in crate::ui::settings_window) fn modal_open(&mut self) -> bool {
        self.cx
            .update(|window, cx| spaceterm_ui::window_modal_is_open(window, cx))
    }

    /// Types `query` into Settings Search.
    fn search(&mut self, query: &str) {
        let window = self.window.clone();
        let query = query.to_owned();
        self.cx.update(|_, cx| {
            window.update(cx, |window, cx| {
                let search = window.search.clone();
                search.update(cx, |search, cx| search.set_value(query, cx));
            });
        });
        self.cx.run_until_parked();
    }

    fn rows(&mut self) -> Vec<SettingsRowId> {
        self.window.read_with(self.cx, |window, _| {
            window.rows_for(SettingsSectionId::Servers)
        })
    }

    /// Replaces the New Worktree Location field's text and presses Return, as typing does.
    fn enter_location(&mut self, text: &str) {
        self.click("settings-server-worktree-location");
        let input = self
            .window
            .read_with(self.cx, |window, _| window.servers.worktree.input.clone());
        let text = text.to_owned();
        self.cx.update(|_, cx| {
            input.update(cx, |input, cx| input.set_value(text, cx));
        });
        self.cx.simulate_keystrokes("enter");
        self.cx.run_until_parked();
    }

    pub(in crate::ui::settings_window) fn document(&mut self) -> SettingsDocument {
        self.window
            .read_with(self.cx, |window, _| window.editor.document().clone())
    }
}

/// Opens Settings on the Servers section with `snapshot` published.
fn open(appearance: Appearance, snapshot: ServersSnapshot, cx: &mut TestAppContext) -> Fixture<'_> {
    open_with_source(appearance, snapshot, None, cx)
}

pub(in crate::ui::settings_window) fn open_from_publisher(
    source: Entity<Servers>,
    cx: &mut TestAppContext,
) -> Fixture<'_> {
    open_with_source(
        Appearance::Dark,
        ServersSnapshot::default(),
        Some(source),
        cx,
    )
}

fn open_with_source(
    appearance: Appearance,
    snapshot: ServersSnapshot,
    publisher: Option<Entity<Servers>>,
    cx: &mut TestAppContext,
) -> Fixture<'_> {
    let storage = MemoryStorage::with_document(&SettingsDocument::default());
    let settings = crate::settings::Settings::load(storage.clone());
    let platform = RecordingAppearancePlatform::default();
    platform.set_system_appearance(Some(appearance));
    let requests = Rc::new(Recorder::default());
    let servers = cx.update(|cx| {
        appearance_runtime::install(settings, Rc::new(platform), cx)
            .expect("appearance runtime should install");
        crate::ui::init(cx).expect("UI initialization should succeed");
        if let Some(source) = publisher {
            source
        } else {
            let servers = model::install(requests.clone(), cx);
            servers.update(cx, |servers, cx| servers.publish(snapshot, cx));
            servers
        }
    });
    let (window, cx) = cx.add_window_view(|window, cx| {
        SettingsWindow::new_with_capabilities(
            Rc::new(RecordingOperatingSystemWindowDragPlatform::default()),
            Default::default(),
            None,
            window,
            cx,
        )
    });
    cx.update(|window, _| window.activate_window());
    cx.run_until_parked();
    let mut fixture = Fixture {
        window,
        servers,
        requests,
        storage,
        cx,
    };
    fixture.click("settings-navigation-settings-section-servers");
    fixture
}

fn location(revision: u64, edit: Option<SettingEdit>) -> ServerTextSetting {
    ServerTextSetting {
        value: LOCATION.into(),
        revision,
        edit,
    }
}

fn scopes() -> Vec<BindScope> {
    vec![
        BindScope {
            id: "lan".into(),
            label: "Local network".into(),
            addresses: "192.168.1.20".into(),
            endpoints: vec!["192.168.1.20:7443".parse().unwrap()],
        },
        BindScope {
            id: "tailnet".into(),
            label: "Tailnet".into(),
            addresses: "100.64.0.20".into(),
            endpoints: vec!["100.64.0.20:7443".parse().unwrap()],
        },
    ]
}

pub(super) fn pairing(server: u8, generation: u64) -> spaceterm_core::pairing::LocalPairingKey {
    use spaceterm_core::{
        identity::{PairingId, ServerId},
        pairing::{LocalPairingGeneration, LocalPairingKey, LocalPairingKeyParts},
    };
    let server = ServerId::new([server; 16]);
    LocalPairingKey::new(LocalPairingKeyParts {
        server,
        pairing: PairingId::new(server, 1).unwrap(),
        generation: LocalPairingGeneration::new(generation).unwrap(),
    })
    .unwrap()
}

pub(super) fn operation(server: u8) -> spaceterm_core::operations::OperationId {
    use spaceterm_core::{
        identity::{ClientId, OperationStreamId, OperationStreamKey},
        operations::{OperationId, OperationSequence},
    };
    let key = pairing(server, 1);
    let id = key.parts().server;
    OperationId::new(
        OperationStreamId::new(
            OperationStreamKey::new(id, 1).unwrap(),
            key.parts().pairing,
            ClientId::new(id, 1).unwrap(),
        )
        .unwrap(),
        OperationSequence::new(1).unwrap(),
    )
}

pub(super) fn staged_server(mut server: ServerSummary) -> ServerSummary {
    use spaceterm_host::{
        native_deployment::contracts::NativeRequestId,
        startup::{DesiredServerState, InstallationId},
    };
    use std::num::NonZeroU64;
    let candidate = model::ServerInstallation {
        installation: InstallationId(NonZeroU64::new(2).unwrap()),
        artifact_sha256: [2; 32],
        version: "1.5.0".into(),
    };
    let generation = NonZeroU64::new(1).unwrap();
    server.installations = model::ServerInstallations {
        selection_generation: Some(generation),
        staged: Some(candidate.clone()),
        activation: Some(model::UpdateAssessment {
            reference: spaceterm_client::management_cli::confirmation::AssessmentToken::new(
                "activation-1".into(),
            )
            .unwrap(),
            native_request: NativeRequestId::new([1; 16]).unwrap(),
            original_pairing: server.pairing.clone(),
            selection_generation: generation,
            approved: None,
            running: None,
            candidate,
            runtime_epoch: server.runtime_epoch,
            desired: if server.status == ServerStatus::Stopped {
                DesiredServerState::Stopped
            } else {
                DesiredServerState::Running
            },
        }),
        ..model::ServerInstallations::default()
    };
    server
}

pub(super) fn local() -> ServerSummary {
    ServerSummary {
        key: ServerKey::new("local"),
        pairing: pairing(1, 1),
        runtime_epoch: Some(spaceterm_core::authority::ServerEpoch::new(1)),
        service_lifecycle: Some(model::ServerServiceLifecycle::LoginSession),
        compatibility: None,
        installations: model::ServerInstallations::default(),
        paired_devices: None,
        unpair_assessment: None,
        management: model::ServerManagement::default(),
        name: "Studio".into(),
        route: ServerRoute::Local,
        status: ServerStatus::Running,
        version: Some("1.4.0".into()),
        staged_version: None,
        worktree_location: Some(location(3, None)),
        permissions: Some(ServerPermissions {
            naming: AccessibilityNaming::DeviceControl,
            entries: vec![
                ServerPermission {
                    kind: ServerPermissionKind::Microphone,
                    state: ServerPermissionState::Allowed,
                },
                ServerPermission {
                    kind: ServerPermissionKind::ScreenRecording,
                    state: ServerPermissionState::NotSetUp,
                },
                ServerPermission {
                    kind: ServerPermissionKind::Accessibility,
                    state: ServerPermissionState::NotAllowed,
                },
            ],
        }),
        remote_access: Some(RemoteAccess {
            version: spaceterm_core::authority::FieldVersion::new(1),
            enabled: false,
            scope: None,
            scopes: scopes(),
            pending: false,
            endpoints: Vec::new(),
            edit: None,
        }),
    }
}

fn ssh() -> ServerSummary {
    ServerSummary {
        key: ServerKey::new("build"),
        pairing: pairing(2, 1),
        runtime_epoch: Some(spaceterm_core::authority::ServerEpoch::new(1)),
        service_lifecycle: Some(model::ServerServiceLifecycle::BootWithLingering),
        compatibility: None,
        installations: model::ServerInstallations::default(),
        paired_devices: None,
        unpair_assessment: None,
        management: model::ServerManagement::default(),
        name: "Build Box".into(),
        route: ServerRoute::Ssh {
            destination: "dev@build.example.com".into(),
        },
        status: ServerStatus::Offline,
        version: Some("1.3.2".into()),
        staged_version: None,
        worktree_location: None,
        permissions: None,
        remote_access: None,
    }
}

fn wss() -> ServerSummary {
    ServerSummary {
        key: ServerKey::new("lab"),
        pairing: pairing(3, 1),
        runtime_epoch: Some(spaceterm_core::authority::ServerEpoch::new(1)),
        service_lifecycle: Some(model::ServerServiceLifecycle::BootWithLingering),
        compatibility: None,
        installations: model::ServerInstallations::default(),
        paired_devices: None,
        unpair_assessment: None,
        management: model::ServerManagement::default(),
        name: "Lab".into(),
        route: ServerRoute::Wss {
            authority: "lab.example.com:7443".into(),
        },
        status: ServerStatus::Incompatible(IncompatibleSide::Server),
        version: Some("1.1.0".into()),
        staged_version: None,
        worktree_location: None,
        permissions: None,
        remote_access: None,
    }
}

fn snapshot(servers: Vec<ServerSummary>) -> ServersSnapshot {
    ServersSnapshot {
        servers,
        pending_unpairs: Vec::new(),
    }
}

/// The published node with `role` and `label`.
fn named<'a>(tree: &'a A11yTree, role: &str, label: &str) -> &'a Value {
    tree.with_role(role)
        .into_iter()
        .find(|node| node["aria"]["label"] == label)
        .unwrap_or_else(|| panic!("no {role} is named {label:?}"))
}

// Presentation -------------------------------------------------------------------------------

#[gpui::test]
fn the_list_groups_local_servers_before_remote_ones_and_states_each_status_in_words(
    cx: &mut TestAppContext,
) {
    // Published remote first, so the order comes from the section.
    let fixture = open(Appearance::Dark, snapshot(vec![wss(), ssh(), local()]), cx);

    let local_bounds = fixture
        .cx
        .debug_bounds("settings-server-option-local")
        .unwrap();
    let ssh_bounds = fixture
        .cx
        .debug_bounds("settings-server-option-build")
        .unwrap();
    let wss_bounds = fixture
        .cx
        .debug_bounds("settings-server-option-lab")
        .unwrap();
    // Local first, then the remote servers in the order the client lists them.
    assert!(local_bounds.top() < wss_bounds.top());
    assert!(wss_bounds.top() < ssh_bounds.top());
    let local_group = fixture
        .cx
        .debug_bounds("settings-server-group-local")
        .unwrap();
    let remote_group = fixture
        .cx
        .debug_bounds("settings-server-group-remote")
        .unwrap();
    assert!(local_group.contains(&local_bounds.center()));
    assert!(remote_group.contains(&ssh_bounds.center()));
    assert!(remote_group.contains(&wss_bounds.center()));

    let tree = A11yTree::read(fixture.cx);
    named(&tree, "ListBox", "Servers");
    named(&tree, "Group", "Local");
    named(&tree, "Group", "Remote");
    let studio = named(&tree, "ListBoxOption", "Studio");
    assert_eq!(
        studio["aria"]["description"],
        "Running, Local · Version 1.4.0"
    );
    assert_eq!(studio["aria"]["selected"], true);
    assert_eq!(studio["aria"]["position_in_set"], 1);
    assert_eq!(studio["aria"]["size_of_set"], 3);
    let build = named(&tree, "ListBoxOption", "Build Box");
    assert_eq!(
        build["aria"]["description"],
        "Offline, SSH · dev@build.example.com · Version 1.3.2"
    );
    assert_eq!(build["aria"]["selected"], false);
    assert_eq!(build["aria"]["position_in_set"], 3);
    let lab = named(&tree, "ListBoxOption", "Lab");
    assert_eq!(
        lab["aria"]["description"],
        "Incompatible, WSS · lab.example.com:7443 · Version 1.1.0"
    );
    assert_eq!(lab["aria"]["position_in_set"], 2);

    // The selected server's rows sit under its name, which sits under the page's title.
    assert_eq!(named(&tree, "Heading", "Servers")["aria"]["level"], 1);
    let heading = named(&tree, "Heading", "Studio");
    assert_eq!(heading["aria"]["level"], 2);
    for group in [
        "Server",
        "Server Settings",
        "Permissions",
        "Remote Access",
        "Pairing",
    ] {
        assert_eq!(
            named(&tree, "Heading", group)["aria"]["level"],
            3,
            "{group}"
        );
    }
    assert!(tree.find_text("Running").is_some());
    assert!(tree.find_text("Private local connection").is_some());
    assert!(tree.find_text("1.4.0").is_some());
}

#[test]
fn every_status_reads_as_words_with_its_own_explanation() {
    let statuses = [
        ServerStatus::Connecting,
        ServerStatus::Running,
        ServerStatus::Starting,
        ServerStatus::Stopping,
        ServerStatus::Stopped,
        ServerStatus::Offline,
        ServerStatus::Incompatible(IncompatibleSide::ThisDevice),
        ServerStatus::Incompatible(IncompatibleSide::Server),
        ServerStatus::UnsupportedProfiles,
        ServerStatus::Updating {
            version: "1.5.0".into(),
        },
    ];
    let mut explanations = Vec::new();
    for status in statuses {
        let server = ServerSummary {
            status: status.clone(),
            ..ssh()
        };
        assert!(!copy::status_label(&status).is_empty());
        let explanation = copy::status_explanation(&server);
        assert!(!explanations.contains(&explanation), "{status:?}");
        explanations.push(explanation);
    }
    assert_eq!(
        copy::status_label(&ServerStatus::Updating {
            version: "1.5.0".into()
        }),
        "Updating to 1.5.0…"
    );
    assert_eq!(
        copy::status_explanation(&ServerSummary {
            status: ServerStatus::Stopped,
            ..ssh()
        }),
        "It stays stopped, including after its machine restarts, until you start it."
    );
    assert_eq!(
        copy::status_explanation(&ServerSummary {
            status: ServerStatus::Incompatible(IncompatibleSide::ThisDevice),
            ..ssh()
        }),
        "Build Box needs a newer SpaceTerm on this device. Update SpaceTerm in Updates. SpaceTerm restarts, and Build Box's Terminal Sessions keep running."
    );
}

#[gpui::test]
fn with_no_servers_the_section_explains_how_servers_arrive(cx: &mut TestAppContext) {
    let mut fixture = open(Appearance::Dark, ServersSnapshot::default(), cx);

    assert!(fixture.rendered("settings-server-list"));
    assert!(fixture.rendered("settings-row-server-list"));
    assert!(!fixture.rendered("settings-server-heading"));
    assert!(!fixture.rendered("settings-row-server-status"));
    let rows = fixture.window.read_with(fixture.cx, |window, _| {
        window.rows_for(SettingsSectionId::Servers)
    });
    assert_eq!(rows, vec![SettingsRowId::ServerList]);
    let tree = A11yTree::read(fixture.cx);
    assert!(tree.find_text("No servers yet").is_some());
}

#[gpui::test]
fn servers_installed_after_settings_opens_appear_without_reopening(cx: &mut TestAppContext) {
    let storage = MemoryStorage::with_document(&SettingsDocument::default());
    let settings = crate::settings::Settings::load(storage);
    cx.update(|cx| {
        appearance_runtime::install(
            settings,
            Rc::new(RecordingAppearancePlatform::default()),
            cx,
        )
        .expect("appearance runtime should install");
        crate::ui::init(cx).expect("UI initialization should succeed");
    });
    let (window, cx) = cx.add_window_view(|window, cx| {
        SettingsWindow::new_with_capabilities(
            Rc::new(RecordingOperatingSystemWindowDragPlatform::default()),
            Default::default(),
            None,
            window,
            cx,
        )
    });
    cx.run_until_parked();
    assert!(window.read_with(cx, |window, _| window.servers.selected_server().is_none()));

    let requests = Rc::new(Recorder::default());
    cx.update(|_, cx| {
        let servers = model::install(requests, cx);
        servers.update(cx, |servers, cx| {
            servers.publish(snapshot(vec![local()]), cx)
        });
    });
    cx.run_until_parked();
    assert_eq!(
        window.read_with(cx, |window, _| window
            .rows_for(SettingsSectionId::Servers)
            .len()),
        11
    );
}

// Selection and keyboard ---------------------------------------------------------------------

#[gpui::test]
fn clicking_a_server_selects_it_and_its_rows_follow(cx: &mut TestAppContext) {
    let mut fixture = open(Appearance::Dark, snapshot(vec![local(), ssh()]), cx);
    assert_eq!(fixture.selected(), Some(ServerKey::new("local")));

    fixture.click("settings-server-option-build");
    assert_eq!(fixture.selected(), Some(ServerKey::new("build")));
    let tree = A11yTree::read(fixture.cx);
    named(&tree, "Heading", "Build Box");
    assert_eq!(
        named(&tree, "ListBoxOption", "Build Box")["aria"]["selected"],
        true
    );
    assert!(tree.find_text("Offline").is_some());
    assert!(tree.find_text("SSH to dev@build.example.com").is_some());
    assert_eq!(
        fixture
            .caption(SettingsRowId::ServerWorktreeLocation)
            .as_deref(),
        Some("Connect to Build Box to change its settings.")
    );
}

#[gpui::test]
fn arrow_keys_move_the_selection_through_the_list_and_stop_at_both_ends(cx: &mut TestAppContext) {
    let mut fixture = open(Appearance::Dark, snapshot(vec![local(), ssh(), wss()]), cx);
    let window = fixture.window.clone();
    fixture.cx.update(|gpui_window, cx| {
        let focus = window.read(cx).servers.list_focus.clone();
        focus.focus(gpui_window, cx);
    });
    fixture.cx.run_until_parked();

    fixture.cx.simulate_keystrokes("down");
    assert_eq!(fixture.selected(), Some(ServerKey::new("build")));
    fixture.cx.simulate_keystrokes("down down");
    assert_eq!(fixture.selected(), Some(ServerKey::new("lab")));
    fixture.cx.simulate_keystrokes("up");
    assert_eq!(fixture.selected(), Some(ServerKey::new("build")));
    fixture.cx.simulate_keystrokes("home");
    assert_eq!(fixture.selected(), Some(ServerKey::new("local")));
    fixture.cx.simulate_keystrokes("up");
    assert_eq!(fixture.selected(), Some(ServerKey::new("local")));
    fixture.cx.simulate_keystrokes("end");
    assert_eq!(fixture.selected(), Some(ServerKey::new("lab")));

    // The list is one tab stop that names the selected option as its active descendant.
    let tree = A11yTree::read(fixture.cx);
    let lab = named(&tree, "ListBoxOption", "Lab");
    assert_eq!(lab["aria"]["selected"], true);
    let focused = tree.focused().expect("the list's selection is focused");
    assert_eq!(focused["aria"]["label"], "Lab");
}

#[gpui::test]
fn assistive_technology_selects_a_server_by_pressing_it(cx: &mut TestAppContext) {
    let mut fixture = open(Appearance::Dark, snapshot(vec![local(), ssh()]), cx);
    let tree = A11yTree::read(fixture.cx);
    perform(
        fixture.cx,
        named(&tree, "ListBoxOption", "Build Box"),
        gpui::accesskit::Action::Click,
    );
    assert_eq!(fixture.selected(), Some(ServerKey::new("build")));
}

#[gpui::test]
fn a_selected_server_that_leaves_the_list_hands_selection_to_the_first(cx: &mut TestAppContext) {
    let mut fixture = open(Appearance::Dark, snapshot(vec![local(), ssh()]), cx);
    fixture.click("settings-server-option-build");
    fixture.publish(snapshot(vec![local()]));
    assert_eq!(fixture.selected(), Some(ServerKey::new("local")));
}

// Lifecycle ----------------------------------------------------------------------------------

#[gpui::test]
fn stop_asks_first_and_sends_only_when_confirmed(cx: &mut TestAppContext) {
    let mut fixture = open(Appearance::Dark, snapshot(vec![local()]), cx);

    fixture.review_management("settings-server-stop");
    assert!(fixture.modal_open());
    fixture.click("modal-action-settings-server-stop-cancel");
    assert!(!fixture.modal_open());
    assert!(fixture.requests.take().is_empty());

    fixture.review_management("settings-server-stop");
    fixture.click("modal-action-settings-server-stop-confirm");
    assert_eq!(
        fixture.requests.take(),
        vec![ServerRequest::Stop(ServerKey::new("local"))]
    );
}

#[test]
fn the_stop_confirmation_says_who_is_affected_and_that_stopping_lasts() {
    let confirmation = copy::stop_confirmation(&local());
    assert_eq!(confirmation.title, "Stop Studio?");
    assert_eq!(
        confirmation.message,
        "Every Terminal Session on Studio ends for every client. Studio stays stopped, including after its machine restarts, until you start it again."
    );
    assert_eq!(confirmation.confirm, "Stop Server");
    assert!(confirmation.destructive);
}

#[gpui::test]
fn start_sends_at_once_where_this_device_can_manage_the_host(cx: &mut TestAppContext) {
    let stopped_ssh = ServerSummary {
        status: ServerStatus::Stopped,
        ..ssh()
    };
    let stopped_wss = ServerSummary {
        status: ServerStatus::Stopped,
        ..wss()
    };
    let mut fixture = open(
        Appearance::Dark,
        snapshot(vec![stopped_ssh, stopped_wss]),
        cx,
    );

    fixture.click("settings-server-start");
    assert!(!fixture.modal_open());
    assert_eq!(
        fixture.requests.take(),
        vec![ServerRequest::Start(ServerKey::new("build"))]
    );

    // A WSS connection reaches only a running server, so it cannot start one.
    fixture.click("settings-server-option-lab");
    fixture.click("settings-server-start");
    assert!(fixture.requests.take().is_empty());
    assert_eq!(
        fixture.caption(SettingsRowId::ServerStatus).as_deref(),
        Some("It stays stopped until someone starts it on its machine or over SSH.")
    );
}

#[test]
fn status_actions_follow_what_the_route_and_state_allow() {
    use super::{StatusAction, status_action};
    let with = |status: ServerStatus, server: ServerSummary| {
        status_action(&ServerSummary { status, ..server })
    };
    assert_eq!(
        with(ServerStatus::Running, local()),
        Some(StatusAction::Stop { enabled: true })
    );
    assert_eq!(
        with(ServerStatus::Running, ssh()),
        Some(StatusAction::Stop { enabled: true })
    );
    // A WSS connection can't start the server again, so it can't stop it either.
    assert_eq!(
        with(ServerStatus::Running, wss()),
        Some(StatusAction::Stop { enabled: false })
    );
    assert_eq!(
        with(ServerStatus::Stopping, local()),
        Some(StatusAction::Stop { enabled: false })
    );
    assert_eq!(
        with(ServerStatus::Incompatible(IncompatibleSide::Server), ssh()),
        Some(StatusAction::Stop { enabled: true })
    );
    assert_eq!(with(ServerStatus::Offline, ssh()), None);
    assert_eq!(
        with(
            ServerStatus::Updating {
                version: "1.5.0".into()
            },
            local()
        ),
        None
    );
}

#[gpui::test]
fn restart_confirms_that_every_client_loses_its_sessions(cx: &mut TestAppContext) {
    let mut fixture = open(Appearance::Dark, snapshot(vec![local()]), cx);

    fixture.review_management("settings-server-restart");
    fixture.click("modal-action-settings-server-restart-confirm");
    assert_eq!(
        fixture.requests.take(),
        vec![ServerRequest::Restart(ServerKey::new("local"))]
    );

    let confirmation = copy::restart_confirmation(&local());
    assert_eq!(confirmation.confirm, "Restart Server");
    assert_eq!(
        confirmation.message,
        "Every Terminal Session on Studio ends for every client."
    );
    assert_eq!(
        confirmation.detail.as_deref(),
        Some(
            "Studio keeps its identity, its paired devices, and its saved Workspaces, Tabs, and Panes. Each Pane starts a fresh shell when you next use it."
        )
    );
}

#[gpui::test]
fn a_staged_version_offers_an_update_that_names_its_effect(cx: &mut TestAppContext) {
    let staged = ServerSummary {
        staged_version: Some("1.5.0".into()),
        ..staged_server(local())
    };
    let mut fixture = open(Appearance::Dark, snapshot(vec![staged.clone()]), cx);
    assert!(!fixture.rendered("settings-server-restart"));
    assert_eq!(
        fixture.caption(SettingsRowId::ServerVersion).as_deref(),
        Some("Version 1.5.0 is ready. Installing it restarts Studio.")
    );

    fixture.review_management("settings-server-update");
    let assessed = fixture.servers.read_with(fixture.cx, |source, _| {
        source.snapshot().servers[0]
            .installations
            .activation
            .clone()
            .unwrap()
    });
    assert_ne!(
        assessed.reference,
        staged.installations.activation.as_ref().unwrap().reference
    );
    fixture.click("modal-action-settings-server-update-confirm");
    assert_eq!(
        fixture.requests.take(),
        vec![ServerRequest::InstallUpdate {
            server: ServerKey::new("local"),
            version: "1.5.0".into(),
            assessment: assessed,
        }]
    );

    let running = copy::update_confirmation(&staged, "1.5.0");
    assert_eq!(running.confirm, "Update and Restart");
    assert!(running.destructive);
    assert_eq!(
        running.message,
        "Studio restarts to finish the update. Every Terminal Session on Studio ends for every client."
    );
    let stopped = copy::update_confirmation(
        &ServerSummary {
            status: ServerStatus::Stopped,
            ..staged
        },
        "1.5.0",
    );
    assert_eq!(stopped.confirm, "Install Update");
    assert!(!stopped.destructive);
    assert_eq!(
        stopped.message,
        "Studio is stopped and stays stopped after the update."
    );
}

#[test]
fn installing_an_update_needs_host_access_as_start_and_stop_do() {
    use super::{VersionAction, version_action};
    let staged = |server: ServerSummary| ServerSummary {
        status: ServerStatus::Running,
        staged_version: Some("1.5.0".into()),
        ..staged_server(ServerSummary {
            status: ServerStatus::Running,
            ..server
        })
    };
    let update = |enabled| VersionAction::Update {
        version: "1.5.0".into(),
        enabled,
    };
    assert_eq!(version_action(&staged(local())), update(true));
    assert_eq!(version_action(&staged(ssh())), update(true));
    // Activating a release replaces the server's approved installation, which only the host's
    // authorization allows, and a WSS connection can't obtain it.
    assert_eq!(version_action(&staged(wss())), update(false));
    // Restart also requires the selected host's native assessment provider.
    assert_eq!(
        version_action(&ServerSummary {
            status: ServerStatus::Running,
            ..wss()
        }),
        VersionAction::Restart { enabled: false }
    );
}

#[gpui::test]
fn over_wss_an_update_explains_where_it_can_be_installed(cx: &mut TestAppContext) {
    let staged = ServerSummary {
        status: ServerStatus::Running,
        staged_version: Some("1.5.0".into()),
        ..staged_server(ServerSummary {
            status: ServerStatus::Running,
            ..wss()
        })
    };
    let mut fixture = open(Appearance::Dark, snapshot(vec![staged]), cx);

    fixture.review_management("settings-server-update");
    assert!(!fixture.modal_open());
    assert!(fixture.requests.take().is_empty());
    assert_eq!(
        fixture.caption(SettingsRowId::ServerVersion).as_deref(),
        Some("Version 1.5.0 is ready. To install it, connect to Lab over SSH or use its machine.")
    );
}

#[gpui::test]
fn a_confirmation_accepted_after_its_server_changed_sends_nothing(cx: &mut TestAppContext) {
    let stopped_with_update = ServerSummary {
        status: ServerStatus::Stopped,
        staged_version: Some("1.5.0".into()),
        ..staged_server(ServerSummary {
            status: ServerStatus::Stopped,
            ..ssh()
        })
    };
    let mut fixture = open(
        Appearance::Dark,
        snapshot(vec![local(), stopped_with_update.clone()]),
        cx,
    );

    // The connection drops while End All Sessions waits for an answer.
    fixture.review_management("settings-server-end-all-sessions");
    assert!(fixture.modal_open());
    let offline = ServerSummary {
        status: ServerStatus::Offline,
        ..local()
    };
    fixture.publish(snapshot(vec![offline, stopped_with_update.clone()]));
    fixture.click("modal-action-settings-server-end-all-sessions-confirm");
    assert!(fixture.modal_open());
    fixture.click("modal-action-settings-server-confirmation-changed-cancel");
    assert!(fixture.requests.take().is_empty());

    // The update was confirmed for a stopped server, which it would leave stopped. A server that
    // started meanwhile would restart instead, which nobody confirmed.
    fixture.click("settings-server-option-build");
    fixture.review_management("settings-server-update");
    assert!(fixture.modal_open());
    fixture.publish(snapshot(vec![
        local(),
        ServerSummary {
            status: ServerStatus::Running,
            ..stopped_with_update.clone()
        },
    ]));
    fixture.click("modal-action-settings-server-update-confirm");
    assert!(fixture.requests.take().is_empty());
    fixture.click("modal-action-settings-server-confirmation-changed-cancel");

    // Another device turned remote connections on, on other addresses, before this one confirmed.
    fixture.publish(snapshot(vec![local(), stopped_with_update]));
    fixture.click("settings-server-option-local");
    fixture.click("settings-server-remote-access");
    assert!(fixture.modal_open());
    fixture.publish(snapshot(vec![ServerSummary {
        remote_access: Some(RemoteAccess {
            version: spaceterm_core::authority::FieldVersion::new(1),
            enabled: true,
            scope: Some("tailnet".into()),
            scopes: scopes(),
            pending: false,
            endpoints: Vec::new(),
            edit: None,
        }),
        ..local()
    }]));
    fixture.click("modal-action-settings-server-remote-access-enable-confirm");
    assert!(fixture.requests.take().is_empty());
}

#[gpui::test]
fn end_all_sessions_needs_a_live_connection_and_confirms(cx: &mut TestAppContext) {
    let mut fixture = open(Appearance::Dark, snapshot(vec![local(), ssh()]), cx);

    fixture.review_management("settings-server-end-all-sessions");
    fixture.click("modal-action-settings-server-end-all-sessions-confirm");
    assert_eq!(
        fixture.requests.take(),
        vec![ServerRequest::EndAllSessions(ServerKey::new("local"))]
    );

    fixture.click("settings-server-option-build");
    fixture.review_management("settings-server-end-all-sessions");
    assert!(!fixture.modal_open());
    assert!(fixture.requests.take().is_empty());
    assert_eq!(
        fixture
            .caption(SettingsRowId::ServerEndAllSessions)
            .as_deref(),
        Some("Needs a live connection to Build Box.")
    );

    let confirmation = copy::end_all_sessions_confirmation(&local());
    assert_eq!(confirmation.title, "End All Sessions on Studio?");
    assert_eq!(confirmation.confirm, "End All Sessions");
    assert_eq!(
        confirmation.detail.as_deref(),
        Some(
            "Studio keeps running, and its saved Workspaces, Tabs, and Panes remain. Repository files and Git Worktree directories stay on disk."
        )
    );
}

// Pairing ------------------------------------------------------------------------------------

#[gpui::test]
fn unpair_confirms_then_sends_even_when_the_server_is_unreachable(cx: &mut TestAppContext) {
    let mut fixture = open(Appearance::Dark, snapshot(vec![ssh()]), cx);

    fixture.click("settings-server-unpair");
    fixture.click("modal-action-settings-server-unpair-cancel");
    assert!(fixture.requests.take().is_empty());
    fixture.click("settings-server-unpair");
    fixture.click("modal-action-settings-server-unpair-confirm");
    assert_eq!(
        fixture.requests.take(),
        vec![ServerRequest::UnpairThisDevice(ServerKey::new("build"))]
    );
}

#[gpui::test]
fn a_pending_unpair_notice_dismisses_without_cancelling_the_revocation(cx: &mut TestAppContext) {
    let pending = PendingUnpair {
        id: "unpair-1".into(),
        server_name: "Old Box".into(),
        original_pairing: pairing(4, 1),
        operation: operation(4),
        phase: model::PendingUnpairPhase::AwaitingConfirmation(
            model::PendingUnpairReason::Unreachable,
        ),
    };
    let mut fixture = open(
        Appearance::Dark,
        ServersSnapshot {
            servers: vec![local()],
            pending_unpairs: vec![pending.clone()],
        },
        cx,
    );
    assert!(fixture.rendered("settings-server-unpair-notice-unpair-1"));
    let notice = fixture
        .cx
        .debug_bounds("settings-server-unpair-notice-unpair-1")
        .unwrap();
    let list = fixture.cx.debug_bounds("settings-server-list").unwrap();
    assert!(
        notice.bottom() <= list.top(),
        "the notice sits above the list"
    );
    let tree = A11yTree::read(fixture.cx);
    assert!(tree.find_text("Unpair pending for Old Box").is_some());
    assert!(
        tree.find_text(
            "This device's connection to Old Box was removed. The server can't be reached. SpaceTerm keeps asking Old Box to confirm in the background, including after you dismiss this."
        )
        .is_some()
    );

    fixture.click("settings-server-unpair-notice-unpair-1-dismiss");
    assert_eq!(
        fixture.requests.take(),
        vec![ServerRequest::DismissUnpairNotice("unpair-1".into())]
    );
    assert!(!fixture.rendered("settings-server-unpair-notice-unpair-1"));

    // The snapshot that still lists it does not bring it back.
    fixture.publish(ServersSnapshot {
        servers: vec![local(), ssh()],
        pending_unpairs: vec![pending],
    });
    assert!(!fixture.rendered("settings-server-unpair-notice-unpair-1"));
}

// Server Settings ----------------------------------------------------------------------------

#[gpui::test]
fn a_location_change_is_sent_with_the_revision_it_was_made_from(cx: &mut TestAppContext) {
    let mut fixture = open(Appearance::Dark, snapshot(vec![local()]), cx);
    assert_eq!(fixture.field(), LOCATION);
    let legacy = fixture.document().git.worktree_path_template;

    fixture.click("settings-server-worktree-location");
    let input = fixture.window.read_with(fixture.cx, |window, _| {
        window.servers.worktree.input.clone()
    });
    fixture.cx.update(|_, cx| {
        input.update(cx, |input, cx| input.set_value(String::from("~/wt/"), cx));
    });
    fixture.cx.simulate_input("{branch}");
    // Another device's change arrives while this one is typing. The typing stays.
    fixture.publish(snapshot(vec![ServerSummary {
        worktree_location: Some(ServerTextSetting {
            value: "~/elsewhere/{branch}".into(),
            revision: 4,
            edit: None,
        }),
        ..local()
    }]));
    assert_eq!(fixture.field(), "~/wt/{branch}");

    fixture.cx.simulate_keystrokes("enter");
    fixture.cx.run_until_parked();
    assert_eq!(
        fixture.requests.take(),
        vec![ServerRequest::SetWorktreeLocation {
            server: ServerKey::new("local"),
            template: "~/wt/{branch}".into(),
            base_revision: 3,
        }]
    );
    // The server's setting is not this device's Settings Document.
    assert_eq!(fixture.document().git.worktree_path_template, legacy);
    assert_eq!(
        fixture
            .storage
            .document()
            .map(|document| document.git.worktree_path_template),
        Some(legacy)
    );
}

#[gpui::test]
fn a_sent_location_stays_in_the_field_until_the_server_answers(cx: &mut TestAppContext) {
    let mut fixture = open(Appearance::Dark, snapshot(vec![local()]), cx);

    fixture.enter_location("~/wt/{branch}");
    assert_eq!(fixture.requests.take().len(), 1);
    // No answer has been published yet. The field keeps what the person sent.
    assert_eq!(fixture.field(), "~/wt/{branch}");
    assert_eq!(
        fixture
            .caption(SettingsRowId::ServerWorktreeLocation)
            .as_deref(),
        Some("Saving to Studio…")
    );
    // A snapshot that still carries the old value and no answer doesn't take the draft away.
    fixture.publish(snapshot(vec![ServerSummary {
        name: "Studio Mac".into(),
        ..local()
    }]));
    assert_eq!(fixture.field(), "~/wt/{branch}");

    // A refusal keeps the draft through the snapshot's own copy of it.
    fixture.publish(snapshot(vec![ServerSummary {
        worktree_location: Some(location(
            3,
            Some(SettingEdit {
                draft: "~/wt/{branch}".into(),
                state: SettingEditState::Rejected(SettingRejection::Unwritable),
            }),
        )),
        ..local()
    }]));
    assert_eq!(fixture.field(), "~/wt/{branch}");

    // Typing the server's value back drops the change, and the field keeps that value while the
    // server's answer is on its way.
    fixture.enter_location(LOCATION);
    assert_eq!(
        fixture.requests.take(),
        vec![ServerRequest::DiscardWorktreeLocationEdit(ServerKey::new(
            "local"
        ))]
    );
    assert_eq!(fixture.field(), LOCATION);

    // Once the server applies a change, the field follows the server again.
    fixture.enter_location("~/next/{branch}");
    fixture.publish(snapshot(vec![ServerSummary {
        worktree_location: Some(ServerTextSetting {
            value: "~/next/{branch}".into(),
            revision: 4,
            edit: None,
        }),
        ..local()
    }]));
    fixture.publish(snapshot(vec![ServerSummary {
        worktree_location: Some(ServerTextSetting {
            value: "~/later/{branch}".into(),
            revision: 5,
            edit: None,
        }),
        ..local()
    }]));
    assert_eq!(fixture.field(), "~/later/{branch}");
}

#[gpui::test]
fn escape_returns_the_location_to_the_servers_value(cx: &mut TestAppContext) {
    let mut fixture = open(Appearance::Dark, snapshot(vec![local()]), cx);
    fixture.click("settings-server-worktree-location");
    fixture.cx.simulate_input("/more");
    assert_ne!(fixture.field(), LOCATION);

    fixture.cx.simulate_keystrokes("escape");
    fixture.cx.run_until_parked();
    assert_eq!(fixture.field(), LOCATION);
    assert!(fixture.requests.take().is_empty());
}

#[gpui::test]
fn a_rejected_location_keeps_the_draft_and_says_why(cx: &mut TestAppContext) {
    let rejected = |rejection| ServerSummary {
        worktree_location: Some(location(
            3,
            Some(SettingEdit {
                draft: "/srv/{branch}".into(),
                state: SettingEditState::Rejected(rejection),
            }),
        )),
        ..local()
    };
    let mut fixture = open(
        Appearance::Dark,
        snapshot(vec![rejected(SettingRejection::Unwritable)]),
        cx,
    );
    assert_eq!(fixture.field(), "/srv/{branch}");
    assert_eq!(
        fixture
            .caption(SettingsRowId::ServerWorktreeLocation)
            .as_deref(),
        Some("Studio can't create folders there. Choose a location its account can write to.")
    );

    fixture.publish(snapshot(vec![rejected(SettingRejection::InvalidTemplate(
        WorktreePathTemplateError::MissingBranch,
    ))]));
    assert_eq!(fixture.field(), "/srv/{branch}");
    assert_eq!(
        fixture
            .caption(SettingsRowId::ServerWorktreeLocation)
            .as_deref(),
        Some(
            "Studio didn't accept this location. Include {branch}, so each Worktree gets its own folder."
        )
    );
    let tree = A11yTree::read(fixture.cx);
    assert!(
        tree.find_text(
            "Studio didn't accept this location. Include {branch}, so each Worktree gets its own folder."
        )
        .is_some()
    );
}

#[gpui::test]
fn a_location_being_saved_shows_the_draft_and_says_so(cx: &mut TestAppContext) {
    let saving = ServerSummary {
        worktree_location: Some(location(
            3,
            Some(SettingEdit {
                draft: "~/next/{branch}".into(),
                state: SettingEditState::Saving,
            }),
        )),
        ..local()
    };
    let mut fixture = open(Appearance::Dark, snapshot(vec![saving]), cx);
    assert_eq!(fixture.field(), "~/next/{branch}");
    assert_eq!(
        fixture
            .caption(SettingsRowId::ServerWorktreeLocation)
            .as_deref(),
        Some("Saving to Studio…")
    );
}

/// A rejection the server does not classify reads as fixed text, so a server's own message, which
/// may carry a path or a native error, never reaches the window.
#[test]
fn an_unclassified_rejection_reads_as_fixed_text() {
    assert_eq!(
        copy::rejection_caption("Studio", &SettingRejection::NotSaved),
        "Studio couldn't save this location. Try again."
    );
}

#[gpui::test]
fn a_conflict_keeps_the_draft_shows_the_current_value_and_resolves_either_way(
    cx: &mut TestAppContext,
) {
    let conflicted = ServerSummary {
        worktree_location: Some(ServerTextSetting {
            value: "~/theirs/{branch}".into(),
            revision: 9,
            edit: Some(SettingEdit {
                draft: "~/mine/{branch}".into(),
                state: SettingEditState::Conflict,
            }),
        }),
        ..local()
    };
    let mut fixture = open(Appearance::Dark, snapshot(vec![conflicted.clone()]), cx);
    assert_eq!(fixture.field(), "~/mine/{branch}");
    let tree = A11yTree::read(fixture.cx);
    assert!(
        tree.find_text(
            "Another device changed this to \"~/theirs/{branch}\" first. Your change wasn't saved and is still in the field."
        )
        .is_some()
    );

    fixture.click("settings-server-worktree-location-save-mine");
    assert_eq!(
        fixture.requests.take(),
        vec![ServerRequest::SetWorktreeLocation {
            server: ServerKey::new("local"),
            template: "~/mine/{branch}".into(),
            base_revision: 9,
        }]
    );
    // The resent change is on its way, so the choices give way to its progress.
    assert!(!fixture.rendered("settings-server-worktree-location-save-mine"));
    assert_eq!(
        fixture
            .caption(SettingsRowId::ServerWorktreeLocation)
            .as_deref(),
        Some("Saving to Studio…")
    );
    // A snapshot that still carries the conflict it answered has not answered the resent change.
    fixture.publish(snapshot(vec![ServerSummary {
        staged_version: Some("1.5.0".into()),
        ..conflicted.clone()
    }]));
    assert_eq!(fixture.field(), "~/mine/{branch}");
    assert!(!fixture.rendered("settings-server-worktree-location-save-mine"));
    assert_eq!(
        fixture
            .caption(SettingsRowId::ServerWorktreeLocation)
            .as_deref(),
        Some("Saving to Studio…")
    );

    // Another conflict brings the choices back. Discarding shows the server's value at once.
    fixture.publish(snapshot(vec![ServerSummary {
        worktree_location: Some(ServerTextSetting {
            value: "~/third/{branch}".into(),
            revision: 10,
            ..conflicted.worktree_location.clone().unwrap()
        }),
        ..conflicted
    }]));
    fixture.click("settings-server-worktree-location-discard-mine");
    assert_eq!(
        fixture.requests.take(),
        vec![ServerRequest::DiscardWorktreeLocationEdit(ServerKey::new(
            "local"
        ))]
    );
    assert_eq!(fixture.field(), "~/third/{branch}");
    assert!(!fixture.rendered("settings-server-worktree-location-conflict"));

    // Once the server drops the change, the field shows its value.
    fixture.publish(snapshot(vec![ServerSummary {
        worktree_location: Some(ServerTextSetting {
            value: "~/theirs/{branch}".into(),
            revision: 9,
            edit: None,
        }),
        ..local()
    }]));
    assert_eq!(fixture.field(), "~/theirs/{branch}");
    assert!(!fixture.rendered("settings-server-worktree-location-conflict"));
}

#[gpui::test]
fn a_disconnected_server_shows_its_location_without_offering_edits(cx: &mut TestAppContext) {
    let offline = ServerSummary {
        status: ServerStatus::Offline,
        ..local()
    };
    let mut fixture = open(Appearance::Dark, snapshot(vec![offline]), cx);
    assert_eq!(fixture.field(), LOCATION);
    let tree = A11yTree::read(fixture.cx);
    let field = named(&tree, "TextInput", "New Worktree Location");
    assert_eq!(field["aria"]["read_only"], true);
}

#[gpui::test]
fn a_location_no_connection_has_read_reads_as_unknown_instead_of_an_empty_field(
    cx: &mut TestAppContext,
) {
    let mut fixture = open(Appearance::Dark, snapshot(vec![ssh()]), cx);
    assert!(fixture.rendered("settings-server-worktree-location-frame"));
    assert!(!fixture.rendered("settings-server-worktree-location"));
    let tree = A11yTree::read(fixture.cx);
    assert!(tree.find_text("Unknown").is_some());
    assert!(
        tree.with_role("TextInput")
            .into_iter()
            .all(|node| node["aria"]["label"] != "New Worktree Location")
    );
    assert_eq!(
        fixture
            .caption(SettingsRowId::ServerWorktreeLocation)
            .as_deref(),
        Some("Connect to Build Box to change its settings.")
    );
}

// Permissions --------------------------------------------------------------------------------

#[gpui::test]
fn permissions_name_the_servers_own_grants_in_words(cx: &mut TestAppContext) {
    let mut fixture = open(Appearance::Dark, snapshot(vec![local(), ssh()]), cx);
    let tree = A11yTree::read(fixture.cx);
    named(&tree, "List", "System permissions");
    named(&tree, "ListItem", "Microphone: Allowed");
    named(&tree, "ListItem", "Screen recording: Not Set Up");
    named(&tree, "ListItem", "Device control: Not Allowed");
    // Each state reads as a badge, as this device's own permissions do in Privacy.
    for state in [
        "settings-server-permission-microphone-state",
        "settings-server-permission-screen-recording-state",
        "settings-server-permission-accessibility-state",
    ] {
        assert!(fixture.rendered(state), "{state}");
    }
    assert!(tree.find_text("Not Allowed").is_some());
    assert_eq!(
        copy::permission_state(ServerPermissionState::NotNeeded),
        "Not Needed"
    );
    assert_eq!(
        fixture.caption(SettingsRowId::ServerPermissions).as_deref(),
        Some(
            "Programs in Studio's Terminal Sessions use the SpaceTerm Server's own permissions. Permissions you gave this app don't carry over."
        )
    );

    fixture.click("settings-server-option-build");
    assert_eq!(
        fixture.caption(SettingsRowId::ServerPermissions).as_deref(),
        Some("Connect to Build Box to see its permissions.")
    );
}

// Remote access ------------------------------------------------------------------------------

#[gpui::test]
fn remote_access_is_off_by_default_and_turning_it_on_names_the_server_and_scope(
    cx: &mut TestAppContext,
) {
    let mut fixture = open(Appearance::Dark, snapshot(vec![local()]), cx);
    let tree = A11yTree::read(fixture.cx);
    let switch = named(&tree, "Switch", "Allow remote connections");
    assert_eq!(switch["aria"]["toggled"], "False");
    assert_eq!(
        named(&tree, "ComboBox", "Listen on")["aria"]["value"],
        "Local network"
    );

    fixture.click("settings-server-remote-access");
    assert!(fixture.modal_open());
    fixture.click("modal-action-settings-server-remote-access-enable-cancel");
    assert!(fixture.requests.take().is_empty());

    // A scope chosen while remote connections are off waits for them to be turned on.
    let window = fixture.window.clone();
    fixture.cx.update(|gpui_window, cx| {
        window.update(cx, |settings, cx| {
            settings.choose_remote_access_scope(
                &ServerKey::new("local"),
                "tailnet".into(),
                gpui_window,
                cx,
            );
        });
    });
    fixture.cx.run_until_parked();
    assert!(!fixture.modal_open());
    assert!(fixture.requests.take().is_empty());
    assert_eq!(
        fixture
            .caption(SettingsRowId::ServerRemoteAccessScope)
            .as_deref(),
        Some("100.64.0.20. Used when remote connections are on.")
    );

    fixture.click("settings-server-remote-access");
    fixture.click("modal-action-settings-server-remote-access-enable-confirm");
    assert_eq!(
        fixture.requests.take(),
        vec![ServerRequest::EnableRemoteAccess {
            server: ServerKey::new("local"),
            scope: "tailnet".into(),
        }]
    );

    let confirmation = copy::enable_remote_access_confirmation(&local(), &scopes()[1]);
    assert_eq!(confirmation.title, "Allow Remote Connections to Studio?");
    assert_eq!(
        confirmation.message,
        "Studio accepts SpaceTerm connections over the network on Tailnet (100.64.0.20). Paired devices can then connect without SSH."
    );
}

fn remote_access_on() -> Option<RemoteAccess> {
    Some(RemoteAccess {
        version: spaceterm_core::authority::FieldVersion::new(1),
        enabled: true,
        scope: Some("lan".into()),
        scopes: scopes(),
        pending: false,
        endpoints: Vec::new(),
        edit: None,
    })
}

/// A Running WSS server with remote connections on, which this device reaches over them.
fn wss_with_remote_access() -> ServerSummary {
    ServerSummary {
        status: ServerStatus::Running,
        remote_access: remote_access_on(),
        ..wss()
    }
}

#[gpui::test]
fn turning_remote_access_off_asks_because_network_devices_disconnect(cx: &mut TestAppContext) {
    let over_ssh = ServerSummary {
        status: ServerStatus::Running,
        remote_access: remote_access_on(),
        ..ssh()
    };
    let mut fixture = open(
        Appearance::Dark,
        snapshot(vec![over_ssh.clone(), wss_with_remote_access()]),
        cx,
    );

    // Other devices may be connected over the network, so even an SSH route asks first.
    fixture.click("settings-server-remote-access");
    assert!(fixture.modal_open());
    // The confirmation is destructive, so Return doesn't turn remote connections off.
    fixture.press_return();
    assert!(fixture.requests.take().is_empty());
    // Focus enters on Cancel, which Return activates.
    assert!(!fixture.modal_open());

    fixture.click("settings-server-remote-access");
    fixture.click("modal-action-settings-server-remote-access-disable-confirm");
    assert_eq!(
        fixture.requests.take(),
        vec![ServerRequest::DisableRemoteAccess(ServerKey::new("build"))]
    );

    fixture.click("settings-server-option-lab");
    fixture.click("settings-server-remote-access");
    assert!(fixture.modal_open());
    fixture.click("modal-action-settings-server-remote-access-disable-confirm");
    assert_eq!(
        fixture.requests.take(),
        vec![ServerRequest::DisableRemoteAccess(ServerKey::new("lab"))]
    );

    let over_ssh = copy::disable_remote_access_confirmation(&over_ssh);
    assert!(over_ssh.destructive);
    assert_eq!(
        over_ssh.message,
        "Build Box stops accepting network connections. Devices connected to Build Box over the network disconnect."
    );
    assert_eq!(
        over_ssh.detail.as_deref(),
        Some("Terminal Sessions on Build Box keep running.")
    );
    let over_wss = copy::disable_remote_access_confirmation(&wss_with_remote_access());
    assert!(over_wss.destructive);
    assert_eq!(
        over_wss.message,
        "Lab stops accepting network connections. Devices connected to Lab over the network disconnect, including this one."
    );
    assert_eq!(
        over_wss.detail.as_deref(),
        Some(
            "Terminal Sessions on Lab keep running. To turn remote connections on again, connect to Lab over SSH or on its machine."
        )
    );
}

#[gpui::test]
fn moving_remote_access_asks_and_warns_a_wss_device_that_it_may_disconnect(
    cx: &mut TestAppContext,
) {
    let mut fixture = open(
        Appearance::Dark,
        snapshot(vec![wss_with_remote_access()]),
        cx,
    );
    let choose = |fixture: &mut Fixture<'_>| {
        let window = fixture.window.clone();
        fixture.cx.update(|gpui_window, cx| {
            window.update(cx, |settings, cx| {
                settings.choose_remote_access_scope(
                    &ServerKey::new("lab"),
                    "tailnet".into(),
                    gpui_window,
                    cx,
                );
            });
        });
        fixture.cx.run_until_parked();
    };

    choose(&mut fixture);
    assert!(fixture.modal_open());
    // The confirmation is destructive, so Return doesn't move remote connections.
    fixture.press_return();
    assert!(fixture.requests.take().is_empty());
    // Focus enters on Cancel, which Return activates.
    assert!(!fixture.modal_open());

    choose(&mut fixture);
    fixture.click("modal-action-settings-server-remote-access-scope-confirm");
    assert_eq!(
        fixture.requests.take(),
        vec![ServerRequest::EnableRemoteAccess {
            server: ServerKey::new("lab"),
            scope: "tailnet".into(),
        }]
    );

    let over_wss = copy::change_scope_confirmation(&wss_with_remote_access(), &scopes()[1]);
    assert!(over_wss.destructive);
    assert_eq!(over_wss.title, "Listen on Tailnet?");
    assert_eq!(
        over_wss.message,
        "Lab accepts SpaceTerm connections over the network on 100.64.0.20 instead. Devices connected on other addresses disconnect."
    );
    assert_eq!(
        over_wss.detail.as_deref(),
        Some(
            "This device connects to Lab over the network. If its address isn't in Tailnet, it disconnects too. Terminal Sessions on Lab keep running."
        )
    );
    let local_scope = copy::change_scope_confirmation(&local(), &scopes()[1]);
    assert!(local_scope.destructive);
    assert_eq!(
        local_scope.detail.as_deref(),
        Some("Terminal Sessions on Studio keep running.")
    );
}

#[gpui::test]
fn a_pending_remote_access_change_waits_for_the_server(cx: &mut TestAppContext) {
    let pending = ServerSummary {
        remote_access: Some(RemoteAccess {
            version: spaceterm_core::authority::FieldVersion::new(1),
            enabled: false,
            scope: None,
            scopes: scopes(),
            pending: true,
            endpoints: Vec::new(),
            edit: None,
        }),
        ..local()
    };
    let mut fixture = open(Appearance::Dark, snapshot(vec![pending]), cx);
    fixture.click("settings-server-remote-access");
    assert!(!fixture.modal_open());
    assert!(fixture.requests.take().is_empty());
    assert_eq!(
        fixture
            .caption(SettingsRowId::ServerRemoteAccess)
            .as_deref(),
        Some("Waiting for Studio to confirm…")
    );
}

#[gpui::test]
fn remote_access_turns_off_after_the_server_stops_offering_its_addresses(cx: &mut TestAppContext) {
    let stranded = ServerSummary {
        remote_access: Some(RemoteAccess {
            version: spaceterm_core::authority::FieldVersion::new(1),
            enabled: true,
            scope: Some("lan".into()),
            scopes: Vec::new(),
            pending: false,
            endpoints: Vec::new(),
            edit: None,
        }),
        ..local()
    };
    let mut fixture = open(Appearance::Dark, snapshot(vec![stranded]), cx);
    assert_eq!(
        fixture
            .caption(SettingsRowId::ServerRemoteAccessScope)
            .as_deref(),
        Some(
            "Studio no longer offers the addresses remote connections use. Turn remote connections off, or wait for Studio to offer them again."
        )
    );

    // Turning remote connections off needs no addresses.
    fixture.click("settings-server-remote-access");
    assert!(fixture.modal_open());
    fixture.click("modal-action-settings-server-remote-access-disable-confirm");
    assert_eq!(
        fixture.requests.take(),
        vec![ServerRequest::DisableRemoteAccess(ServerKey::new("local"))]
    );
}

#[gpui::test]
fn listen_on_never_presents_other_addresses_as_the_ones_in_use(cx: &mut TestAppContext) {
    let moved = ServerSummary {
        remote_access: Some(RemoteAccess {
            version: spaceterm_core::authority::FieldVersion::new(1),
            enabled: true,
            scope: Some("wifi".into()),
            scopes: scopes(),
            pending: false,
            endpoints: Vec::new(),
            edit: None,
        }),
        ..local()
    };
    let mut fixture = open(Appearance::Dark, snapshot(vec![moved]), cx);
    assert_eq!(
        fixture
            .caption(SettingsRowId::ServerRemoteAccessScope)
            .as_deref(),
        Some(
            "Studio no longer offers the addresses remote connections use. Choose other addresses, or turn remote connections off."
        )
    );
    // The picker selects none of the offered scopes and shows its "Unavailable" prompt.
    let tree = A11yTree::read(fixture.cx);
    assert!(named(&tree, "ComboBox", "Listen on")["aria"]["value"].is_null());

    // Choosing offered addresses moves remote connections there, and asks first.
    let window = fixture.window.clone();
    fixture.cx.update(|gpui_window, cx| {
        window.update(cx, |settings, cx| {
            settings.choose_remote_access_scope(
                &ServerKey::new("local"),
                "lan".into(),
                gpui_window,
                cx,
            );
        });
    });
    fixture.cx.run_until_parked();
    fixture.click("modal-action-settings-server-remote-access-scope-confirm");
    assert_eq!(
        fixture.requests.take(),
        vec![ServerRequest::EnableRemoteAccess {
            server: ServerKey::new("local"),
            scope: "lan".into(),
        }]
    );
}

#[gpui::test]
fn a_server_without_addresses_says_why_remote_access_cannot_turn_on(cx: &mut TestAppContext) {
    let unoffered = ServerSummary {
        remote_access: Some(RemoteAccess {
            version: spaceterm_core::authority::FieldVersion::new(1),
            enabled: false,
            scope: None,
            scopes: Vec::new(),
            pending: false,
            endpoints: Vec::new(),
            edit: None,
        }),
        ..local()
    };
    let mut fixture = open(Appearance::Dark, snapshot(vec![unoffered]), cx);
    fixture.click("settings-server-remote-access");
    assert!(!fixture.modal_open());
    assert!(fixture.requests.take().is_empty());
    assert_eq!(
        fixture
            .caption(SettingsRowId::ServerRemoteAccessScope)
            .as_deref(),
        Some("Studio offers no addresses for remote connections.")
    );
}

// Search and layout --------------------------------------------------------------------------

#[gpui::test]
fn a_server_search_opens_the_section_with_the_list_above_the_rows_it_found(
    cx: &mut TestAppContext,
) {
    let mut fixture = open(Appearance::Dark, snapshot(vec![local()]), cx);
    fixture.click("settings-navigation-settings-section-interface");
    let window = fixture.window.clone();
    fixture.cx.update(|_, cx| {
        window.update(cx, |window, cx| {
            let search = window.search.clone();
            search.update(cx, |search, cx| search.set_value("end all sessions", cx));
        });
    });
    fixture.cx.run_until_parked();

    let (section, rows) = fixture.window.read_with(fixture.cx, |window, _| {
        (
            window.active_section,
            window.rows_for(SettingsSectionId::Servers),
        )
    });
    assert_eq!(section, SettingsSectionId::Servers);
    assert_eq!(rows.first(), Some(&SettingsRowId::ServerList));
    assert!(rows.contains(&SettingsRowId::ServerEndAllSessions));
    assert!(fixture.rendered("settings-server-end-all-sessions"));
    let list = fixture.cx.debug_bounds("settings-server-list").unwrap();
    let row = fixture
        .cx
        .debug_bounds("settings-row-server-end-all-sessions")
        .unwrap();
    assert!(list.bottom() <= row.top());
}

#[gpui::test]
fn the_list_leads_even_when_the_search_ranks_a_server_row_above_it(cx: &mut TestAppContext) {
    let mut fixture = open(Appearance::Dark, snapshot(vec![local()]), cx);
    // "ver" starts the word Version but sits inside the word server, so Version ranks higher.
    fixture.search("ver");
    let ranked = fixture
        .window
        .read_with(fixture.cx, |window, _| window.matching_rows());
    let position = |row| ranked.iter().position(|ranked| *ranked == row);
    assert!(position(SettingsRowId::ServerVersion) < position(SettingsRowId::ServerList));

    let rows = fixture.rows();
    assert_eq!(rows.first(), Some(&SettingsRowId::ServerList));
    assert_eq!(
        rows.iter()
            .filter(|row| **row == SettingsRowId::ServerList)
            .count(),
        1
    );
    let list = fixture.cx.debug_bounds("settings-server-list").unwrap();
    let heading = fixture.cx.debug_bounds("settings-server-heading").unwrap();
    let version = fixture
        .cx
        .debug_bounds("settings-row-server-version")
        .unwrap();
    assert!(list.bottom() <= heading.top());
    assert!(heading.bottom() <= version.top());
}

#[gpui::test]
fn without_servers_a_search_finds_only_the_list(cx: &mut TestAppContext) {
    let fixture = open(Appearance::Dark, ServersSnapshot::default(), cx);
    let window = fixture.window.clone();
    fixture.cx.update(|_, cx| {
        window.update(cx, |window, cx| {
            let search = window.search.clone();
            search.update(cx, |search, cx| search.set_value("remote access", cx));
        });
    });
    fixture.cx.run_until_parked();
    let rows = fixture
        .window
        .read_with(fixture.cx, |window, _| window.matching_rows());
    assert!(
        rows.iter()
            .all(|row| row.descriptor().section != SettingsSectionId::Servers
                || *row == SettingsRowId::ServerList)
    );
}

fn assert_rows_share_one_right_edge(fixture: &mut Fixture<'_>) {
    let rows = fixture.window.read_with(fixture.cx, |window, _| {
        window.rows_for(SettingsSectionId::Servers)
    });
    assert_eq!(rows.len(), 11);
    let mut right = None;
    for row in rows {
        let bounds = fixture
            .cx
            .debug_bounds(row.descriptor().selector)
            .unwrap_or_else(|| panic!("{row:?} should render"));
        match right {
            Some(edge) => assert_eq!(bounds.right(), edge, "{row:?}"),
            None => right = Some(bounds.right()),
        }
    }
}

#[gpui::test]
fn the_section_lays_out_in_light_appearance(cx: &mut TestAppContext) {
    let mut fixture = open(Appearance::Light, snapshot(vec![local(), ssh(), wss()]), cx);
    assert_rows_share_one_right_edge(&mut fixture);
    assert!(fixture.rendered("settings-server-heading"));
}

#[gpui::test]
fn the_section_lays_out_in_dark_appearance(cx: &mut TestAppContext) {
    let mut fixture = open(Appearance::Dark, snapshot(vec![local(), ssh(), wss()]), cx);
    assert_rows_share_one_right_edge(&mut fixture);
}

#[test]
fn no_settings_reset_reaches_a_server_row() {
    for row in [
        SettingsRowId::ServerList,
        SettingsRowId::ServerStatus,
        SettingsRowId::ServerVersion,
        SettingsRowId::ServerConnection,
        SettingsRowId::ServerEndAllSessions,
        SettingsRowId::ServerWorktreeLocation,
        SettingsRowId::ServerPermissions,
        SettingsRowId::ServerRemoteAccess,
        SettingsRowId::ServerRemoteAccessScope,
        SettingsRowId::ServerPairedDevices,
        SettingsRowId::ServerUnpair,
    ] {
        for appearance in [Appearance::Light, Appearance::Dark] {
            assert_eq!(row.reset_target(appearance), None, "{row:?}");
        }
    }
}

#[gpui::test]
fn a_confirmation_cannot_target_a_repaired_server_under_the_same_ui_key(cx: &mut TestAppContext) {
    let mut fixture = open(Appearance::Dark, snapshot(vec![local()]), cx);
    fixture.review_management("settings-server-stop");
    assert!(fixture.modal_open());
    let replacement = ServerSummary {
        pairing: pairing(1, 2),
        ..local()
    };
    fixture.publish(snapshot(vec![replacement]));
    fixture.click("modal-action-settings-server-stop-confirm");
    assert!(fixture.requests.take().is_empty());
}

#[gpui::test]
fn a_worktree_draft_cannot_follow_a_repaired_server_under_the_same_ui_key(cx: &mut TestAppContext) {
    let mut fixture = open(Appearance::Dark, snapshot(vec![local()]), cx);
    fixture.click("settings-server-worktree-location");
    fixture.cx.simulate_keystrokes("cmd-a");
    fixture.cx.simulate_input("~/old-draft/{branch}");
    let replacement = ServerSummary {
        pairing: pairing(1, 2),
        worktree_location: Some(ServerTextSetting {
            value: "~/new-pairing/{branch}".into(),
            revision: 1,
            edit: None,
        }),
        ..local()
    };
    fixture.publish(snapshot(vec![replacement]));
    fixture.cx.simulate_keystrokes("enter");
    fixture.cx.run_until_parked();
    assert!(fixture.requests.take().is_empty());
    assert_eq!(fixture.field(), "~/new-pairing/{branch}");
}

#[gpui::test]
fn an_unanswered_worktree_change_cannot_follow_a_repaired_server(cx: &mut TestAppContext) {
    let mut fixture = open(Appearance::Dark, snapshot(vec![local()]), cx);
    fixture.enter_location("~/old-request/{branch}");
    assert_eq!(fixture.requests.take().len(), 1);
    fixture.publish(snapshot(vec![ServerSummary {
        pairing: pairing(1, 2),
        ..local()
    }]));
    assert_eq!(fixture.field(), LOCATION);
    assert_ne!(
        fixture
            .caption(SettingsRowId::ServerWorktreeLocation)
            .as_deref(),
        Some("Saving to Studio…")
    );
}

#[gpui::test]
fn importing_and_resetting_client_settings_never_reseed_an_existing_server(
    cx: &mut TestAppContext,
) {
    use crate::ui::sidebar_window::SidebarOwner as _;
    let original = snapshot(vec![local()]);
    let mut fixture = open(Appearance::Dark, original.clone(), cx);
    let mut imported = SettingsDocument::default();
    imported.git.worktree_path_template = "~/client-import/{branch}".into();
    let bytes = crate::settings::export_settings(&imported)
        .unwrap()
        .into_bytes();
    let window = fixture.window.clone();
    fixture.cx.update(|native, cx| {
        window.update(cx, |settings, cx| {
            settings.select_section(SettingsSectionId::Advanced, cx);
            settings.finish_settings_import(Ok(bytes), native, cx);
        });
    });
    fixture.cx.run_until_parked();
    fixture.click("modal-action-settings-import-confirm");
    assert_eq!(
        fixture.document().git.worktree_path_template,
        "~/client-import/{branch}"
    );
    assert!(fixture.requests.take().is_empty());
    fixture.cx.update(|_, cx| {
        assert_eq!(fixture.servers.read(cx).snapshot(), &original);
    });
    fixture.click("settings-reset-all");
    fixture.click("modal-action-settings-reset-all-confirm");
    assert_eq!(
        fixture.document().git.worktree_path_template,
        crate::worktrees::path_template::DEFAULT_WORKTREE_PATH_TEMPLATE
    );
    assert!(fixture.requests.take().is_empty());
    fixture.cx.update(|_, cx| {
        assert_eq!(fixture.servers.read(cx).snapshot(), &original);
    });
    fixture.cx.update(|_, cx| {
        window.update(cx, |settings, cx| {
            settings.select_section(SettingsSectionId::Servers, cx)
        });
    });
    fixture.cx.run_until_parked();
    assert_eq!(fixture.field(), LOCATION);
}

#[gpui::test]
fn desktop_server_settings_from_pane_keeps_the_owning_machine_through_late_publication(
    cx: &mut TestAppContext,
) {
    let wanted = ssh();
    let mut fixture = open(Appearance::Dark, snapshot(vec![local()]), cx);
    fixture.cx.update(|window, cx| {
        cx.set_global(crate::ui::settings_window::OpenSettingsWindow(
            window.window_handle().downcast::<SettingsWindow>().unwrap(),
        ));
        crate::ui::settings_window::open_for_server(wanted.pairing.parts().server, cx);
    });
    fixture.cx.run_until_parked();
    assert_eq!(fixture.selected(), None);
    fixture.publish(snapshot(vec![local(), wanted.clone()]));
    assert_eq!(fixture.selected(), Some(wanted.key));
    fixture.click("settings-server-option-local");
    assert_eq!(fixture.selected(), Some(local().key));
}

#[gpui::test]
fn management_reads_an_assessment_before_opening_a_destructive_confirmation(
    cx: &mut TestAppContext,
) {
    let mut fixture = open(Appearance::Dark, snapshot(vec![local()]), cx);
    fixture.click("settings-server-stop");
    assert_eq!(
        fixture.requests.take(),
        vec![ServerRequest::AssessStop(ServerKey::new("local"))]
    );
    assert!(!fixture.modal_open());
}

#[gpui::test]
fn an_offline_unsent_location_draft_survives_switching_servers(cx: &mut TestAppContext) {
    let mut fixture = open(Appearance::Dark, snapshot(vec![local(), ssh()]), cx);
    fixture.click("settings-server-worktree-location");
    let input = fixture.window.read_with(fixture.cx, |settings, _| {
        settings.servers.worktree.input.clone()
    });
    fixture
        .cx
        .update(|_, cx| input.update(cx, |input, cx| input.set_value("~/unsent/{branch}", cx)));
    fixture.publish(snapshot(vec![
        ServerSummary {
            status: ServerStatus::Offline,
            ..local()
        },
        ssh(),
    ]));
    fixture.click("settings-server-option-build");
    fixture.click("settings-server-option-local");
    assert_eq!(fixture.field(), "~/unsent/{branch}");
    assert!(fixture.requests.take().is_empty());
}

#[gpui::test]
fn a_typed_management_failure_is_visible_and_announced(cx: &mut TestAppContext) {
    let mut server = local();
    server.management.failure =
        Some(spaceterm_client::management_cli::ManagementFailure::StaleAssessment);
    let mut fixture = open(Appearance::Dark, snapshot(vec![server]), cx);
    assert!(fixture.rendered("settings-server-management-status"));
    let tree = A11yTree::read(fixture.cx);
    assert!(tree.with_role("Status").iter().any(|node| {
        node["aria"]["label"]
            .as_str()
            .is_some_and(|label| label.contains("Review"))
    }));
}

#[gpui::test]
fn switching_servers_cancels_a_pending_management_review(cx: &mut TestAppContext) {
    let mut fixture = open(Appearance::Dark, snapshot(vec![local(), ssh()]), cx);
    fixture.click("settings-server-stop");
    assert_eq!(
        fixture.requests.take(),
        vec![ServerRequest::AssessStop(ServerKey::new("local"))]
    );
    fixture.click("settings-server-option-build");
    let mut studio = local();
    studio.management.stop = Some(host_assessment(
        spaceterm_host::account_service::ServiceAction::Stop,
        1,
    ));
    fixture.publish(snapshot(vec![studio, ssh()]));
    assert!(!fixture.modal_open());
    assert!(fixture.requests.take().is_empty());
}

#[gpui::test]
fn a_changed_exact_update_candidate_requires_another_confirmation_with_cancel_focused(
    cx: &mut TestAppContext,
) {
    let mut server = staged_server(local());
    server.staged_version = Some("1.5.0".into());
    let mut fixture = open(Appearance::Dark, snapshot(vec![server.clone()]), cx);
    fixture.review_management("settings-server-update");
    server
        .installations
        .staged
        .as_mut()
        .unwrap()
        .artifact_sha256 = [9; 32];
    fixture.publish(snapshot(vec![server]));
    fixture.click("modal-action-settings-server-update-confirm");
    assert!(fixture.requests.take().is_empty());
    assert!(fixture.modal_open());
    let tree = A11yTree::read(fixture.cx);
    assert_eq!(tree.focused().unwrap()["aria"]["label"], "Cancel");
    fixture.press_return();
    assert!(!fixture.modal_open());
}

fn host_assessment(
    action: spaceterm_host::account_service::ServiceAction,
    identity: u8,
) -> model::HostAssessment {
    model::HostAssessment {
        reference: spaceterm_client::management_cli::confirmation::AssessmentToken::new(format!(
            "host-{identity}"
        ))
        .unwrap(),
        request: spaceterm_host::native_deployment::contracts::NativeRequestId::new([identity; 16])
            .unwrap(),
        action,
        affected_sessions: 2,
    }
}

fn end_all_assessment(
    server: &ServerSummary,
) -> spaceterm_client::management_cli::confirmation::ManagementAssessment {
    use spaceterm_client::management_cli::confirmation::{
        AssessmentEvidence, AssessmentToken, ManagementAction, ManagementAssessment,
    };
    use spaceterm_core::{
        observations::MonotonicTick,
        operations::{
            AssessmentRef, AssessmentSummary, AssessmentTarget, CloseAssessment, RequestDigest,
        },
    };
    let id = server.pairing.parts().server;
    let original = spaceterm_core::operations::OperationId::new(
        spaceterm_core::identity::OperationStreamId::new(
            spaceterm_core::identity::OperationStreamKey::new(id, 1).unwrap(),
            server.pairing.parts().pairing,
            spaceterm_core::identity::ClientId::new(id, 1).unwrap(),
        )
        .unwrap(),
        spaceterm_core::operations::OperationSequence::new(1).unwrap(),
    );
    let assessed = CloseAssessment::new(
        AssessmentRef {
            operation: original,
            digest: RequestDigest::from_bytes([1; 32]),
        },
        server.runtime_epoch.unwrap(),
        AssessmentSummary::new(AssessmentTarget::EndAllSessions(id), 1, 1, 2, 2, true).unwrap(),
    )
    .unwrap();
    ManagementAssessment::new(
        AssessmentToken::new("end-all-1".into()).unwrap(),
        ManagementAction::EndAllSessions { server: id },
        AssessmentEvidence::EndAllSessions(assessed),
        MonotonicTick::new(1),
        MonotonicTick::new(30_001),
    )
    .unwrap()
}

#[gpui::test]
fn a_remote_access_conflict_retains_the_exact_draft_and_shows_current_addresses(
    cx: &mut TestAppContext,
) {
    let mut server = local();
    let remote = server.remote_access.as_mut().unwrap();
    remote.enabled = true;
    remote.version = spaceterm_core::authority::FieldVersion::new(4);
    remote.scope = Some("tailnet".into());
    remote.endpoints = vec!["100.64.0.4:443".parse().unwrap()];
    remote.edit = Some(model::RemoteAccessEdit {
        draft: spaceterm_core::remote_access::RemoteAccessPolicy::disabled(
            server.pairing.parts().server,
        ),
        base_version: spaceterm_core::authority::FieldVersion::new(3),
        state: SettingEditState::Conflict,
    });
    let mut fixture = open(Appearance::Dark, snapshot(vec![server]), cx);
    fixture.search("remote connections");
    assert!(fixture.rendered("settings-server-remote-access-conflict"));
    fixture.click("settings-server-remote-access-save-mine");
    assert!(fixture.modal_open());
    assert!(fixture.requests.take().is_empty());
    let tree = A11yTree::read(fixture.cx);
    assert_eq!(tree.focused().unwrap()["aria"]["label"], "Cancel");
    fixture.click("modal-action-settings-server-remote-access-disable-confirm");
    assert_eq!(
        fixture.requests.take(),
        vec![ServerRequest::DisableRemoteAccess(ServerKey::new("local"))]
    );
}

#[gpui::test]
fn pending_revocation_has_only_dismissal_and_restores_search_focus_without_servers(
    cx: &mut TestAppContext,
) {
    let mut fixture = open(
        Appearance::Light,
        ServersSnapshot {
            servers: vec![],
            pending_unpairs: vec![PendingUnpair {
                id: "pending-only".into(),
                server_name: "Removed server".into(),
                original_pairing: pairing(4, 1),
                operation: operation(4),
                phase: model::PendingUnpairPhase::AwaitingConfirmation(
                    model::PendingUnpairReason::Incompatible,
                ),
            }],
        },
        cx,
    );
    for selector in [
        "settings-server-start",
        "settings-server-stop",
        "settings-server-restart",
        "settings-server-update",
        "settings-server-end-all-sessions",
        "settings-server-unpair",
    ] {
        assert!(!fixture.rendered(selector));
    }
    fixture.click("settings-server-unpair-notice-pending-only-dismiss");
    assert_eq!(
        fixture.requests.take(),
        vec![ServerRequest::DismissUnpairNotice("pending-only".into())]
    );
    let tree = A11yTree::read(fixture.cx);
    assert_eq!(tree.focused().unwrap()["aria"]["label"], "Search settings");
}

#[gpui::test]
fn client_preferences_save_while_all_servers_are_offline(cx: &mut TestAppContext) {
    let mut fixture = open(
        Appearance::Light,
        snapshot(vec![ServerSummary {
            status: ServerStatus::Offline,
            ..local()
        }]),
        cx,
    );
    fixture.click("settings-navigation-settings-section-interface");
    fixture.click("settings-sidebar-default-state-collapsed");
    fixture
        .cx
        .executor()
        .advance_clock(crate::ui::settings_window::editor::COMMIT_DELAY * 2);
    fixture.cx.run_until_parked();
    assert_eq!(
        fixture.storage.document().unwrap().sidebar.default_state,
        crate::settings::sidebar::SidebarState::Collapsed
    );
    assert!(fixture.requests.take().is_empty());
}

#[gpui::test]
fn a_changed_native_session_scope_is_not_accepted_under_an_unchanged_token(
    cx: &mut TestAppContext,
) {
    let mut fixture = open(Appearance::Dark, snapshot(vec![local()]), cx);
    fixture.review_management("settings-server-stop");
    let mut current = local();
    let mut changed = host_assessment(spaceterm_host::account_service::ServiceAction::Stop, 1);
    changed.affected_sessions += 1;
    current.management.stop = Some(changed);
    fixture.publish(snapshot(vec![current]));
    fixture.click("modal-action-settings-server-stop-confirm");
    assert!(fixture.requests.take().is_empty());
    assert!(fixture.modal_open());
    assert_eq!(
        A11yTree::read(fixture.cx).focused().unwrap()["aria"]["label"],
        "Cancel"
    );
}

#[gpui::test]
fn known_bind_endpoints_are_shown_without_inventing_an_offered_scope(cx: &mut TestAppContext) {
    let mut server = local();
    let remote = server.remote_access.as_mut().unwrap();
    remote.enabled = true;
    remote.scopes.clear();
    remote.endpoints = vec!["[::1]:4444".parse().unwrap()];
    let mut fixture = open(Appearance::Dark, snapshot(vec![server]), cx);
    assert!(
        fixture
            .caption(SettingsRowId::ServerRemoteAccessScope)
            .unwrap()
            .contains("[::1]:4444")
    );
    assert!(!fixture.rendered("settings-server-remote-access-scope-lan"));
}

#[gpui::test]
fn server_update_click_assesses_before_confirmation_for_running_and_stopped(
    cx: &mut TestAppContext,
) {
    for status in [ServerStatus::Running, ServerStatus::Stopped] {
        let mut server = staged_server(ServerSummary {
            status,
            staged_version: Some("1.5.0".into()),
            ..local()
        });
        server.installations.activation = None;
        let mut fixture = open(Appearance::Dark, snapshot(vec![server.clone()]), cx);
        fixture.click("settings-server-update");
        assert_eq!(
            fixture.requests.take(),
            vec![ServerRequest::AssessInstallUpdate(server.key.clone())]
        );
        assert!(
            !fixture.modal_open(),
            "native assessment must precede confirmation"
        );
        let assessed = staged_server(server.clone());
        fixture.publish(snapshot(vec![assessed.clone()]));
        assert!(fixture.modal_open());
        fixture.click("modal-action-settings-server-update-confirm");
        assert_eq!(
            fixture.requests.take(),
            vec![ServerRequest::InstallUpdate {
                server: server.key,
                version: "1.5.0".into(),
                assessment: assessed.installations.activation.unwrap()
            }]
        );
    }
}

#[gpui::test]
fn stopped_pane_recovery_opens_settings_for_its_owning_server_without_effects(
    cx: &mut TestAppContext,
) {
    struct Movement;
    impl crate::platform::window_movement::WindowMovementFactory for Movement {
        fn create(
            &self,
        ) -> Rc<dyn crate::platform::window_movement::OperatingSystemWindowDragPlatform> {
            Rc::new(RecordingOperatingSystemWindowDragPlatform::default())
        }
    }
    cx.update(|cx| {
        appearance_runtime::install(
            crate::settings::Settings::load(MemoryStorage::with_document(
                &SettingsDocument::default(),
            )),
            Rc::new(RecordingAppearancePlatform::default()),
            cx,
        )
        .unwrap();
        super::super::configure_window_chrome(Rc::new(Movement), Default::default(), None, cx);
    });
    let window = crate::ui::workspace_manager::tests::stopped_pane_settings(cx);
    window
        .read_with(cx, |settings, _| {
            assert_eq!(settings.active_section, SettingsSectionId::Servers);
            assert_eq!(
                settings.servers.requested,
                Some(crate::domain::server_view_testing::server(
                    crate::domain::server_view_testing::B
                ))
            );
        })
        .unwrap();
}
