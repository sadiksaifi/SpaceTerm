//! The Servers section, driven through published server snapshots and a recording request sink.

use std::cell::RefCell;
use std::rc::Rc;

use gpui::{App, Entity, Modifiers, TestAppContext, VisualTestContext};
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

const LOCATION: &str = "~/Worktrees/{repository}/{branch}";

/// Records every request the section sends.
#[derive(Default)]
struct Recorder(RefCell<Vec<ServerRequest>>);

impl ServerRequests for Recorder {
    fn send(&self, request: ServerRequest, _: &mut App) {
        self.0.borrow_mut().push(request);
    }
}

impl Recorder {
    fn take(&self) -> Vec<ServerRequest> {
        self.0.borrow_mut().drain(..).collect()
    }
}

struct Fixture<'a> {
    window: Entity<SettingsWindow>,
    servers: Entity<Servers>,
    requests: Rc<Recorder>,
    storage: std::sync::Arc<MemoryStorage>,
    cx: &'a mut VisualTestContext,
}

impl Fixture<'_> {
    fn publish(&mut self, snapshot: ServersSnapshot) {
        let servers = self.servers.clone();
        self.cx
            .update(|_, cx| servers.update(cx, |servers, cx| servers.publish(snapshot, cx)));
        self.cx.run_until_parked();
    }

    fn click(&mut self, selector: &str) {
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

    fn modal_open(&mut self) -> bool {
        self.cx
            .update(|window, cx| spaceterm_ui::window_modal_is_open(window, cx))
    }

    fn document(&mut self) -> SettingsDocument {
        self.window
            .read_with(self.cx, |window, _| window.editor.document().clone())
    }
}

/// Opens Settings on the Servers section with `snapshot` published.
fn open(appearance: Appearance, snapshot: ServersSnapshot, cx: &mut TestAppContext) -> Fixture<'_> {
    let storage = MemoryStorage::with_document(&SettingsDocument::default());
    let settings = crate::settings::Settings::load(storage.clone());
    let platform = RecordingAppearancePlatform::default();
    platform.set_system_appearance(Some(appearance));
    let requests = Rc::new(Recorder::default());
    let servers = cx.update(|cx| {
        appearance_runtime::install(settings, Rc::new(platform), cx)
            .expect("appearance runtime should install");
        crate::ui::init(cx).expect("UI initialization should succeed");
        let servers = model::install(requests.clone(), cx);
        servers.update(cx, |servers, cx| servers.publish(snapshot, cx));
        servers
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
        },
        BindScope {
            id: "tailnet".into(),
            label: "Tailnet".into(),
            addresses: "100.64.0.20".into(),
        },
    ]
}

fn local() -> ServerSummary {
    ServerSummary {
        key: ServerKey::new("local"),
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
            enabled: false,
            scope: None,
            scopes: scopes(),
            pending: false,
        }),
    }
}

fn ssh() -> ServerSummary {
    ServerSummary {
        key: ServerKey::new("build"),
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

    // The selected server's rows sit under its name.
    let heading = named(&tree, "Heading", "Studio");
    assert_eq!(heading["aria"]["level"], 2);
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
        10
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

    fixture.click("settings-server-stop");
    assert!(fixture.modal_open());
    fixture.click("modal-action-settings-server-stop-cancel");
    assert!(!fixture.modal_open());
    assert!(fixture.requests.take().is_empty());

    fixture.click("settings-server-stop");
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
        with(ServerStatus::Running, wss()),
        Some(StatusAction::Stop { enabled: true })
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

    fixture.click("settings-server-restart");
    fixture.click("modal-action-settings-server-restart-confirm");
    assert_eq!(
        fixture.requests.take(),
        vec![ServerRequest::Restart(ServerKey::new("local"))]
    );

    let confirmation = copy::restart_confirmation(&local());
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
        ..local()
    };
    let mut fixture = open(Appearance::Dark, snapshot(vec![staged.clone()]), cx);
    assert!(!fixture.rendered("settings-server-restart"));
    assert_eq!(
        fixture.caption(SettingsRowId::ServerVersion).as_deref(),
        Some("Version 1.5.0 is ready. Installing it restarts Studio.")
    );

    fixture.click("settings-server-update");
    fixture.click("modal-action-settings-server-update-confirm");
    assert_eq!(
        fixture.requests.take(),
        vec![ServerRequest::InstallUpdate {
            server: ServerKey::new("local"),
            version: "1.5.0".into(),
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

#[gpui::test]
fn end_all_sessions_needs_a_live_connection_and_confirms(cx: &mut TestAppContext) {
    let mut fixture = open(Appearance::Dark, snapshot(vec![local(), ssh()]), cx);

    fixture.click("settings-server-end-all-sessions");
    fixture.click("modal-action-settings-server-end-all-sessions-confirm");
    assert_eq!(
        fixture.requests.take(),
        vec![ServerRequest::EndAllSessions(ServerKey::new("local"))]
    );

    fixture.click("settings-server-option-build");
    fixture.click("settings-server-end-all-sessions");
    assert!(!fixture.modal_open());
    assert!(fixture.requests.take().is_empty());
    assert_eq!(
        fixture
            .caption(SettingsRowId::ServerEndAllSessions)
            .as_deref(),
        Some("Needs a live connection to Build Box.")
    );

    let confirmation = copy::end_all_sessions_confirmation(&local());
    assert_eq!(confirmation.title, "End all sessions on Studio?");
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
            "This device's connection to Old Box was removed. SpaceTerm keeps asking Old Box to confirm in the background, including after you dismiss this."
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

#[test]
fn a_rejection_in_the_servers_words_is_quoted_after_its_name() {
    assert_eq!(
        copy::rejection_caption(
            "Studio",
            &SettingRejection::Other("The disk is full.".into())
        ),
        "Studio didn't accept this location. The disk is full."
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
    let mut fixture = open(Appearance::Dark, snapshot(vec![conflicted]), cx);
    assert_eq!(fixture.field(), "~/mine/{branch}");
    let tree = A11yTree::read(fixture.cx);
    assert!(
        tree.find_text(
            "Another device changed this to \"~/theirs/{branch}\" first. Your change wasn't saved and is still in the field."
        )
        .is_some()
    );

    fixture.click("settings-server-worktree-location-save-mine");
    fixture.click("settings-server-worktree-location-discard-mine");
    assert_eq!(
        fixture.requests.take(),
        vec![
            ServerRequest::SetWorktreeLocation {
                server: ServerKey::new("local"),
                template: "~/mine/{branch}".into(),
                base_revision: 9,
            },
            ServerRequest::DiscardWorktreeLocationEdit(ServerKey::new("local")),
        ]
    );

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
    named(&tree, "ListItem", "Screen recording: Not set up");
    named(&tree, "ListItem", "Device control: Not allowed");
    assert_eq!(
        copy::permission_state(ServerPermissionState::NotNeeded),
        "Not needed"
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
        vec![ServerRequest::SetRemoteAccess {
            server: ServerKey::new("local"),
            enabled: true,
            scope: "tailnet".into(),
        }]
    );

    let confirmation = copy::enable_remote_access_confirmation(&local(), &scopes()[1]);
    assert_eq!(confirmation.title, "Allow remote connections to Studio?");
    assert_eq!(
        confirmation.message,
        "Studio accepts SpaceTerm connections over the network on Tailnet (100.64.0.20). Paired devices can then connect without SSH."
    );
}

#[gpui::test]
fn turning_remote_access_off_asks_only_when_this_device_would_disconnect(cx: &mut TestAppContext) {
    let enabled = Some(RemoteAccess {
        enabled: true,
        scope: Some("lan".into()),
        scopes: scopes(),
        pending: false,
    });
    let over_ssh = ServerSummary {
        status: ServerStatus::Running,
        remote_access: enabled.clone(),
        ..ssh()
    };
    let over_wss = ServerSummary {
        status: ServerStatus::Running,
        remote_access: enabled,
        ..wss()
    };
    let mut fixture = open(Appearance::Dark, snapshot(vec![over_ssh, over_wss]), cx);

    fixture.click("settings-server-remote-access");
    assert!(!fixture.modal_open());
    assert_eq!(
        fixture.requests.take(),
        vec![ServerRequest::SetRemoteAccess {
            server: ServerKey::new("build"),
            enabled: false,
            scope: "lan".into(),
        }]
    );

    fixture.click("settings-server-option-lab");
    fixture.click("settings-server-remote-access");
    assert!(fixture.modal_open());
    fixture.click("modal-action-settings-server-remote-access-disable-confirm");
    assert_eq!(
        fixture.requests.take(),
        vec![ServerRequest::SetRemoteAccess {
            server: ServerKey::new("lab"),
            enabled: false,
            scope: "lan".into(),
        }]
    );
}

#[gpui::test]
fn a_pending_remote_access_change_waits_for_the_server(cx: &mut TestAppContext) {
    let pending = ServerSummary {
        remote_access: Some(RemoteAccess {
            enabled: false,
            scope: None,
            scopes: scopes(),
            pending: true,
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
    assert_eq!(rows.len(), 10);
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
        SettingsRowId::ServerUnpair,
    ] {
        for appearance in [Appearance::Light, Appearance::Dark] {
            assert_eq!(row.reset_target(appearance), None, "{row:?}");
        }
    }
}
