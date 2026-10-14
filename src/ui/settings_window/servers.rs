//! The Servers section: the servers this device is paired with, the selected server's status, the
//! settings it owns, its permissions, and managing it.
//!
//! The section reads published server state and sends requests. It never writes server state to
//! the Settings Document, and every request that ends work for other clients asks first.

mod copy;
pub(crate) mod model;
mod paired_devices;

#[cfg(test)]
#[path = "servers_tests.rs"]
pub(super) mod tests;

use std::cell::Cell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use gpui::prelude::*;
use gpui::{
    AnyElement, App, Bounds, Context, Entity, FocusHandle, KeyDownEvent, Pixels, SharedString,
    Subscription, Window, accesskit, div, px,
};
use spaceterm_ui::{
    Alert, AlertIntent, Button, ButtonSize, ButtonVariant, ComboBoxItem, FieldState, Icon,
    IconName, ModalAction, ModalActionEmphasis, ModalActionIntent, ModalActionRole, ModalId,
    Switch, TextInput, TextInputEvent, TextInputVariant, ToggleSize,
};

use super::{SettingsRowId, SettingsWindow, settings_selector};
use crate::appearance::ChromeColors;
use crate::ui::appearance::settings::{SettingsAppearance, SettingsSurfaceRole};
use crate::ui::appearance::{ChromeAppearance, gpui_color};
use crate::ui::chrome_geometry::{HAIRLINE, RadiusRole};
use crate::ui::chrome_icons::IconRole;
use crate::ui::chrome_typography::{ChromeTextStyleExt as _, TextRole};
use crate::ui::sidebar_window::SidebarOwner as _;
use crate::ui::sidebar_window::form::{CaptionTone, action_button, badge, row_horizontal_inset};
use crate::worktrees::path_template::validate;
use copy::{Confirmation, StatusTone};
use model::{
    BindScope, BoundServerRequest, IncompatibleSide, InstalledServers, PendingUnpair, RemoteAccess,
    ServerKey, ServerPermissionKind, ServerRequest, ServerStatus, ServerSummary, ServerTextSetting,
    Servers, ServersSnapshot, SettingEditState,
};

/// The outline level of the selected server's name. The groups that present it sit one level
/// below.
pub(super) const SELECTED_SERVER_HEADING_LEVEL: usize = 2;

/// The rows that present the selected server. With no server to select they have nothing to say,
/// so Settings omits them.
fn presents_selected_server(row: SettingsRowId) -> bool {
    matches!(
        row,
        SettingsRowId::ServerStatus
            | SettingsRowId::ServerVersion
            | SettingsRowId::ServerConnection
            | SettingsRowId::ServerEndAllSessions
            | SettingsRowId::ServerWorktreeLocation
            | SettingsRowId::ServerPermissions
            | SettingsRowId::ServerRemoteAccess
            | SettingsRowId::ServerRemoteAccessScope
            | SettingsRowId::ServerPairedDevices
            | SettingsRowId::ServerUnpair
    )
}

/// The server rows whose controls take keyboard focus. Each one is a focus scope, so focus that
/// the keyboard moves into it scrolls the row into view.
const FOCUSABLE_ROWS: [SettingsRowId; 8] = [
    SettingsRowId::ServerStatus,
    SettingsRowId::ServerVersion,
    SettingsRowId::ServerEndAllSessions,
    SettingsRowId::ServerWorktreeLocation,
    SettingsRowId::ServerRemoteAccess,
    SettingsRowId::ServerRemoteAccessScope,
    SettingsRowId::ServerPairedDevices,
    SettingsRowId::ServerUnpair,
];

/// The Servers section's own state: the latest published servers and the person's choices.
pub(super) struct ServersSection {
    snapshot: ServersSnapshot,
    source: Option<Entity<Servers>>,
    _source_changes: Option<Subscription>,
    _installation: Subscription,
    /// The server the person chose. The first listed server stands in until they choose one.
    selected: Option<ServerKey>,
    requested: Option<spaceterm_core::identity::ServerId>,
    list_focus: FocusHandle,
    worktree: WorktreeField,
    devices: paired_devices::DeviceInventoryView,
    /// Bind scopes chosen while remote connections are off, used when they are turned on.
    proposed_scopes: HashMap<ServerKey, SharedString>,
    management_review: Option<ManagementReview>,
    management_notice: Option<(spaceterm_core::pairing::LocalPairingKey, SharedString)>,
    /// Notices dismissed here that the next snapshot has not dropped yet.
    dismissed: HashSet<SharedString>,
    /// The focus scope around each row in [`FOCUSABLE_ROWS`].
    row_focus: HashMap<SettingsRowId, FocusHandle>,
    /// The focus scope around the pending-unpair notices, and where they were last laid out.
    notices_focus: FocusHandle,
    notices_bounds: Rc<Cell<Option<Bounds<Pixels>>>>,
    _focus_scopes: Vec<Subscription>,
}

/// The selected server's New Worktree Location field. It sends a change on Return or when focus
/// leaves; Escape returns it to the server's value.
struct WorktreeField {
    input: Entity<TextInput>,
    /// The text the field last took from the published setting, and the revision the person edits
    /// from. Text that differs from it while the field has focus is the person's typing.
    synced: Option<SyncedText>,
    /// The request sent to each server that no published snapshot has answered yet. The field
    /// shows what it sent until the answer arrives, so a draft never reverts in the meantime.
    sent: HashMap<ServerKey, SentLocation>,
    unsent: HashMap<ServerKey, SyncedText>,
    _events: Subscription,
}

/// A New Worktree Location request that no published snapshot has answered yet.
#[derive(Clone, Debug, Eq, PartialEq)]
enum SentLocation {
    /// A change to `text`, made from the server's `base_revision`.
    Change {
        text: SharedString,
        base_revision: u64,
        /// The setting as published when the change went out.
        seen: ServerTextSetting,
    },
    /// Dropping this device's unapplied change.
    Discard,
}

impl SentLocation {
    fn request(&self, server: &ServerKey) -> ServerRequest {
        match self {
            Self::Change {
                text,
                base_revision,
                ..
            } => ServerRequest::SetWorktreeLocation {
                server: server.clone(),
                template: text.clone(),
                base_revision: *base_revision,
            },
            Self::Discard => ServerRequest::DiscardWorktreeLocationEdit(server.clone()),
        }
    }

    /// The text the field shows while the request is on its way.
    fn shown(&self, setting: &ServerTextSetting) -> SharedString {
        match self {
            Self::Change { text, .. } => text.clone(),
            Self::Discard => setting.value.clone(),
        }
    }

    /// Whether `setting` carries the request's outcome. The receiver publishes a change as an
    /// edit before anything else, so a change is answered once the setting differs from the one it
    /// went out against. A resent draft can match the edit already published, so the draft alone
    /// cannot tell. A discard is answered once no edit is left.
    fn answered(&self, setting: &ServerTextSetting) -> bool {
        match self {
            Self::Change { seen, .. } => setting != seen,
            Self::Discard => setting.edit.is_none(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct SyncedText {
    server: ServerKey,
    pairing: spaceterm_core::pairing::LocalPairingKey,
    text: SharedString,
    revision: u64,
}

/// A read-only assessment in flight. Selection changes cancel its presentation, not its operation.
struct ManagementReview {
    captured: BoundServerRequest,
    action: ManagementReviewAction,
    previous: Option<spaceterm_client::management_cli::confirmation::AssessmentToken>,
}

#[derive(Clone, Copy)]
enum ManagementReviewAction {
    Update,
    Stop,
    Restart,
    EndAll,
}

impl ManagementReviewAction {
    fn read(self, key: ServerKey) -> ServerRequest {
        match self {
            Self::Stop => ServerRequest::AssessStop(key),
            Self::Restart => ServerRequest::AssessRestart(key),
            Self::Update => ServerRequest::AssessInstallUpdate(key),
            Self::EndAll => ServerRequest::AssessEndAllSessions(key),
        }
    }

    fn execute(self, server: &ServerSummary) -> Option<ServerRequest> {
        Some(match self {
            Self::Stop => ServerRequest::Stop(server.key.clone()),
            Self::Restart => ServerRequest::Restart(server.key.clone()),
            Self::EndAll => ServerRequest::EndAllSessions(server.key.clone()),
            Self::Update => ServerRequest::InstallUpdate {
                server: server.key.clone(),
                version: server.staged_version.clone()?,
                assessment: server.installations.activation.clone()?,
            },
        })
    }

    fn assessment(
        self,
        server: &ServerSummary,
    ) -> Option<spaceterm_client::management_cli::confirmation::AssessmentToken> {
        use spaceterm_client::management_cli::confirmation::ManagementAction;
        use spaceterm_host::account_service::ServiceAction;
        match self {
            Self::Update => server.installations.activation.as_ref().filter(|assessment| server.activation_matches(assessment)).map(|assessment| assessment.reference.clone()),
            Self::Stop => server.management.stop.as_ref().filter(|value| value.action == ServiceAction::Stop).map(|value| value.reference.clone()),
            Self::Restart => server.management.restart.as_ref().filter(|value| value.action == ServiceAction::Restart).map(|value| value.reference.clone()),
            Self::EndAll => server.management.end_all.as_ref().filter(|value| {
                matches!(value.action(), ManagementAction::EndAllSessions {server: id} if *id == server.pairing.parts().server)
                    && value.epoch() == server.runtime_epoch
            }).map(|value| value.token().clone()),
        }
    }

    fn allowed(self, server: &ServerSummary) -> bool {
        match self {
            Self::Stop => status_action(server) == Some(StatusAction::Stop { enabled: true }),
            Self::Restart => version_action(server) == VersionAction::Restart { enabled: true },
            Self::EndAll => server.connected(),
            Self::Update => matches!(
                version_action(server),
                VersionAction::Update { enabled: true, .. }
            ),
        }
    }

    fn confirmation(self, server: &ServerSummary) -> Option<Confirmation> {
        let (mut prompt, count) = match self {
            Self::Update => (copy::update_confirmation(server, server.staged_version.as_ref()?), None),
            Self::Stop => (copy::stop_confirmation(server), server.management.stop.as_ref().map(|value| value.affected_sessions)),
            Self::Restart => (copy::restart_confirmation(server), server.management.restart.as_ref().map(|value| value.affected_sessions)),
            Self::EndAll => (copy::end_all_sessions_confirmation(server), server.management.end_all.as_ref().and_then(|value| match value.evidence() {
                spaceterm_client::management_cli::confirmation::AssessmentEvidence::EndAllSessions(value) => Some(value.summary().live_session_count()),
                _ => None,
            })),
        };
        if let Some(count) = count {
            prompt.detail = Some(
                format!(
                    "{} The assessment includes {count} live Terminal Sessions.",
                    prompt.detail.as_deref().unwrap_or_default()
                )
                .into(),
            );
        }
        Some(prompt)
    }
}

impl ServersSection {
    pub(super) fn new(window: &mut Window, cx: &mut Context<SettingsWindow>) -> Self {
        let list_focus = cx.focus_handle().tab_stop(true);
        cx.on_focus(&list_focus, window, |settings, window, cx| {
            settings.reveal_server_row(SettingsRowId::ServerList, window, cx);
            cx.notify();
        })
        .detach();
        cx.on_blur(&list_focus, window, |_, _, cx| cx.notify())
            .detach();
        // The section is taller than the window, so Tab can reach a control scrolled out of
        // sight. Each scope is a container, never a tab stop of its own.
        let mut focus_scopes = Vec::new();
        let row_focus = FOCUSABLE_ROWS
            .into_iter()
            .map(|row| {
                let focus = cx.focus_handle();
                focus_scopes.push(cx.on_focus_in(&focus, window, move |settings, window, cx| {
                    settings.reveal_server_row(row, window, cx);
                }));
                (row, focus)
            })
            .collect();
        let notices_focus = cx.focus_handle();
        let notices_bounds = Rc::new(Cell::new(None));
        focus_scopes.push(cx.on_focus_in(&notices_focus, window, {
            let bounds = Rc::clone(&notices_bounds);
            move |settings, window, cx| {
                if let Some(bounds) = bounds.get()
                    && window.last_input_was_keyboard()
                {
                    settings.scroll_bounds_into_view(bounds, cx);
                }
            }
        }));
        let input = cx.new(|cx| {
            TextInput::new(
                "settings-server-worktree-location",
                "New Worktree Location",
                String::new(),
                window,
                cx,
            )
            .variant(TextInputVariant::Bare)
            .input_length_limit(Some(1024))
            .debug_selector("settings-server-worktree-location")
        });
        let events = cx.subscribe_in(
            &input,
            window,
            |settings, _, event: &TextInputEvent, _, cx| match event {
                TextInputEvent::Submitted | TextInputEvent::FocusLost => {
                    settings.commit_server_worktree_location(cx);
                }
                TextInputEvent::Cancelled => settings.revert_server_worktree_location(cx),
                _ => {}
            },
        );
        // The client's server connections may be installed after this window opens.
        let installation =
            cx.observe_global::<InstalledServers>(|settings, cx| settings.follow_servers(cx));
        let mut section = Self {
            snapshot: ServersSnapshot::default(),
            source: None,
            _source_changes: None,
            _installation: installation,
            selected: None,
            requested: None,
            list_focus,
            worktree: WorktreeField {
                input,
                synced: None,
                sent: HashMap::new(),
                unsent: HashMap::new(),
                _events: events,
            },
            devices: paired_devices::DeviceInventoryView::default(),
            proposed_scopes: HashMap::new(),
            management_review: None,
            management_notice: None,
            dismissed: HashSet::new(),
            row_focus,
            notices_focus,
            notices_bounds,
            _focus_scopes: focus_scopes,
        };
        section.follow(cx);
        section
    }

    /// Follows the installed servers, returning whether the source changed.
    fn follow(&mut self, cx: &mut Context<SettingsWindow>) -> bool {
        let installed = cx
            .try_global::<InstalledServers>()
            .map(|installed| installed.0.clone());
        if installed.as_ref().map(Entity::entity_id) == self.source.as_ref().map(Entity::entity_id)
        {
            return false;
        }
        self._source_changes = installed.as_ref().map(|servers| {
            cx.observe(servers, |settings, servers, cx| {
                let snapshot = servers.read(cx).snapshot().clone();
                settings.servers.receive(snapshot);
                settings.synchronize_search_results();
                cx.notify();
            })
        });
        let snapshot = installed
            .as_ref()
            .map(|servers| servers.read(cx).snapshot().clone())
            .unwrap_or_default();
        self.receive(snapshot);
        self.source = installed;
        true
    }

    fn receive(&mut self, snapshot: ServersSnapshot) {
        self.dismissed.retain(|id| {
            snapshot.pending_unpairs.iter().any(|pending| {
                pending.id == *id
                    && self.snapshot.pending_unpairs.iter().any(|previous| {
                        previous.id == *id
                            && previous.original_pairing == pending.original_pairing
                            && previous.operation == pending.operation
                    })
            })
        });
        let same_pairing = |server: &ServerSummary| {
            self.snapshot
                .servers
                .iter()
                .any(|previous| previous.key == server.key && previous.pairing == server.pairing)
        };
        self.proposed_scopes.retain(|key, _| {
            snapshot
                .servers
                .iter()
                .any(|server| server.key == *key && same_pairing(server))
        });
        self.worktree.sent.retain(|key, sent| {
            snapshot
                .servers
                .iter()
                .find(|server| server.key == *key && same_pairing(server))
                .and_then(|server| server.worktree_location.as_ref())
                .is_some_and(|setting| !sent.answered(setting))
        });
        self.worktree.unsent.retain(|_, draft| {
            snapshot
                .servers
                .iter()
                .any(|server| server.pairing == draft.pairing)
        });
        self.snapshot = snapshot;
    }

    /// Every server in list order: the servers on this device, then the remote ones.
    fn ordered(&self) -> impl Iterator<Item = &ServerSummary> {
        let servers = &self.snapshot.servers;
        servers
            .iter()
            .filter(|server| server.route.is_local())
            .chain(servers.iter().filter(|server| !server.route.is_local()))
    }

    fn find(&self, key: &ServerKey) -> Option<&ServerSummary> {
        self.snapshot
            .servers
            .iter()
            .find(|server| server.key == *key)
    }

    /// The server the rows below the list present.
    pub(super) fn selected_server(&self) -> Option<&ServerSummary> {
        if let Some(server) = self.requested {
            return self
                .snapshot
                .servers
                .iter()
                .find(|value| value.pairing.parts().server == server);
        }
        self.selected
            .as_ref()
            .and_then(|key| self.find(key))
            .or_else(|| self.ordered().next())
    }

    /// Whether Settings omits `row` because there is no server for it to present.
    pub(super) fn omits(&self, row: SettingsRowId) -> bool {
        presents_selected_server(row) && self.selected_server().is_none()
    }

    /// The server rows answer for the server the list selects, so the list leads whenever a search
    /// finds any of them, including one the search ranks above the list.
    pub(super) fn complete(&self, rows: &mut Vec<SettingsRowId>) {
        if rows.iter().copied().any(presents_selected_server) {
            rows.retain(|row| *row != SettingsRowId::ServerList);
            rows.insert(0, SettingsRowId::ServerList);
        }
    }

    fn notices(&self) -> Vec<PendingUnpair> {
        self.snapshot
            .pending_unpairs
            .iter()
            .filter(|pending| !self.dismissed.contains(&pending.id))
            .cloned()
            .collect()
    }

    /// The scope Listen on shows. While remote connections are on, it is the one the server listens
    /// on, or none when the server no longer offers that one. While they are off, it is the one
    /// turning them on would use: the one the person chose, the one the server used last, or the
    /// first it offers.
    fn shown_scope(&self, server: &ServerSummary) -> Option<BindScope> {
        let remote = server.remote_access.as_ref()?;
        let offered = |id: &SharedString| remote.scopes.iter().find(|scope| scope.id == *id);
        if remote.enabled {
            return remote.scope.as_ref().and_then(offered).cloned();
        }
        self.proposed_scopes
            .get(&server.key)
            .and_then(offered)
            .or_else(|| remote.scope.as_ref().and_then(offered))
            .or_else(|| remote.scopes.first())
            .cloned()
    }

    /// Where this device's change to the server's New Worktree Location stands, counting a request
    /// no snapshot has answered yet.
    fn location_edit_state(&self, server: &ServerSummary) -> Option<SettingEditState> {
        match self.worktree.sent.get(&server.key) {
            Some(SentLocation::Change { .. }) => Some(SettingEditState::Saving),
            Some(SentLocation::Discard) => None,
            None => server
                .worktree_location
                .as_ref()?
                .edit
                .as_ref()
                .map(|edit| edit.state.clone()),
        }
    }
}

/// The server's remote connections when this device can change them: it holds a live connection
/// and has no change of its own waiting for the server.
fn adjustable_remote_access(server: &ServerSummary) -> Option<&RemoteAccess> {
    server
        .remote_access
        .as_ref()
        .filter(|remote| server.connected() && !remote.pending)
}

/// Where one option sits in the server list, and whether the list holds keyboard focus.
#[derive(Clone, Copy, Debug)]
struct OptionPlacement {
    selected: bool,
    list_focused: bool,
    /// Whether the selection shows the accent fill, which only keyboard focus earns.
    emphasized: bool,
    /// The option's 1-based position among every listed server.
    position: usize,
    total: usize,
}

/// Names the Status row's badge and the text it publishes.
const STATUS_BADGE: &str = "settings-server-status-text";
/// Names the Version row's badge and the text it publishes.
const VERSION_BADGE: &str = "settings-server-version-text";

/// What the Status row offers.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum StatusAction {
    Start { enabled: bool },
    Stop { enabled: bool },
}

/// Start and Stop both need host access through the service manager or SSH: Start launches the
/// service, and Stop suspends its automatic startup with the host's authorization. A WSS
/// connection offers neither, so this device could not undo a Stop it sent.
fn status_action(server: &ServerSummary) -> Option<StatusAction> {
    let host = server.route.manages_host();
    match server.status {
        ServerStatus::Stopped => Some(StatusAction::Start { enabled: host }),
        ServerStatus::Running
        | ServerStatus::Starting
        | ServerStatus::Incompatible(_)
        | ServerStatus::UnsupportedProfiles => Some(StatusAction::Stop { enabled: host }),
        ServerStatus::Stopping => Some(StatusAction::Stop { enabled: false }),
        ServerStatus::Connecting | ServerStatus::Offline | ServerStatus::Updating { .. } => None,
    }
}

/// What the Version row offers.
#[derive(Clone, Debug, Eq, PartialEq)]
enum VersionAction {
    Restart {
        enabled: bool,
    },
    Update {
        version: SharedString,
        enabled: bool,
    },
}

/// Restart and activation use the selected host's native owner. A WSS connection has no such
/// capability, so it cannot obtain the exact account-service assessment.
fn version_action(server: &ServerSummary) -> VersionAction {
    match &server.staged_version {
        Some(version) => VersionAction::Update {
            version: version.clone(),
            enabled: server.route.manages_host()
                && server.installations.selection_generation.is_some()
                && server
                    .installations
                    .staged
                    .as_ref()
                    .is_some_and(|candidate| {
                        candidate.artifact_sha256 != [0; 32] && &candidate.version == version
                    })
                && matches!(
                    server.status,
                    ServerStatus::Running
                        | ServerStatus::Stopped
                        | ServerStatus::Incompatible(IncompatibleSide::Server)
                ),
        },
        None => VersionAction::Restart {
            enabled: server.connected() && server.route.manages_host(),
        },
    }
}

fn tone_color(tone: StatusTone, colors: &ChromeColors) -> crate::appearance::Color {
    match tone {
        StatusTone::Positive => colors.success,
        StatusTone::Neutral => colors.icon_muted,
        StatusTone::Attention => colors.warning,
    }
}

/// The selection colors one server list option paints.
fn option_selection(emphasized: bool, appearance: &ChromeAppearance) -> ChromeColors {
    let card = appearance.host_colors(spaceterm_ui::ControlHost::Card);
    if emphasized && appearance.active {
        crate::ui::selection_chip::emphasized_selection_colors(card)
    } else if appearance.active {
        appearance
            .unfocused_selection_colors(spaceterm_ui::ControlHost::Card)
            .clone()
    } else {
        card.clone()
    }
}

/// A row action whose label and enabled state follow the selected server.
fn server_button(
    selector: &'static str,
    label: &'static str,
    enabled: bool,
    on_activate: impl Fn(&mut SettingsWindow, &mut Window, &mut Context<SettingsWindow>) + 'static,
    cx: &mut Context<SettingsWindow>,
) -> AnyElement {
    let owner = cx.weak_entity();
    action_button(selector, label, enabled, move |window, cx| {
        let _ = owner.update(cx, |settings, cx| on_activate(settings, window, cx));
    })
    .into_any_element()
}

impl SettingsWindow {
    /// Scrolls a server row into view when the keyboard moved focus into it. Pointer focus leaves
    /// the scroll position alone, so a press never moves its target out from under the pointer.
    fn reveal_server_row(&mut self, row: SettingsRowId, window: &Window, cx: &mut Context<Self>) {
        if window.last_input_was_keyboard() {
            self.scroll_row_into_view(row, cx);
        }
    }

    /// Follows a server source installed after this window opened.
    fn follow_servers(&mut self, cx: &mut Context<Self>) {
        if self.servers.follow(cx) {
            self.synchronize_search_results();
            cx.notify();
        }
    }

    /// Sends one request to the installed servers.
    fn send_server_request(&mut self, request: ServerRequest, cx: &mut Context<Self>) {
        let Some(request) = BoundServerRequest::bind(request, &self.servers.snapshot) else {
            return;
        };
        self.send_bound_server_request(request, cx);
    }

    fn send_bound_server_request(&mut self, request: BoundServerRequest, cx: &mut Context<Self>) {
        let Some(servers) = self.servers.source.clone() else {
            return;
        };
        if !request.still_matches(servers.read(cx).snapshot()) {
            return;
        }
        let requests = servers.read(cx).requests();
        requests.send(request, cx);
    }

    pub(super) fn select_server_id(
        &mut self,
        server: spaceterm_core::identity::ServerId,
        cx: &mut Context<Self>,
    ) {
        self.select_section(super::SettingsSectionId::Servers, cx);
        self.servers.devices.selection_changed();
        self.servers.requested = Some(server);
        self.servers.selected = None;
        self.synchronize_search_results();
        cx.notify();
    }

    fn select_server(&mut self, key: ServerKey, cx: &mut Context<Self>) {
        let requested = self.servers.requested.take().is_some();
        if requested || self.servers.selected.as_ref() != Some(&key) {
            self.servers.devices.selection_changed();
            self.servers.management_review = None;
            self.servers.selected = Some(key);
            cx.notify();
        }
    }

    /// Moves the list's selection with the arrow keys. Selection follows the keyboard.
    fn navigate_servers(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.servers.list_focus.is_focused(window) || event.keystroke.modifiers.modified() {
            return;
        }
        let keys = self
            .servers
            .ordered()
            .map(|server| server.key.clone())
            .collect::<Vec<_>>();
        if keys.is_empty() {
            return;
        }
        let current = self
            .servers
            .selected_server()
            .and_then(|server| keys.iter().position(|key| *key == server.key))
            .unwrap_or(0);
        let next = match event.keystroke.key.as_str() {
            "up" => current.saturating_sub(1),
            "down" => (current + 1).min(keys.len() - 1),
            "home" => 0,
            "end" => keys.len() - 1,
            _ => return,
        };
        window.prevent_default();
        cx.stop_propagation();
        self.select_server(keys[next].clone(), cx);
        // A key press after a pointer press turns the accent selection on even where the
        // selection stays put.
        cx.notify();
    }

    pub(super) fn render_server_row(
        &mut self,
        row: SettingsRowId,
        appearance: &ChromeAppearance,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        if row == SettingsRowId::ServerList {
            return self.render_server_list(appearance, window, cx);
        }
        let Some(server) = self.servers.selected_server().cloned() else {
            return div().into_any_element();
        };
        self.advance_management_review(&server, window, cx);
        let control = if row == SettingsRowId::ServerPairedDevices {
            self.render_paired_devices(&server, appearance, window, cx)
        } else {
            self.render_selected_server_row(row, &server, appearance, cx)
        };
        match self.servers.row_focus.get(&row) {
            // The scope wraps the control without a box of its own, so the row keeps its layout.
            Some(focus) => div()
                .track_focus(focus)
                .min_w_0()
                .flex()
                .flex_row()
                .items_center()
                .when(
                    matches!(
                        row,
                        SettingsRowId::ServerWorktreeLocation | SettingsRowId::ServerPairedDevices
                    ),
                    |scope| scope.w_full(),
                )
                .child(control)
                .into_any_element(),
            None => control,
        }
    }

    fn render_selected_server_row(
        &mut self,
        row: SettingsRowId,
        server: &ServerSummary,
        appearance: &ChromeAppearance,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        match row {
            SettingsRowId::ServerStatus => self.render_server_status(server, appearance, cx),
            SettingsRowId::ServerVersion => self.render_server_version(server, appearance, cx),
            SettingsRowId::ServerConnection => text_value(
                "settings-row-server-connection-control",
                copy::route_description(&server.route),
                appearance,
            ),
            SettingsRowId::ServerEndAllSessions => {
                let key = server.key.clone();
                server_button(
                    "settings-server-end-all-sessions",
                    "End All Sessions…",
                    server.connected() && !server.management.pending,
                    move |settings, _, cx| {
                        settings.review_management(&key, ManagementReviewAction::EndAll, cx);
                    },
                    cx,
                )
            }
            SettingsRowId::ServerWorktreeLocation => {
                self.render_server_worktree_location(server, appearance, cx)
            }
            SettingsRowId::ServerPermissions => render_server_permissions(server, appearance),
            SettingsRowId::ServerRemoteAccess => self.render_remote_access(server, appearance, cx),
            SettingsRowId::ServerRemoteAccessScope => {
                self.render_remote_access_scope(server, appearance, cx)
            }
            SettingsRowId::ServerUnpair => {
                let key = server.key.clone();
                server_button(
                    "settings-server-unpair",
                    "Unpair…",
                    true,
                    move |settings, window, cx| {
                        // Unpairing works without a connection: the revocation retries until the
                        // server confirms it.
                        settings.confirm_server_request(
                            &key,
                            copy::unpair_confirmation,
                            ServerRequest::UnpairThisDevice(key.clone()),
                            |_| true,
                            window,
                            cx,
                        );
                    },
                    cx,
                )
            }
            _ => div().into_any_element(),
        }
    }

    /// The caption under one server row, when it has something to say.
    pub(super) fn server_row_caption(
        &self,
        row: SettingsRowId,
        cx: &App,
    ) -> Option<(SharedString, CaptionTone)> {
        let server = self.servers.selected_server()?;
        let name = &server.name;
        let guidance = |text: SharedString| Some((text, CaptionTone::Guidance));
        match row {
            SettingsRowId::ServerStatus => {
                let tone = if matches!(server.status, ServerStatus::Incompatible(_) | ServerStatus::UnsupportedProfiles) {
                    CaptionTone::Warning
                } else {
                    CaptionTone::Guidance
                };
                Some((copy::status_explanation(server), tone))
            }
            SettingsRowId::ServerVersion => {
                let version = server.staged_version.as_ref()?;
                let text = if !server.route.manages_host() {
                    format!(
                        "Version {version} is ready. To install it, connect to {name} over SSH or use its machine."
                    )
                } else if server.status == ServerStatus::Stopped {
                    format!("Version {version} is ready. {name} stays stopped after it installs.")
                } else {
                    format!("Version {version} is ready. Installing it restarts {name}.")
                };
                guidance(text.into())
            }
            SettingsRowId::ServerEndAllSessions if server.connected() => guidance(
                format!("Ends every Terminal Session on {name} for every client. {name} keeps running.")
                    .into(),
            ),
            SettingsRowId::ServerEndAllSessions => {
                guidance(format!("Needs a live connection to {name}.").into())
            }
            SettingsRowId::ServerWorktreeLocation => self.server_worktree_location_caption(server, cx),
            SettingsRowId::ServerPermissions => match &server.permissions {
                Some(_) => guidance(copy::permissions_explanation(server)),
                None if server.connected() => {
                    guidance(format!("{name} doesn't report its permissions.").into())
                }
                None => guidance(format!("Connect to {name} to see its permissions.").into()),
            },
            SettingsRowId::ServerRemoteAccess => {
                let remote = server.remote_access.as_ref();
                let text: SharedString = match remote {
                    _ if !server.connected() => {
                        format!("Connect to {name} to change this.").into()
                    }
                    None => format!("{name} doesn't offer remote connections.").into(),
                    Some(remote) if remote.pending || remote.edit.as_ref().is_some_and(|edit| edit.state == SettingEditState::Saving) => {
                        format!("Waiting for {name} to confirm…").into()
                    }
                    Some(remote) if remote.enabled => format!(
                        "On. Paired devices can connect to {name} over the network without SSH."
                    )
                    .into(),
                    Some(_) => format!(
                        "Off. Devices reach {name} over SSH or on its own machine."
                    )
                    .into(),
                };
                guidance(text)
            }
            SettingsRowId::ServerRemoteAccessScope => {
                let remote = server.remote_access.as_ref()?;
                let warning = |text: String| Some((text.into(), CaptionTone::Warning));
                if remote.enabled && !remote.endpoints.is_empty() && self.servers.shown_scope(server).is_none() {
                    return guidance(format!("Configured addresses: {}. Scope choices are unavailable on this connection.", remote.endpoints.iter().map(ToString::to_string).collect::<Vec<_>>().join(", ")).into());
                }
                match (remote.enabled, self.servers.shown_scope(server)) {
                    (true, Some(scope)) => guidance(scope.addresses),
                    (false, Some(scope)) => guidance(
                        format!("{}. Used when remote connections are on.", scope.addresses)
                            .into(),
                    ),
                    // The server listens on addresses it no longer offers, so none of the offered
                    // ones may stand in for them.
                    (true, None) if remote.scopes.is_empty() => warning(format!(
                        "{name} no longer offers the addresses remote connections use. Turn remote connections off, or wait for {name} to offer them again."
                    )),
                    (true, None) => warning(format!(
                        "{name} no longer offers the addresses remote connections use. Choose other addresses, or turn remote connections off."
                    )),
                    (false, None) => guidance(
                        format!("{name} offers no addresses for remote connections.").into(),
                    ),
                }
            }
            SettingsRowId::ServerUnpair => guidance(
                format!(
                    "Removes {name} from this device now. {name} keeps running for its other devices."
                )
                .into(),
            ),
            _ => None,
        }
    }

    fn render_server_list(
        &mut self,
        appearance: &ChromeAppearance,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let card = appearance.host_colors(spaceterm_ui::ControlHost::Card);
        if self.servers.snapshot.servers.is_empty() {
            return div()
                .id("settings-server-list")
                .debug_selector(|| "settings-server-list".to_owned())
                .w_full()
                .flex()
                .flex_col()
                .gap(appearance.spacing(2.0))
                .child(
                    div()
                        .chrome_text(appearance.typography.style(TextRole::BodyEmphasis))
                        .text_color(gpui_color(card.text))
                        .child(gpui::Text::new(
                            SharedString::from("settings-server-list-empty").into(),
                            SharedString::from("No servers yet"),
                        )),
                )
                .child(
                    div()
                        .chrome_text(appearance.typography.style(TextRole::Secondary))
                        .text_color(gpui_color(card.text_muted))
                        .whitespace_normal()
                        .child(gpui::Text::new(
                            SharedString::from("settings-server-list-empty-guidance").into(),
                            SharedString::from(
                                "Servers this device is paired with appear here, including the one on this device. Connecting to a machine over SSH pairs this device with its server.",
                            ),
                        )),
                )
                .into_any_element();
        }
        let focus = self.servers.list_focus.clone();
        let focused = focus.is_focused(window);
        // A list the keyboard is driving shows its selection in the accent color, as a native
        // table does. A pointer press focuses the list too, but leaves the resting selection
        // colors, as the sidebar and the theme slots do.
        let emphasized = focused && window.last_input_was_keyboard();
        let selection = option_selection(emphasized, appearance);
        let selected = self
            .servers
            .selected_server()
            .map(|server| server.key.clone());
        let ordered = self.servers.ordered().cloned().collect::<Vec<_>>();
        let total = ordered.len();
        let mut position = 0;
        let mut groups = Vec::new();
        for (title, local) in [("Local", true), ("Remote", false)] {
            let members = ordered
                .iter()
                .filter(|server| server.route.is_local() == local)
                .map(|server| {
                    position += 1;
                    let placement = OptionPlacement {
                        selected: selected.as_ref() == Some(&server.key),
                        list_focused: focused,
                        emphasized,
                        position,
                        total,
                    };
                    self.render_server_option(server, placement, &selection, appearance, cx)
                })
                .collect::<Vec<_>>();
            if members.is_empty() {
                continue;
            }
            let slug = title.to_ascii_lowercase();
            groups.push(
                div()
                    .id(SharedString::from(format!("settings-server-group-{slug}")))
                    .role(accesskit::Role::Group)
                    .aria_label(title)
                    .debug_selector(move || format!("settings-server-group-{slug}"))
                    .flex()
                    .flex_col()
                    .gap(appearance.spacing(1.0))
                    .child(
                        div()
                            .px(appearance.spacing(8.0))
                            .pb(appearance.spacing(2.0))
                            .chrome_text(appearance.typography.style(TextRole::Caption))
                            .text_color(gpui_color(card.text_muted))
                            .child(title),
                    )
                    .children(members),
            );
        }
        div()
            .id("settings-server-list")
            .role(accesskit::Role::ListBox)
            .aria_label("Servers")
            .debug_selector(|| "settings-server-list".to_owned())
            .track_focus(&focus)
            .on_key_down(cx.listener(Self::navigate_servers))
            .w_full()
            .flex()
            .flex_col()
            .gap(appearance.spacing(8.0))
            .children(groups)
            .into_any_element()
    }

    fn render_server_option(
        &self,
        server: &ServerSummary,
        placement: OptionPlacement,
        selection: &ChromeColors,
        appearance: &ChromeAppearance,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let OptionPlacement {
            selected,
            list_focused: focused,
            emphasized,
            position,
            total,
        } = placement;
        let card = appearance.host_colors(spaceterm_ui::ControlHost::Card);
        let (foreground, secondary, icon) = if selected {
            (
                selection.row_selected_foreground,
                selection.row_selected_secondary,
                selection.row_selected_icon,
            )
        } else {
            (card.text, card.text_muted, card.icon_muted)
        };
        // The accent fill carries the selection's own foreground, so status tints yield to it.
        let status_color = if selected && emphasized && appearance.active {
            selection.row_selected_icon
        } else {
            tone_color(copy::status_tone(&server.status), card)
        };
        let key = server.key.clone();
        let clicked = key.clone();
        let selector = format!("settings-server-option-{}", server.key.0);
        let selecting = cx.weak_entity();
        let pressing = cx.weak_entity();
        let glyph = appearance.icons.metrics(IconRole::Row).glyph_size;
        div()
            .id(SharedString::from(selector.clone()))
            .role(accesskit::Role::ListBoxOption)
            .aria_label(server.name.clone())
            .aria_description(copy::list_description(server))
            .aria_selected(selected)
            .aria_position_in_set(position)
            .aria_size_of_set(total)
            .when(selected && focused, |row| row.aria_active_descendant())
            .on_a11y_action(accesskit::Action::Click, move |_, _, cx| {
                let _ = pressing.update(cx, |settings, cx| settings.select_server(key.clone(), cx));
            })
            .on_click(move |_, _, cx| {
                let _ = selecting.update(cx, |settings, cx| {
                    settings.select_server(clicked.clone(), cx);
                });
            })
            .debug_selector(move || selector.clone())
            .flex()
            .flex_row()
            .items_center()
            .gap(appearance.spacing(10.0))
            .px(appearance.spacing(8.0))
            .py(appearance.spacing(6.0))
            .rounded(RadiusRole::Control.pixels())
            .cursor_default()
            .when(selected, |row| {
                row.bg(gpui_color(selection.row_selected_background))
            })
            .when(!selected, |row| {
                let hover = card.row_hover_background;
                row.hover(move |style| style.bg(gpui_color(hover)))
            })
            .child(div().flex_none().child(Icon::new(
                if server.route.is_local() {
                    IconName::Laptop
                } else {
                    IconName::Server
                },
                glyph,
                gpui_color(icon),
            )))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .min_w_0()
                    .flex_1()
                    .child(
                        div()
                            .truncate()
                            .chrome_text(appearance.typography.style(TextRole::BodyEmphasis))
                            .text_color(gpui_color(foreground))
                            .child(server.name.clone()),
                    )
                    .child(
                        div()
                            .truncate()
                            .chrome_text(appearance.typography.style(TextRole::Secondary))
                            .text_color(gpui_color(secondary))
                            .child(copy::list_detail(server)),
                    ),
            )
            .child(
                div()
                    .flex_none()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(appearance.spacing(4.0))
                    .child(Icon::new(
                        copy::status_icon(&server.status),
                        appearance.icons.metrics(IconRole::Status).glyph_size,
                        gpui_color(status_color),
                    ))
                    .child(
                        div()
                            .chrome_text(appearance.typography.style(TextRole::Secondary))
                            .text_color(gpui_color(secondary))
                            .child(copy::status_label(&server.status)),
                    ),
            )
            .into_any_element()
    }

    /// The selected server's name above the rows that present it. It is the parent of their group
    /// headings, one level below the section's title and in the group headings' text style.
    pub(super) fn render_selected_server_heading(
        &self,
        appearance: &ChromeAppearance,
    ) -> Option<AnyElement> {
        let server = self.servers.selected_server()?;
        let glyph = appearance.icons.metrics(IconRole::Row).glyph_size;
        Some(
            div()
                .id("settings-server-heading")
                .role(accesskit::Role::Heading)
                .aria_level(SELECTED_SERVER_HEADING_LEVEL)
                .aria_label(server.name.clone())
                .debug_selector(|| "settings-server-heading".to_owned())
                .px(row_horizontal_inset(appearance))
                .flex()
                .flex_row()
                .items_center()
                .gap(appearance.spacing(6.0))
                .child(div().flex_none().child(Icon::new(
                    if server.route.is_local() {
                        IconName::Laptop
                    } else {
                        IconName::Server
                    },
                    glyph,
                    gpui_color(appearance.colors.icon_muted),
                )))
                .child(
                    div()
                        .min_w_0()
                        .truncate()
                        .chrome_text(appearance.typography.style(TextRole::Section))
                        .text_color(gpui_color(appearance.colors.text))
                        .child(server.name.clone()),
                )
                .child(
                    div()
                        .flex_none()
                        .chrome_text(appearance.typography.style(TextRole::Secondary))
                        .text_color(gpui_color(appearance.colors.text_muted))
                        .child(copy::route_summary(&server.route)),
                )
                .into_any_element(),
        )
    }

    /// One dismissible notice per pending unpair, stacked above the list in one focus scope.
    pub(super) fn render_unpair_notices(
        &self,
        settings: &SettingsAppearance,
        spacing: Pixels,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let notices = self.render_unpair_notice_cards(settings, cx);
        if notices.is_empty() {
            self.servers.notices_bounds.set(None);
            return None;
        }
        let bounds = Rc::clone(&self.servers.notices_bounds);
        Some(
            div()
                .track_focus(&self.servers.notices_focus)
                .flex()
                .flex_col()
                .w_full()
                .gap(spacing)
                .on_children_prepainted(move |children: Vec<Bounds<Pixels>>, _, _| {
                    bounds.set(
                        children
                            .into_iter()
                            .reduce(|all, notice| all.union(&notice)),
                    );
                })
                .children(notices)
                .into_any_element(),
        )
    }

    fn render_unpair_notice_cards(
        &self,
        settings: &SettingsAppearance,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let appearance = &settings.chrome;
        let colors = appearance.host_colors(spaceterm_ui::ControlHost::Card);
        self.servers
            .notices()
            .into_iter()
            .map(|pending| {
                let (title, body) = copy::pending_unpair_text(&pending);
                let captured = BoundServerRequest::bind(
                    ServerRequest::DismissUnpairNotice(pending.id.clone()),
                    &self.servers.snapshot,
                );
                let owner = cx.weak_entity();
                let selector = format!("settings-server-unpair-notice-{}", pending.id);
                let dismiss = format!("{selector}-dismiss");
                spaceterm_ui::ControlHost::Card
                    .mount(
                        div()
                            .id(SharedString::from(selector.clone()))
                            .role(accesskit::Role::Group)
                            .aria_label(title.clone())
                            .debug_selector({
                                let selector = selector.clone();
                                move || selector.clone()
                            })
                            .flex()
                            .flex_row()
                            .items_start()
                            .gap(appearance.spacing(8.0))
                            .p(appearance.spacing(10.0))
                            .rounded(RadiusRole::Card.pixels())
                            .bg(gpui_color(
                                settings.surface(SettingsSurfaceRole::Card).paint,
                            ))
                            .border(px(HAIRLINE))
                            .border_color(gpui_color(settings.card_edge()))
                            .child(div().flex_none().mt(px(1.0)).child(Icon::new(
                                IconName::Info,
                                appearance.icons.metrics(IconRole::Status).glyph_size,
                                gpui_color(colors.info),
                            )))
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .min_w_0()
                                    .flex_1()
                                    .gap(appearance.spacing(2.0))
                                    .child(
                                        div()
                                            .chrome_text(
                                                appearance.typography.style(TextRole::BodyEmphasis),
                                            )
                                            .text_color(gpui_color(colors.text))
                                            .child(gpui::Text::new(
                                                SharedString::from(format!("{selector}-title"))
                                                    .into(),
                                                title,
                                            )),
                                    )
                                    .child(
                                        div()
                                            .chrome_text(
                                                appearance.typography.style(TextRole::Secondary),
                                            )
                                            .text_color(gpui_color(colors.text_muted))
                                            .whitespace_normal()
                                            .child(gpui::Text::new(
                                                SharedString::from(format!("{selector}-body"))
                                                    .into(),
                                                body,
                                            )),
                                    ),
                            )
                            .child(
                                Button::new(SharedString::from(dismiss.clone()), "Dismiss")
                                    .variant(ButtonVariant::Outline)
                                    .size(ButtonSize::Regular)
                                    .tab_stop(true)
                                    .debug_selector(dismiss)
                                    .on_activate(move |_, window, cx| {
                                        if let Some(captured) = captured.clone() {
                                            let _ = owner.update(cx, |settings, cx| {
                                                settings
                                                    .dismiss_unpair_notice(captured, window, cx);
                                            });
                                        }
                                    }),
                            ),
                    )
                    .into_any_element()
            })
            .collect()
    }

    /// Hides the notice now. The background revocation keeps retrying.
    fn dismiss_unpair_notice(
        &mut self,
        bound: BoundServerRequest,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !bound.still_matches(&self.servers.snapshot) {
            return;
        }
        let ServerRequest::DismissUnpairNotice(id) = &bound.request else {
            return;
        };
        self.servers.dismissed.insert(id.clone());
        self.send_bound_server_request(bound, cx);
        if self.servers.snapshot.servers.is_empty() {
            self.search.read(cx).focus_handle().focus(window, cx);
        } else {
            self.servers.list_focus.focus(window, cx);
        }
        cx.notify();
    }

    fn render_server_status(
        &mut self,
        server: &ServerSummary,
        appearance: &ChromeAppearance,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let key = server.key.clone();
        let action = status_action(server).map(|action| match action {
            StatusAction::Start { enabled } => server_button(
                "settings-server-start",
                "Start Server",
                enabled && !server.management.pending,
                move |settings, _, cx| {
                    settings.send_server_request(ServerRequest::Start(key.clone()), cx);
                },
                cx,
            ),
            StatusAction::Stop { enabled } => server_button(
                "settings-server-stop",
                "Stop Server…",
                enabled && !server.management.pending,
                move |settings, _, cx| {
                    settings.review_management(&key, ManagementReviewAction::Stop, cx);
                },
                cx,
            ),
        });
        let notice = self.management_status(server).map(|text| {
            let announcement = text.clone();
            div()
                .id("settings-server-management-status")
                .role(accesskit::Role::Group)
                .aria_label("Server management status")
                .debug_selector(|| "settings-server-management-status".to_owned())
                .a11y_synthetic_children(move |builder| {
                    let mut status = accesskit::Node::new(accesskit::Role::Status);
                    status.set_label(announcement.as_ref());
                    status.set_live(accesskit::Live::Polite);
                    builder.push_child(builder.synthetic_node_id("management-status"), status);
                })
                .whitespace_normal()
                .chrome_text(appearance.typography.style(TextRole::Secondary))
                .text_color(gpui_color(appearance.colors.text_secondary))
                .child(text)
                .when(self.servers.management_review.is_some(), |notice| {
                    notice.child(server_button(
                        "settings-server-management-cancel-review",
                        "Cancel Review",
                        true,
                        |settings, window, cx| {
                            settings.servers.management_review = None;
                            settings.servers.list_focus.focus(window, cx);
                            cx.notify();
                        },
                        cx,
                    ))
                })
        });
        // The state reads as a badge beside its action, as every state row in Settings does.
        div()
            .debug_selector(|| "settings-row-server-status-control".to_owned())
            .flex()
            .flex_row()
            .items_center()
            .gap(appearance.spacing(8.0))
            .child(
                div()
                    .debug_selector(|| STATUS_BADGE.to_owned())
                    .child(badge(
                        STATUS_BADGE,
                        copy::status_label(&server.status),
                        appearance,
                    )),
            )
            .children(action)
            .children(notice)
            .into_any_element()
    }

    fn render_server_version(
        &mut self,
        server: &ServerSummary,
        appearance: &ChromeAppearance,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let key = server.key.clone();
        let action = match version_action(server) {
            VersionAction::Restart { enabled } => server_button(
                "settings-server-restart",
                "Restart Server…",
                enabled && !server.management.pending,
                move |settings, _, cx| {
                    settings.review_management(&key, ManagementReviewAction::Restart, cx);
                },
                cx,
            ),
            VersionAction::Update { enabled, .. } => server_button(
                "settings-server-update",
                "Update Server…",
                enabled && !server.management.pending,
                move |settings, _, cx| {
                    settings.review_management(&key, ManagementReviewAction::Update, cx);
                },
                cx,
            ),
        };
        div()
            .debug_selector(|| "settings-row-server-version-control".to_owned())
            .flex()
            .flex_row()
            .items_center()
            .gap(appearance.spacing(8.0))
            .child(
                div()
                    .debug_selector(|| VERSION_BADGE.to_owned())
                    .child(badge(
                        VERSION_BADGE,
                        server
                            .version
                            .clone()
                            .unwrap_or_else(|| SharedString::new_static("Unknown")),
                        appearance,
                    )),
            )
            .child(action)
            .into_any_element()
    }

    fn review_management(
        &mut self,
        key: &ServerKey,
        action: ManagementReviewAction,
        cx: &mut Context<Self>,
    ) {
        let Some(server) = self
            .servers
            .find(key)
            .filter(|server| action.allowed(server) && !server.management.pending)
        else {
            return;
        };
        let previous = action.assessment(server);
        let pairing = server.pairing.clone();
        let Some(captured) =
            BoundServerRequest::bind(action.read(key.clone()), &self.servers.snapshot)
        else {
            return;
        };
        self.servers.management_review = Some(ManagementReview {
            captured: captured.clone(),
            action,
            previous,
        });
        self.servers.management_notice = None;
        self.send_bound_server_request(captured, cx);
        // A synchronous provider can publish a failure before returning.
        if self
            .servers
            .find(key)
            .is_none_or(|server| server.pairing != pairing)
        {
            self.servers.management_review = None;
        }
        cx.notify();
    }

    fn management_status(&self, server: &ServerSummary) -> Option<SharedString> {
        if let Some((pairing, text)) = &self.servers.management_notice
            && pairing == &server.pairing
        {
            return Some(text.clone());
        }
        if let Some(failure) = server.management.failure {
            return Some(copy::management_failure(failure));
        }
        self.servers
            .management_review
            .as_ref()
            .filter(|review| review.captured.request.server() == Some(&server.key))
            .map(|_| format!("Checking the affected sessions on {}…", server.name).into())
    }

    fn advance_management_review(
        &mut self,
        server: &ServerSummary,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(review) = self.servers.management_review.as_ref() else {
            return;
        };
        if review.captured.request.server() != Some(&server.key)
            || !review.captured.still_matches(&self.servers.snapshot)
            || !review.action.allowed(server)
        {
            self.servers.management_review = None;
            self.servers.management_notice = Some((
                server.pairing.clone(),
                "The server changed. Review the action again before confirming.".into(),
            ));
            return;
        }
        if server.management.pending {
            return;
        }
        if server.management.failure.is_some() {
            self.servers.management_review = None;
            return;
        }
        let Some(token) = review
            .action
            .assessment(server)
            .filter(|token| Some(token) != review.previous.as_ref())
        else {
            return;
        };
        let Some(review) = self.servers.management_review.take() else {
            return;
        };
        let captured_server = server.clone();
        cx.defer_in(window, move |settings, window, cx| {
            let Some(current) = settings.servers.find(&captured_server.key) else {
                return;
            };
            if settings.servers.selected_server().map(|value| &value.key)
                != Some(&captured_server.key)
                || !review.captured.still_matches(&settings.servers.snapshot)
                || review.action.assessment(current).as_ref() != Some(&token)
                || !review.action.allowed(current)
            {
                return;
            }
            let Some(confirmation) = review.action.confirmation(current) else {
                return;
            };
            let Some(request) = review.action.execute(current) else {
                return;
            };
            settings.present_confirmation(
                confirmation,
                request,
                move |server| review.action.allowed(server),
                window,
                cx,
            );
        });
    }

    /// Asks before a request that ends work, in the words `confirmation` gives for the server.
    fn confirm_server_request(
        &mut self,
        key: &ServerKey,
        confirmation: impl FnOnce(&ServerSummary) -> Confirmation,
        request: ServerRequest,
        allowed: impl Fn(&ServerSummary) -> bool + 'static,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(server) = self.servers.find(key) else {
            return;
        };
        self.present_confirmation(confirmation(server), request, allowed, window, cx);
    }

    /// Presents `confirmation` and sends `request` when the person confirms. The server can change
    /// while the confirmation is open, so the request goes out only if its server is still listed
    /// and `allowed` still holds for it then. A refused request is never kept for later.
    fn present_confirmation(
        &mut self,
        confirmation: Confirmation,
        request: ServerRequest,
        allowed: impl Fn(&ServerSummary) -> bool + 'static,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(bound) = BoundServerRequest::bind(request, &self.servers.snapshot) else {
            return;
        };
        let confirm = ModalAction::new(
            true,
            confirmation.confirm,
            ModalActionRole::Affirmative,
            format!("{}-confirm", confirmation.id),
        )
        .with_emphasis(ModalActionEmphasis::Prominent);
        // A destructive confirmation leaves Cancel as the default, so Return cannot end work.
        let confirm = if confirmation.destructive {
            confirm.with_intent(ModalActionIntent::Destructive)
        } else {
            confirm.default_action(true)
        };
        let cancel = ModalAction::new(
            false,
            "Cancel",
            ModalActionRole::Cancel,
            format!("{}-cancel", confirmation.id),
        );
        let mut alert = Alert::new(
            ModalId::new(confirmation.id),
            confirmation.title.clone(),
            confirmation.title,
            confirmation.message,
            vec![confirm, cancel],
        )
        .intent(if confirmation.destructive {
            AlertIntent::Critical
        } else {
            AlertIntent::Warning
        });
        if let Some(detail) = confirmation.detail {
            alert = alert.detail(detail);
        }
        let owner = cx.weak_entity();
        let handle = window.window_handle();
        let presented = alert.present(window, cx, move |outcome, cx| {
            if !matches!(
                outcome,
                spaceterm_ui::AlertOutcome::Activated {
                    action_id: true,
                    ..
                }
            ) {
                return;
            }
            let _ = owner.update(cx, |settings, cx| {
                // Another window may have unpaired the server, or its connection or state may have
                // changed, since the person was asked.
                let still_allowed = bound
                    .request
                    .server()
                    .and_then(|server| settings.servers.find(server))
                    .is_some_and(&allowed);
                if !still_allowed || !bound.still_matches(&settings.servers.snapshot) {
                    let owner = cx.weak_entity();
                    cx.defer(move |cx| {
                        let _ = handle.update(cx, |_, window, cx| {
                            let _ = owner.update(cx, |settings, cx| {
                                settings.present_changed_confirmation(window, cx);
                            });
                        });
                    });
                    return;
                }
                settings.send_bound_server_request(bound, cx);
                cx.notify();
            });
        });
        if presented.is_err() {
            eprintln!("failed to present a SpaceTerm server confirmation");
        }
    }

    fn present_changed_confirmation(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let alert = Alert::new(
            ModalId::new("settings-server-confirmation-changed"),
            "Server changed",
            "Server changed",
            "Nothing was submitted. Review the server and the action again before confirming.",
            vec![ModalAction::new(
                false,
                "Cancel",
                ModalActionRole::Cancel,
                "settings-server-confirmation-changed-cancel",
            )],
        )
        .intent(AlertIntent::Warning);
        let _ = alert.present(window, cx, |_, _| {});
    }

    fn render_server_worktree_location(
        &mut self,
        server: &ServerSummary,
        appearance: &ChromeAppearance,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        self.synchronize_server_worktree_location(server, cx);
        // Until a live connection reads the setting there is no value to show or edit, and an
        // empty field would read as an empty location.
        if server.worktree_location.is_none() {
            return text_value(
                "settings-server-worktree-location-frame",
                SharedString::new_static("Unknown"),
                appearance,
            );
        }
        let input = self.servers.worktree.input.clone();
        let editable = server.connected();
        input.update(cx, |input, cx| input.set_editable(editable, cx));
        let edit_state = self.servers.location_edit_state(server);
        let invalid = matches!(edit_state, Some(SettingEditState::Rejected(_)));
        let focus = input.read(cx).focus_handle();
        let colors = appearance.host_colors(spaceterm_ui::ControlHost::Card);
        let field = spaceterm_ui::field_frame(
            "settings-server-worktree-location-frame",
            &focus,
            FieldState::default().disabled(!editable).invalid(invalid),
            RadiusRole::Control.pixels(),
            cx,
        )
        .debug_selector(|| "settings-server-worktree-location-frame".to_owned())
        .h(appearance.height(28.0, 13.0))
        .w_full()
        .min_w_0()
        .flex()
        .items_center()
        .px(appearance.spacing(8.0))
        .chrome_text(appearance.typography.style(TextRole::Body))
        .text_color(gpui_color(colors.text))
        .on_click(move |_, window, cx| {
            focus.focus(window, cx);
            cx.stop_propagation();
        })
        .child(input);
        let conflict = server
            .worktree_location
            .as_ref()
            .filter(|_| edit_state == Some(SettingEditState::Conflict));
        let resolution = conflict.map(|setting| {
            let key = server.key.clone();
            let discard_key = key.clone();
            let draft = setting
                .edit
                .as_ref()
                .map(|edit| edit.draft.clone())
                .unwrap_or_default();
            let seen = setting.clone();
            let pairing = server.pairing.clone();
            div()
                .flex()
                .flex_col()
                .gap(appearance.spacing(6.0))
                .child(
                    div()
                        .debug_selector(|| "settings-server-worktree-location-conflict".to_owned())
                        .chrome_text(appearance.typography.style(TextRole::Secondary))
                        .text_color(gpui_color(colors.warning))
                        .whitespace_normal()
                        .child(gpui::Text::new(
                            SharedString::from("settings-server-worktree-location-conflict").into(),
                            copy::conflict_caption(&setting.value),
                        )),
                )
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .gap(appearance.spacing(8.0))
                        .child(server_button(
                            "settings-server-worktree-location-save-mine",
                            "Save My Change",
                            server.connected(),
                            move |settings, _, cx| {
                                if !settings.servers.find(&key).is_some_and(|server| {
                                    server.connected()
                                        && server.pairing == pairing
                                        && server.worktree_location.as_ref() == Some(&seen)
                                }) {
                                    return;
                                }
                                settings.send_server_worktree_location(
                                    &key,
                                    SentLocation::Change {
                                        text: draft.clone(),
                                        base_revision: seen.revision,
                                        seen: seen.clone(),
                                    },
                                    cx,
                                );
                            },
                            cx,
                        ))
                        .child(server_button(
                            "settings-server-worktree-location-discard-mine",
                            "Discard My Change",
                            true,
                            move |settings, _, cx| {
                                settings.send_server_worktree_location(
                                    &discard_key,
                                    SentLocation::Discard,
                                    cx,
                                );
                            },
                            cx,
                        )),
                )
        });
        div()
            .w_full()
            .min_w_0()
            .flex()
            .flex_col()
            .gap(appearance.spacing(8.0))
            .child(field)
            .children(resolution)
            .into_any_element()
    }

    /// Shows the server's value, or this device's unapplied change, unless the person is typing. A
    /// request the server hasn't answered shows what it sent.
    fn synchronize_server_worktree_location(
        &mut self,
        server: &ServerSummary,
        cx: &mut Context<Self>,
    ) {
        let field = &mut self.servers.worktree;
        let current = SharedString::from(field.input.read(cx).value().to_owned());
        if let Some(synced) = &field.synced
            && synced.pairing != server.pairing
            && current != synced.text
            && self
                .servers
                .snapshot
                .servers
                .iter()
                .any(|value| value.pairing == synced.pairing)
        {
            field.unsent.insert(
                synced.server.clone(),
                SyncedText {
                    text: current.clone(),
                    ..synced.clone()
                },
            );
        }
        let Some(setting) = &server.worktree_location else {
            if !field.synced.as_ref().is_some_and(|synced| {
                synced.server == server.key && synced.pairing == server.pairing
            }) {
                field
                    .input
                    .update(cx, |input, cx| input.set_value(String::new(), cx));
                field.synced = None;
            }
            return;
        };
        let retained = field
            .unsent
            .get(&server.key)
            .filter(|draft| draft.pairing == server.pairing)
            .cloned();
        let desired = match (retained.as_ref(), field.sent.get(&server.key)) {
            (Some(draft), _) => draft.text.clone(),
            (_, Some(sent)) => sent.shown(setting),
            (_, None) => setting
                .edit
                .as_ref()
                .map_or_else(|| setting.value.clone(), |edit| edit.draft.clone()),
        };
        let resting = match &field.synced {
            Some(synced) if synced.server == server.key && synced.pairing == server.pairing => {
                current == synced.text || current == desired
            }
            _ => true,
        };
        if !resting {
            // The person is typing. Their text stays, and so does the revision they started from.
            return;
        }
        if current != desired {
            let value = desired.to_string();
            field
                .input
                .update(cx, |input, cx| input.set_value(value, cx));
        }
        field.synced = Some(SyncedText {
            server: server.key.clone(),
            pairing: server.pairing.clone(),
            text: desired,
            revision: retained.map_or(setting.revision, |draft| draft.revision),
        });
    }

    /// Sends the field's text when it differs from what the field last showed.
    fn commit_server_worktree_location(&mut self, cx: &mut Context<Self>) {
        let Some(server) = self.servers.selected_server().cloned() else {
            return;
        };
        let Some(setting) = server.worktree_location.as_ref() else {
            return;
        };
        let Some(synced) = self.servers.worktree.synced.clone() else {
            return;
        };
        if synced.server != server.key || synced.pairing != server.pairing || !server.connected() {
            return;
        }
        let text = SharedString::from(self.servers.worktree.input.read(cx).value().to_owned());
        if text == synced.text && !self.servers.worktree.unsent.contains_key(&server.key) {
            return;
        }
        self.servers.worktree.unsent.remove(&server.key);
        self.servers.worktree.synced = Some(SyncedText {
            text: text.clone(),
            ..synced.clone()
        });
        let sent = if text == setting.value {
            // Typing the server's value back is the same as dropping the change.
            if self.servers.location_edit_state(&server).is_none() {
                cx.notify();
                return;
            }
            SentLocation::Discard
        } else {
            SentLocation::Change {
                text,
                base_revision: synced.revision,
                seen: setting.clone(),
            }
        };
        self.send_server_worktree_location(&server.key, sent, cx);
    }

    /// Sends a New Worktree Location request, and shows what it sent until the server answers.
    fn send_server_worktree_location(
        &mut self,
        server: &ServerKey,
        sent: SentLocation,
        cx: &mut Context<Self>,
    ) {
        let request = sent.request(server);
        self.servers.worktree.sent.insert(server.clone(), sent);
        self.send_server_request(request, cx);
        cx.notify();
    }

    /// Escape returns the field to the server's value and drops any unapplied change.
    fn revert_server_worktree_location(&mut self, cx: &mut Context<Self>) {
        let Some(server) = self.servers.selected_server().cloned() else {
            return;
        };
        let Some(setting) = server.worktree_location.as_ref() else {
            return;
        };
        self.servers.worktree.unsent.remove(&server.key);
        let value = setting.value.clone();
        let text = value.to_string();
        self.servers
            .worktree
            .input
            .update(cx, |input, cx| input.set_value(text, cx));
        self.servers.worktree.synced = Some(SyncedText {
            server: server.key.clone(),
            pairing: server.pairing.clone(),
            text: value,
            revision: setting.revision,
        });
        if self.servers.location_edit_state(&server).is_some() {
            self.send_server_worktree_location(&server.key, SentLocation::Discard, cx);
        }
        cx.notify();
    }

    fn server_worktree_location_caption(
        &self,
        server: &ServerSummary,
        cx: &App,
    ) -> Option<(SharedString, CaptionTone)> {
        let name = &server.name;
        if !server.connected() {
            return Some((
                format!("Connect to {name} to change its settings.").into(),
                CaptionTone::Guidance,
            ));
        }
        if server.worktree_location.is_none() {
            return Some((
                format!("Waiting for {name}'s settings…").into(),
                CaptionTone::Guidance,
            ));
        }
        match self.servers.location_edit_state(server) {
            Some(SettingEditState::Saving) => {
                Some((format!("Saving to {name}…").into(), CaptionTone::Guidance))
            }
            Some(SettingEditState::Rejected(rejection)) => Some((
                copy::rejection_caption(name, &rejection),
                CaptionTone::Error,
            )),
            // The conflict explains itself beside its two choices.
            Some(SettingEditState::Conflict) => None,
            None => {
                let text = self.servers.worktree.input.read(cx).value();
                // The server validates the template. A template this device can't expand still
                // gets guidance, without an example it can't show.
                let guidance = if validate(text).is_ok() {
                    super::git::worktree_location_caption(text, None).0
                } else {
                    SharedString::new_static("Use {repository} and {branch}.")
                };
                Some((
                    format!("Shared by every device paired with {name}. {guidance}").into(),
                    CaptionTone::Guidance,
                ))
            }
        }
    }

    fn render_remote_access(
        &mut self,
        server: &ServerSummary,
        appearance: &ChromeAppearance,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let enabled = server
            .remote_access
            .as_ref()
            .is_some_and(|remote| remote.enabled);
        // Turning remote connections off needs no addresses, so it stays possible after the
        // server stops offering the ones they use.
        let available = adjustable_remote_access(server)
            .is_some_and(|remote| remote.enabled || !remote.scopes.is_empty());
        let owner = cx.weak_entity();
        let key = server.key.clone();
        let toggle = Switch::new(
            "settings-server-remote-access",
            "Allow remote connections",
            enabled,
        )
        .size(ToggleSize::Regular)
        .label_hidden(true)
        .disabled(!available)
        .debug_selector("settings-server-remote-access")
        .on_change(move |change, window, cx| {
            let requested = change.requested();
            let key = key.clone();
            let _ = owner.update(cx, |settings, cx| {
                settings.request_remote_access(&key, requested, window, cx);
            });
        });
        let resolution = server.remote_access.as_ref().and_then(|remote| {
            let edit = remote.edit.as_ref()?;
            if edit.state == SettingEditState::Saving { return None; }
            let current = if remote.enabled {
                format!("On ({})", remote.endpoints.iter().map(ToString::to_string).collect::<Vec<_>>().join(", "))
            } else { "Off".to_owned() };
            let draft = if edit.draft.is_enabled() {
                format!("On ({})", edit.draft.endpoints().iter().map(ToString::to_string).collect::<Vec<_>>().join(", "))
            } else { "Off".to_owned() };
            let lead = if edit.state == SettingEditState::Conflict { "Another device changed remote access." } else { "The server did not save remote access." };
            let text: SharedString = format!("{lead} Current value on {}: {current}. Your change: {draft}. Review before saving again.", server.name).into();
            let announced = text.clone();
            let key = server.key.clone();
            let discard = key.clone();
            let pairing = server.pairing.clone();
            let seen = remote.clone();
            let retryable = !edit.draft.is_enabled() || remote.scopes.iter().any(|scope| scope.endpoints == edit.draft.endpoints());
            Some(div()
                .id("settings-server-remote-access-conflict")
                .debug_selector(|| "settings-server-remote-access-conflict".to_owned())
                .role(accesskit::Role::Group)
                .aria_label("Remote access change")
                .a11y_synthetic_children(move |builder| {
                    let mut status = accesskit::Node::new(accesskit::Role::Status);
                    status.set_label(announced.as_ref());
                    status.set_live(accesskit::Live::Polite);
                    builder.push_child(builder.synthetic_node_id("remote-conflict"), status);
                })
                .flex().flex_col().gap(appearance.spacing(6.0))
                .child(div().whitespace_normal().chrome_text(appearance.typography.style(TextRole::Secondary)).text_color(gpui_color(appearance.colors.warning)).child(text))
                .child(div().flex().flex_row().gap(appearance.spacing(8.0))
                    .child(server_button("settings-server-remote-access-save-mine", "Save My Change…", server.connected() && !remote.pending && retryable, move |settings, window, cx| {
                        let Some(server) = settings.servers.find(&key).cloned().filter(|server| server.pairing == pairing && server.remote_access.as_ref() == Some(&seen)) else { return; };
                        let Some(edit) = &seen.edit else { return; };
                        if !edit.draft.is_enabled() {
                            settings.present_confirmation(copy::disable_remote_access_confirmation(&server), ServerRequest::DisableRemoteAccess(key.clone()), |server| adjustable_remote_access(server).is_some(), window, cx);
                        } else if let Some(scope) = seen.scopes.iter().find(|scope| scope.endpoints == edit.draft.endpoints()).cloned() {
                            let prompt = if seen.enabled { copy::change_scope_confirmation(&server, &scope) } else { copy::enable_remote_access_confirmation(&server, &scope) };
                            settings.present_confirmation(prompt, ServerRequest::EnableRemoteAccess {server: key.clone(), scope: scope.id.clone()}, move |server| adjustable_remote_access(server).is_some_and(|remote| remote.scopes.contains(&scope)), window, cx);
                        }
                    }, cx))
                    .child(server_button("settings-server-remote-access-discard-mine", "Discard My Change", !remote.pending, move |settings, _, cx| settings.send_server_request(ServerRequest::DiscardRemoteAccessEdit(discard.clone()), cx), cx)))
                .into_any_element())
        });
        div()
            .flex()
            .flex_col()
            .min_w_0()
            .gap(appearance.spacing(8.0))
            .child(toggle)
            .children(resolution)
            .into_any_element()
    }

    /// Turning remote connections on asks, naming the server and the scope. Turning them off asks
    /// too, because every device connected over the network disconnects, possibly this one.
    fn request_remote_access(
        &mut self,
        key: &ServerKey,
        enabled: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(server) = self.servers.find(key).cloned() else {
            return;
        };
        if !enabled {
            self.present_confirmation(
                copy::disable_remote_access_confirmation(&server),
                ServerRequest::DisableRemoteAccess(key.clone()),
                |server| adjustable_remote_access(server).is_some_and(|remote| remote.enabled),
                window,
                cx,
            );
            return;
        }
        let Some(scope) = self.servers.shown_scope(&server) else {
            return;
        };
        let request = ServerRequest::EnableRemoteAccess {
            server: key.clone(),
            scope: scope.id.clone(),
        };
        let confirmation = copy::enable_remote_access_confirmation(&server, &scope);
        // The confirmation names the scope's addresses, so it holds only while the server still
        // offers them unchanged and nobody has turned remote connections on meanwhile.
        self.present_confirmation(
            confirmation,
            request,
            move |server| {
                adjustable_remote_access(server)
                    .is_some_and(|remote| !remote.enabled && remote.scopes.contains(&scope))
            },
            window,
            cx,
        );
    }

    fn render_remote_access_scope(
        &mut self,
        server: &ServerSummary,
        appearance: &ChromeAppearance,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(remote) = server
            .remote_access
            .as_ref()
            .filter(|remote| !remote.scopes.is_empty())
        else {
            let listening = server
                .remote_access
                .as_ref()
                .is_some_and(|remote| remote.enabled);
            return text_value(
                "settings-server-remote-access-scope",
                SharedString::new_static(if listening {
                    "Unavailable"
                } else {
                    "None offered"
                }),
                appearance,
            );
        };
        // While remote connections are on, addresses the server no longer offers read as
        // unavailable rather than as one of the offered scopes.
        let shown = self.servers.shown_scope(server).map(|scope| scope.id);
        let prompt = if remote.enabled {
            "Unavailable"
        } else {
            "Choose addresses"
        };
        let items = remote
            .scopes
            .iter()
            .map(|scope| {
                ComboBoxItem::new(scope.id.clone(), scope.label.clone())
                    .description(scope.addresses.clone())
                    .debug_selector(format!("settings-server-remote-access-scope-{}", scope.id))
            })
            .collect();
        let owner = cx.weak_entity();
        let key = server.key.clone();
        settings_selector(
            "settings-server-remote-access-scope".to_owned(),
            "Listen on",
            shown,
            prompt,
            items,
            appearance,
        )
        .disabled(adjustable_remote_access(server).is_none())
        .on_accept(move |acceptance, window, cx| {
            let scope = acceptance.item_id().clone();
            let key = key.clone();
            let _ = owner.update(cx, |settings, cx| {
                settings.choose_remote_access_scope(&key, scope, window, cx);
            });
        })
        .into_any_element()
    }

    /// While remote connections are off the choice waits for them to be turned on. While they are
    /// on, moving them disconnects devices on the other addresses, so it asks first.
    fn choose_remote_access_scope(
        &mut self,
        key: &ServerKey,
        scope: SharedString,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(server) = self.servers.find(key).cloned() else {
            return;
        };
        let Some(remote) = adjustable_remote_access(&server) else {
            return;
        };
        if !remote.enabled {
            self.servers.proposed_scopes.insert(key.clone(), scope);
            cx.notify();
            return;
        }
        if remote.scope.as_ref() == Some(&scope) {
            return;
        }
        let Some(choice) = remote
            .scopes
            .iter()
            .find(|candidate| candidate.id == scope)
            .cloned()
        else {
            return;
        };
        let confirmation = copy::change_scope_confirmation(&server, &choice);
        self.present_confirmation(
            confirmation,
            ServerRequest::EnableRemoteAccess {
                server: key.clone(),
                scope,
            },
            move |server| {
                adjustable_remote_access(server).is_some_and(|remote| {
                    remote.enabled
                        && remote.scope.as_ref() != Some(&choice.id)
                        && remote.scopes.contains(&choice)
                })
            },
            window,
            cx,
        );
    }
}

/// A read-only value at the row's trailing edge, published as text.
fn text_value(
    selector: &'static str,
    value: SharedString,
    appearance: &ChromeAppearance,
) -> AnyElement {
    let card = appearance.host_colors(spaceterm_ui::ControlHost::Card);
    div()
        .debug_selector(move || selector.to_owned())
        .min_w_0()
        .truncate()
        .chrome_text(appearance.typography.style(TextRole::Body))
        .text_color(gpui_color(card.text))
        .child(gpui::Text::new(
            SharedString::from(format!("{selector}-text")).into(),
            value,
        ))
        .into_any_element()
}

/// Each permission the server reports, named as the server's system names it, with its state in
/// a badge as the Privacy section shows this device's own.
fn render_server_permissions(server: &ServerSummary, appearance: &ChromeAppearance) -> AnyElement {
    let card = appearance.host_colors(spaceterm_ui::ControlHost::Card);
    let Some(permissions) = &server.permissions else {
        return text_value(
            "settings-row-server-permissions-control",
            SharedString::new_static("Not reported"),
            appearance,
        );
    };
    div()
        .id("settings-server-permissions")
        .role(accesskit::Role::List)
        .aria_label("System permissions")
        .debug_selector(|| "settings-row-server-permissions-control".to_owned())
        .w_full()
        .flex()
        .flex_col()
        .gap(appearance.spacing(6.0))
        .children(permissions.entries.iter().map(|entry| {
            let name = copy::permission_name(entry.kind, permissions.naming);
            let state = copy::permission_state(entry.state);
            let (item, state_badge) = permission_selectors(entry.kind);
            div()
                .id(item)
                .role(accesskit::Role::ListItem)
                .aria_label(SharedString::from(format!("{name}: {state}")))
                .debug_selector(move || item.to_owned())
                .flex()
                .flex_row()
                .items_center()
                .justify_between()
                .gap(appearance.spacing(12.0))
                .child(
                    div()
                        .chrome_text(appearance.typography.style(TextRole::Body))
                        .text_color(gpui_color(card.text))
                        .child(name),
                )
                .child(
                    div()
                        .debug_selector(move || state_badge.to_owned())
                        .child(badge(state_badge, state, appearance)),
                )
        }))
        .into_any_element()
}

/// The selectors of one permission's list item and of its state badge.
fn permission_selectors(kind: ServerPermissionKind) -> (&'static str, &'static str) {
    match kind {
        ServerPermissionKind::Microphone => (
            "settings-server-permission-microphone",
            "settings-server-permission-microphone-state",
        ),
        ServerPermissionKind::ScreenRecording => (
            "settings-server-permission-screen-recording",
            "settings-server-permission-screen-recording-state",
        ),
        ServerPermissionKind::Accessibility => (
            "settings-server-permission-accessibility",
            "settings-server-permission-accessibility-state",
        ),
    }
}
