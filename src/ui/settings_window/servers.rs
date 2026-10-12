//! The Servers section: the servers this device is paired with, the selected server's status, the
//! settings it owns, its permissions, and managing it.
//!
//! The section reads published server state and sends requests. It never writes server state to
//! the Settings Document, and every request that ends work for other clients asks first.

mod copy;
pub(crate) mod model;

#[cfg(test)]
#[path = "servers_tests.rs"]
mod tests;

use std::collections::{HashMap, HashSet};

use gpui::prelude::*;
use gpui::{
    AnyElement, App, Context, Entity, FocusHandle, KeyDownEvent, SharedString, Subscription,
    Window, accesskit, div, px,
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
use crate::ui::sidebar_window::form::{CaptionTone, action_button, row_horizontal_inset};
use copy::{Confirmation, StatusTone};
use model::{
    BindScope, IncompatibleSide, InstalledServers, PendingUnpair, ServerKey, ServerRequest,
    ServerStatus, ServerSummary, Servers, ServersSnapshot, SettingEditState,
};

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
            | SettingsRowId::ServerUnpair
    )
}

/// The Servers section's own state: the latest published servers and the person's choices.
pub(super) struct ServersSection {
    snapshot: ServersSnapshot,
    source: Option<Entity<Servers>>,
    _source_changes: Option<Subscription>,
    _installation: Subscription,
    /// The server the person chose. The first listed server stands in until they choose one.
    selected: Option<ServerKey>,
    list_focus: FocusHandle,
    worktree: WorktreeField,
    /// Bind scopes chosen while remote connections are off, used when they are turned on.
    proposed_scopes: HashMap<ServerKey, SharedString>,
    /// Notices dismissed here that the next snapshot has not dropped yet.
    dismissed: HashSet<SharedString>,
}

/// The selected server's New Worktree Location field. It sends a change on Return or when focus
/// leaves; Escape returns it to the server's value.
struct WorktreeField {
    input: Entity<TextInput>,
    /// The text the field last took from the published setting, and the revision the person edits
    /// from. Text that differs from it while the field has focus is the person's typing.
    synced: Option<SyncedText>,
    _events: Subscription,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct SyncedText {
    server: ServerKey,
    text: SharedString,
    revision: u64,
}

impl ServersSection {
    pub(super) fn new(window: &mut Window, cx: &mut Context<SettingsWindow>) -> Self {
        let list_focus = cx.focus_handle().tab_stop(true);
        cx.on_focus(&list_focus, window, |_, _, cx| cx.notify())
            .detach();
        cx.on_blur(&list_focus, window, |_, _, cx| cx.notify())
            .detach();
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
            list_focus,
            worktree: WorktreeField {
                input,
                synced: None,
                _events: events,
            },
            proposed_scopes: HashMap::new(),
            dismissed: HashSet::new(),
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
            snapshot
                .pending_unpairs
                .iter()
                .any(|pending| pending.id == *id)
        });
        self.proposed_scopes
            .retain(|key, _| snapshot.servers.iter().any(|server| server.key == *key));
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
        self.selected
            .as_ref()
            .and_then(|key| self.find(key))
            .or_else(|| self.ordered().next())
    }

    /// Whether Settings omits `row` because there is no server for it to present.
    pub(super) fn omits(&self, row: SettingsRowId) -> bool {
        presents_selected_server(row) && self.selected_server().is_none()
    }

    /// The server rows answer for the server the list selects, so the list stays above any of
    /// them a search finds.
    pub(super) fn complete(&self, rows: &mut Vec<SettingsRowId>) {
        if rows.iter().copied().any(presents_selected_server)
            && !rows.contains(&SettingsRowId::ServerList)
        {
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

    /// The scope a change to remote connections uses: the server's while they are on, otherwise
    /// the one the person chose, the one the server used last, or the first it offers.
    fn chosen_scope(&self, server: &ServerSummary) -> Option<BindScope> {
        let remote = server.remote_access.as_ref()?;
        let proposed = (!remote.enabled)
            .then(|| self.proposed_scopes.get(&server.key))
            .flatten();
        proposed
            .or(remote.scope.as_ref())
            .and_then(|id| remote.scopes.iter().find(|scope| scope.id == *id))
            .or_else(|| remote.scopes.first())
            .cloned()
    }
}

/// Where one option sits in the server list, and whether the list holds keyboard focus.
#[derive(Clone, Copy, Debug)]
struct OptionPlacement {
    selected: bool,
    list_focused: bool,
    /// The option's 1-based position among every listed server.
    position: usize,
    total: usize,
}

/// What the Status row offers.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum StatusAction {
    Start { enabled: bool },
    Stop { enabled: bool },
}

/// Start needs host access through the service manager or SSH. Stop needs either a live
/// connection or host access.
fn status_action(server: &ServerSummary) -> Option<StatusAction> {
    let host = server.route.manages_host();
    match server.status {
        ServerStatus::Stopped => Some(StatusAction::Start { enabled: host }),
        ServerStatus::Running => Some(StatusAction::Stop { enabled: true }),
        ServerStatus::Starting | ServerStatus::Incompatible(_) => {
            Some(StatusAction::Stop { enabled: host })
        }
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

fn version_action(server: &ServerSummary) -> VersionAction {
    let host = server.route.manages_host();
    match &server.staged_version {
        Some(version) => VersionAction::Update {
            version: version.clone(),
            enabled: match server.status {
                ServerStatus::Running => true,
                ServerStatus::Stopped | ServerStatus::Incompatible(IncompatibleSide::Server) => {
                    host
                }
                _ => false,
            },
        },
        None => VersionAction::Restart {
            enabled: server.connected(),
        },
    }
}

fn update_label(server: &ServerSummary) -> &'static str {
    if server.status == ServerStatus::Stopped {
        "Install Update…"
    } else {
        "Update and Restart…"
    }
}

fn tone_color(tone: StatusTone, colors: &ChromeColors) -> crate::appearance::Color {
    match tone {
        StatusTone::Positive => colors.success,
        StatusTone::Neutral => colors.icon_muted,
        StatusTone::Attention => colors.warning,
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
    /// Follows a server source installed after this window opened.
    fn follow_servers(&mut self, cx: &mut Context<Self>) {
        if self.servers.follow(cx) {
            self.synchronize_search_results();
            cx.notify();
        }
    }

    /// Sends one request to the installed servers.
    fn send_server_request(&mut self, request: ServerRequest, cx: &mut Context<Self>) {
        let Some(servers) = self.servers.source.clone() else {
            return;
        };
        let requests = servers.read(cx).requests();
        requests.send(request, cx);
    }

    fn select_server(&mut self, key: ServerKey, cx: &mut Context<Self>) {
        if self.servers.selected.as_ref() != Some(&key) {
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
        match row {
            SettingsRowId::ServerStatus => self.render_server_status(&server, appearance, cx),
            SettingsRowId::ServerVersion => self.render_server_version(&server, appearance, cx),
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
                    server.connected(),
                    move |settings, window, cx| {
                        settings.confirm_server_request(
                            &key,
                            copy::end_all_sessions_confirmation,
                            ServerRequest::EndAllSessions(key.clone()),
                            window,
                            cx,
                        );
                    },
                    cx,
                )
            }
            SettingsRowId::ServerWorktreeLocation => {
                self.render_server_worktree_location(&server, appearance, cx)
            }
            SettingsRowId::ServerPermissions => render_server_permissions(&server, appearance),
            SettingsRowId::ServerRemoteAccess => self.render_remote_access(&server, cx),
            SettingsRowId::ServerRemoteAccessScope => {
                self.render_remote_access_scope(&server, appearance, cx)
            }
            SettingsRowId::ServerUnpair => {
                let key = server.key.clone();
                server_button(
                    "settings-server-unpair",
                    "Unpair…",
                    true,
                    move |settings, window, cx| {
                        settings.confirm_server_request(
                            &key,
                            copy::unpair_confirmation,
                            ServerRequest::UnpairThisDevice(key.clone()),
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
                let tone = if matches!(server.status, ServerStatus::Incompatible(_)) {
                    CaptionTone::Warning
                } else {
                    CaptionTone::Guidance
                };
                Some((copy::status_explanation(server), tone))
            }
            SettingsRowId::ServerVersion => match (&server.staged_version, &server.status) {
                (Some(version), ServerStatus::Stopped) => guidance(
                    format!("Version {version} is ready. {name} stays stopped after it installs.")
                        .into(),
                ),
                (Some(version), _) => guidance(
                    format!("Version {version} is ready. Installing it restarts {name}.").into(),
                ),
                (None, _) => None,
            },
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
                    Some(remote) if remote.pending => {
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
                let scope = self.servers.chosen_scope(server)?;
                if remote.enabled {
                    guidance(scope.addresses)
                } else {
                    guidance(
                        format!("{}. Used when remote connections are on.", scope.addresses)
                            .into(),
                    )
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
        // A list with keyboard focus shows its selection in the accent color, as a native table
        // does. Without focus it shows the resting selection colors.
        let selection = if focused && appearance.active {
            crate::ui::selection_chip::emphasized_selection_colors(card)
        } else if appearance.active {
            appearance
                .unfocused_selection_colors(spaceterm_ui::ControlHost::Card)
                .clone()
        } else {
            card.clone()
        };
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
        let status_color = if selected && focused && appearance.active {
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

    /// The selected server's name above the rows that present it.
    pub(super) fn render_selected_server_heading(
        &self,
        appearance: &ChromeAppearance,
    ) -> Option<AnyElement> {
        let server = self.servers.selected_server()?;
        Some(
            div()
                .id("settings-server-heading")
                .role(accesskit::Role::Heading)
                .aria_level(2)
                .aria_label(server.name.clone())
                .debug_selector(|| "settings-server-heading".to_owned())
                .px(row_horizontal_inset(appearance))
                .flex()
                .flex_row()
                .items_baseline()
                .gap(appearance.spacing(8.0))
                .child(
                    div()
                        .min_w_0()
                        .truncate()
                        .chrome_text(appearance.typography.style(TextRole::Title))
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

    /// One dismissible notice per pending unpair, above the list.
    pub(super) fn render_unpair_notices(
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
                let id = pending.id.clone();
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
                                    .on_activate(move |_, _, cx| {
                                        let id = id.clone();
                                        let _ = owner.update(cx, |settings, cx| {
                                            settings.dismiss_unpair_notice(id, cx);
                                        });
                                    }),
                            ),
                    )
                    .into_any_element()
            })
            .collect()
    }

    /// Hides the notice now. The background revocation keeps retrying.
    fn dismiss_unpair_notice(&mut self, id: SharedString, cx: &mut Context<Self>) {
        self.servers.dismissed.insert(id.clone());
        self.send_server_request(ServerRequest::DismissUnpairNotice(id), cx);
        cx.notify();
    }

    fn render_server_status(
        &mut self,
        server: &ServerSummary,
        appearance: &ChromeAppearance,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let card = appearance.host_colors(spaceterm_ui::ControlHost::Card);
        let key = server.key.clone();
        let action = status_action(server).map(|action| match action {
            StatusAction::Start { enabled } => server_button(
                "settings-server-start",
                "Start Server",
                enabled,
                move |settings, _, cx| {
                    settings.send_server_request(ServerRequest::Start(key.clone()), cx);
                },
                cx,
            ),
            StatusAction::Stop { enabled } => server_button(
                "settings-server-stop",
                "Stop Server…",
                enabled,
                move |settings, window, cx| {
                    settings.confirm_server_request(
                        &key,
                        copy::stop_confirmation,
                        ServerRequest::Stop(key.clone()),
                        window,
                        cx,
                    );
                },
                cx,
            ),
        });
        div()
            .debug_selector(|| "settings-row-server-status-control".to_owned())
            .flex()
            .flex_row()
            .items_center()
            .gap(appearance.spacing(10.0))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(appearance.spacing(4.0))
                    .child(Icon::new(
                        copy::status_icon(&server.status),
                        appearance.icons.metrics(IconRole::Status).glyph_size,
                        gpui_color(tone_color(copy::status_tone(&server.status), card)),
                    ))
                    .child(
                        div()
                            .chrome_text(appearance.typography.style(TextRole::Body))
                            .text_color(gpui_color(card.text))
                            .child(gpui::Text::new(
                                SharedString::from("settings-server-status-text").into(),
                                copy::status_label(&server.status),
                            )),
                    ),
            )
            .children(action)
            .into_any_element()
    }

    fn render_server_version(
        &mut self,
        server: &ServerSummary,
        appearance: &ChromeAppearance,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let card = appearance.host_colors(spaceterm_ui::ControlHost::Card);
        let key = server.key.clone();
        let action = match version_action(server) {
            VersionAction::Restart { enabled } => server_button(
                "settings-server-restart",
                "Restart…",
                enabled,
                move |settings, window, cx| {
                    settings.confirm_server_request(
                        &key,
                        copy::restart_confirmation,
                        ServerRequest::Restart(key.clone()),
                        window,
                        cx,
                    );
                },
                cx,
            ),
            VersionAction::Update { version, enabled } => server_button(
                "settings-server-update",
                update_label(server),
                enabled,
                move |settings, window, cx| {
                    let version = version.clone();
                    let request = ServerRequest::InstallUpdate {
                        server: key.clone(),
                        version: version.clone(),
                    };
                    settings.confirm_server_request(
                        &key,
                        move |server| copy::update_confirmation(server, &version),
                        request,
                        window,
                        cx,
                    );
                },
                cx,
            ),
        };
        div()
            .debug_selector(|| "settings-row-server-version-control".to_owned())
            .flex()
            .flex_row()
            .items_center()
            .gap(appearance.spacing(10.0))
            .child(
                div()
                    .chrome_text(appearance.typography.style(TextRole::Body))
                    .text_color(gpui_color(card.text))
                    .child(gpui::Text::new(
                        SharedString::from("settings-server-version-text").into(),
                        server
                            .version
                            .clone()
                            .unwrap_or_else(|| SharedString::new_static("Unknown")),
                    )),
            )
            .child(action)
            .into_any_element()
    }

    /// Asks before a request that ends work, then sends it if the server is still listed.
    fn confirm_server_request(
        &mut self,
        key: &ServerKey,
        confirmation: impl FnOnce(&ServerSummary) -> Confirmation,
        request: ServerRequest,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(server) = self.servers.find(key) else {
            return;
        };
        self.present_confirmation(confirmation(server), request, window, cx);
    }

    fn present_confirmation(
        &mut self,
        confirmation: Confirmation,
        request: ServerRequest,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
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
                settings.send_server_request(request, cx);
                cx.notify();
            });
        });
        if presented.is_err() {
            eprintln!("failed to present a SpaceTerm server confirmation");
        }
    }

    fn render_server_worktree_location(
        &mut self,
        server: &ServerSummary,
        appearance: &ChromeAppearance,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        self.synchronize_server_worktree_location(server, cx);
        let input = self.servers.worktree.input.clone();
        let editable = server.connected() && server.worktree_location.is_some();
        input.update(cx, |input, cx| input.set_editable(editable, cx));
        let edit = server
            .worktree_location
            .as_ref()
            .and_then(|setting| setting.edit.as_ref());
        let invalid = matches!(
            edit.map(|edit| &edit.state),
            Some(SettingEditState::Rejected(_))
        );
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
        let conflict = server.worktree_location.as_ref().filter(|setting| {
            setting
                .edit
                .as_ref()
                .is_some_and(|edit| edit.state == SettingEditState::Conflict)
        });
        let resolution = conflict.map(|setting| {
            let key = server.key.clone();
            let discard_key = key.clone();
            let draft = setting
                .edit
                .as_ref()
                .map(|edit| edit.draft.clone())
                .unwrap_or_default();
            let revision = setting.revision;
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
                            true,
                            move |settings, _, cx| {
                                settings.send_server_request(
                                    ServerRequest::SetWorktreeLocation {
                                        server: key.clone(),
                                        template: draft.clone(),
                                        base_revision: revision,
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
                                settings.send_server_request(
                                    ServerRequest::DiscardWorktreeLocationEdit(discard_key.clone()),
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

    /// Shows the server's value, or this device's unapplied change, unless the person is typing.
    fn synchronize_server_worktree_location(
        &mut self,
        server: &ServerSummary,
        cx: &mut Context<Self>,
    ) {
        let field = &mut self.servers.worktree;
        let Some(setting) = &server.worktree_location else {
            if field.synced.as_ref().map(|synced| &synced.server) != Some(&server.key) {
                field
                    .input
                    .update(cx, |input, cx| input.set_value(String::new(), cx));
                field.synced = None;
            }
            return;
        };
        let desired = setting
            .edit
            .as_ref()
            .map_or_else(|| setting.value.clone(), |edit| edit.draft.clone());
        let current = SharedString::from(field.input.read(cx).value().to_owned());
        let resting = match &field.synced {
            Some(synced) if synced.server == server.key => {
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
            text: desired,
            revision: setting.revision,
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
        if synced.server != server.key || !server.connected() {
            return;
        }
        let text = SharedString::from(self.servers.worktree.input.read(cx).value().to_owned());
        if text == synced.text {
            return;
        }
        self.servers.worktree.synced = Some(SyncedText {
            text: text.clone(),
            ..synced.clone()
        });
        let request = if text == setting.value {
            // Typing the server's value back is the same as dropping the change.
            if setting.edit.is_none() {
                cx.notify();
                return;
            }
            ServerRequest::DiscardWorktreeLocationEdit(server.key.clone())
        } else {
            ServerRequest::SetWorktreeLocation {
                server: server.key.clone(),
                template: text,
                base_revision: synced.revision,
            }
        };
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
        let value = setting.value.clone();
        let text = value.to_string();
        self.servers
            .worktree
            .input
            .update(cx, |input, cx| input.set_value(text, cx));
        self.servers.worktree.synced = Some(SyncedText {
            server: server.key.clone(),
            text: value,
            revision: setting.revision,
        });
        if setting.edit.is_some() {
            self.send_server_request(
                ServerRequest::DiscardWorktreeLocationEdit(server.key.clone()),
                cx,
            );
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
        let Some(setting) = &server.worktree_location else {
            return Some((
                format!("Waiting for {name}'s settings…").into(),
                CaptionTone::Guidance,
            ));
        };
        match setting.edit.as_ref().map(|edit| &edit.state) {
            Some(SettingEditState::Saving) => {
                Some((format!("Saving to {name}…").into(), CaptionTone::Guidance))
            }
            Some(SettingEditState::Rejected(rejection)) => {
                Some((copy::rejection_caption(name, rejection), CaptionTone::Error))
            }
            // The conflict explains itself beside its two choices.
            Some(SettingEditState::Conflict) => None,
            None => {
                let text = self.servers.worktree.input.read(cx).value();
                let (example, _) = super::git::worktree_location_caption(text, None);
                Some((
                    format!("Shared by every device paired with {name}. {example}").into(),
                    CaptionTone::Guidance,
                ))
            }
        }
    }

    fn render_remote_access(
        &mut self,
        server: &ServerSummary,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let remote = server.remote_access.as_ref();
        let enabled = remote.is_some_and(|remote| remote.enabled);
        let available = server.connected()
            && remote.is_some_and(|remote| !remote.pending && !remote.scopes.is_empty());
        let owner = cx.weak_entity();
        let key = server.key.clone();
        Switch::new(
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
        })
        .into_any_element()
    }

    /// Turning remote connections on always asks, naming the server and the scope. Turning them
    /// off asks only when this device would lose its own connection.
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
        let Some(scope) = self.servers.chosen_scope(&server) else {
            return;
        };
        let request = ServerRequest::SetRemoteAccess {
            server: key.clone(),
            enabled,
            scope: scope.id.clone(),
        };
        if enabled {
            let confirmation = copy::enable_remote_access_confirmation(&server, &scope);
            self.present_confirmation(confirmation, request, window, cx);
        } else if matches!(server.route, model::ServerRoute::Wss { .. }) {
            let confirmation = copy::disable_remote_access_confirmation(&server);
            self.present_confirmation(confirmation, request, window, cx);
        } else {
            self.send_server_request(request, cx);
            cx.notify();
        }
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
            return text_value(
                "settings-server-remote-access-scope",
                SharedString::new_static("None offered"),
                appearance,
            );
        };
        let chosen = self.servers.chosen_scope(server).map(|scope| scope.id);
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
            chosen,
            "Choose addresses",
            items,
            appearance,
        )
        .disabled(!server.connected() || remote.pending)
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
    /// on, moving them asks first.
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
        let Some(remote) = server.remote_access.as_ref() else {
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
        let Some(choice) = remote.scopes.iter().find(|candidate| candidate.id == scope) else {
            return;
        };
        let confirmation = copy::change_scope_confirmation(&server, choice);
        self.present_confirmation(
            confirmation,
            ServerRequest::SetRemoteAccess {
                server: key.clone(),
                enabled: true,
                scope,
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
/// words.
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
        .gap(appearance.spacing(4.0))
        .children(permissions.entries.iter().map(|entry| {
            let name = copy::permission_name(entry.kind, permissions.naming);
            let state = copy::permission_state(entry.state);
            div()
                .id(SharedString::from(format!(
                    "settings-server-permission-{name}"
                )))
                .role(accesskit::Role::ListItem)
                .aria_label(SharedString::from(format!("{name}: {state}")))
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
                        .chrome_text(appearance.typography.style(TextRole::Body))
                        .text_color(gpui_color(card.text_muted))
                        .child(state),
                )
        }))
        .into_any_element()
}
