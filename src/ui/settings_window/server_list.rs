//! The Servers section's server list and the selection every Servers row acts on.
//!
//! The list names the local server first, then each saved SSH connection by destination, then
//! each direct network server this device is paired with by the name it gave. The selected
//! server's detail follows, and its headings name that machine, so every step identifies the
//! machine it affects. The list reads the local row and the registries' snapshots only, so
//! rendering reaches no computer and asks for no credential.

use gpui::prelude::*;
use gpui::{AnyElement, Context, KeyDownEvent, SharedString, Window, accesskit, div, px};
use spaceterm_ui::{Icon, IconName, ProgressRing, ProgressSize, ProgressState};

use super::SettingsWindow;
use super::catalog::{SettingsRowId, SettingsSectionId};
use crate::direct_connections::{DirectRouteId, DirectRouteStatus};
use crate::domain::SshDestination;
use crate::ssh::server_connection::RemoteConnectionStatus;
use crate::ui::appearance::{ChromeAppearance, gpui_color};
use crate::ui::chrome_geometry::RadiusRole;
use crate::ui::chrome_icons::IconRole;
use crate::ui::chrome_typography::{ChromeTextStyleExt as _, TextRole};
use crate::ui::sidebar_window::form::CaptionTone;

pub(super) const LIST_SELECTOR: &str = "settings-server-list";
/// The fewest characters of a server's name that search treats as naming it.
const NAME_QUERY_MINIMUM: usize = 3;
/// The list's name for assistive technology, distinct from the Servers section it sits in.
pub(super) const SERVER_LIST_NAME: &str = "Server list";

/// The server a Servers row acts on.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum ServerChoice {
    Local,
    Ssh(SshDestination),
    /// A server this device reaches over a paired direct network route.
    Direct(DirectRouteId),
}

/// One server the list offers, read from what SpaceTerm already knows about it.
struct ListedServer {
    choice: ServerChoice,
    name: SharedString,
    method: &'static str,
    state: &'static str,
    tone: StatusTone,
    working: bool,
}

/// The color a compact status takes.
#[derive(Clone, Copy)]
enum StatusTone {
    Connected,
    Neutral,
    Warning,
    Error,
}

impl StatusTone {
    fn of(connected: bool, tone: CaptionTone) -> Self {
        match tone {
            CaptionTone::Error => Self::Error,
            CaptionTone::Warning => Self::Warning,
            CaptionTone::Guidance if connected => Self::Connected,
            CaptionTone::Guidance => Self::Neutral,
        }
    }
}

/// The rows that present the local server alone.
const LOCAL_ROWS: [SettingsRowId; 3] = [
    SettingsRowId::LocalServer,
    SettingsRowId::ServerService,
    SettingsRowId::ServerUpdate,
];
/// The rows that present one saved SSH server alone.
const SSH_ROWS: [SettingsRowId; 8] = [
    SettingsRowId::SshServerStatus,
    SettingsRowId::SshServerService,
    SettingsRowId::SshServerUpdate,
    SettingsRowId::SshServerVersion,
    SettingsRowId::SshServerPlatform,
    SettingsRowId::SshServerCompatibility,
    SettingsRowId::SshServerStartup,
    SettingsRowId::RemoveServer,
];
/// The rows that present one direct network server alone.
const DIRECT_ROWS: [SettingsRowId; 2] = [
    SettingsRowId::DirectServerStatus,
    SettingsRowId::UnpairThisDevice,
];
/// The rows that present whichever server is selected.
const SELECTED_ROWS: [SettingsRowId; 5] = [
    SettingsRowId::EndAllSessions,
    SettingsRowId::ServerConnection,
    SettingsRowId::DirectNetworkAccess,
    SettingsRowId::DirectNetworkPairing,
    SettingsRowId::ServerWorktreeLocation,
];

/// Whether `row` presents something about `server`. Rows outside the Servers detail always do.
pub(super) fn row_presents(row: SettingsRowId, server: Option<&ServerChoice>) -> bool {
    if LOCAL_ROWS.contains(&row) {
        server == Some(&ServerChoice::Local)
    } else if SSH_ROWS.contains(&row) {
        matches!(server, Some(ServerChoice::Ssh(_)))
    } else if DIRECT_ROWS.contains(&row) {
        matches!(server, Some(ServerChoice::Direct(_)))
    } else if SELECTED_ROWS.contains(&row) {
        server.is_some()
    } else {
        true
    }
}

/// The row that leads a server's detail, which Settings Search reveals when the query names it.
fn leading_row(server: &ServerChoice) -> SettingsRowId {
    match server {
        ServerChoice::Local => SettingsRowId::LocalServer,
        ServerChoice::Ssh(_) => SettingsRowId::SshServerStatus,
        ServerChoice::Direct(_) => SettingsRowId::DirectServerStatus,
    }
}

impl SettingsWindow {
    /// The saved SSH connections and their statuses, from the registry's snapshot.
    pub(super) fn saved_ssh_servers(&self) -> Vec<(SshDestination, RemoteConnectionStatus)> {
        self.ssh_servers
            .as_ref()
            .map(|rows| rows.connections.connections())
            .unwrap_or_default()
    }

    /// The saved direct routes, their statuses and names, from the registry's snapshot.
    pub(super) fn saved_direct_servers(&self) -> Vec<(DirectRouteId, DirectRouteStatus, String)> {
        let Some(direct) = self.direct_routes.as_ref() else {
            return Vec::new();
        };
        direct
            .connections
            .routes()
            .into_iter()
            .filter_map(|(route, status)| {
                let label = direct.connections.route(&route)?.label;
                Some((route, status, label))
            })
            .collect()
    }

    fn listed_servers(&self) -> Vec<ListedServer> {
        let local = self.local_server_presentation().map(|presentation| {
            let connected = presentation.state_selector == "settings-local-server-state-connected";
            ListedServer {
                choice: ServerChoice::Local,
                name: SharedString::new_static(self.this_computer),
                method: "This account",
                state: presentation.state,
                tone: StatusTone::of(connected, presentation.tone),
                working: presentation.working,
            }
        });
        let ssh = self
            .saved_ssh_servers()
            .into_iter()
            .map(|(destination, status)| {
                let presentation = crate::ui::ssh_servers::present(&status);
                let connected = matches!(status, RemoteConnectionStatus::Connected { .. });
                ListedServer {
                    name: SharedString::from(destination.as_str().to_owned()),
                    choice: ServerChoice::Ssh(destination),
                    method: "SSH",
                    state: presentation.state,
                    tone: StatusTone::of(connected, presentation.tone),
                    working: presentation.working,
                }
            });
        let direct = self
            .saved_direct_servers()
            .into_iter()
            .map(|(route, status, label)| {
                let presentation = crate::ui::direct_servers::present(status, &label);
                ListedServer {
                    name: SharedString::from(label),
                    choice: ServerChoice::Direct(route),
                    method: "Direct network",
                    state: presentation.state,
                    tone: StatusTone::of(status == DirectRouteStatus::Connected, presentation.tone),
                    working: presentation.working,
                }
            });
        local.into_iter().chain(ssh).chain(direct).collect()
    }

    /// The server the Servers detail presents: the one the person selected while it is listed,
    /// otherwise the local server, otherwise the first saved SSH server.
    pub(super) fn selected_server(&self) -> Option<ServerChoice> {
        let local = self.local_server.is_some();
        let saved = self.saved_ssh_servers();
        let direct = self.saved_direct_servers();
        let listed = |choice: &ServerChoice| match choice {
            ServerChoice::Local => local,
            ServerChoice::Ssh(destination) => saved.iter().any(|(saved, _)| saved == destination),
            ServerChoice::Direct(route) => direct.iter().any(|(saved, _, _)| saved == route),
        };
        let searching = !self.query.trim().is_empty();
        searching
            .then(|| self.search_selected_server.clone())
            .flatten()
            .filter(listed)
            .or_else(|| self.selected_server.clone().filter(listed))
            .or_else(|| local.then_some(ServerChoice::Local))
            .or_else(|| {
                saved
                    .into_iter()
                    .next()
                    .map(|(destination, _)| ServerChoice::Ssh(destination))
            })
            .or_else(|| {
                direct
                    .into_iter()
                    .next()
                    .map(|(route, _, _)| ServerChoice::Direct(route))
            })
    }

    /// The name of the selected server, as its list item and headings show it.
    pub(super) fn selected_server_name(&self) -> SharedString {
        match self.selected_server() {
            Some(ServerChoice::Ssh(destination)) => {
                SharedString::from(destination.as_str().to_owned())
            }
            Some(ServerChoice::Direct(route)) => self
                .saved_direct_servers()
                .into_iter()
                .find(|(saved, _, _)| *saved == route)
                .map_or_else(
                    || SharedString::new_static("Direct network server"),
                    |(_, _, label)| label.into(),
                ),
            _ => SharedString::new_static(self.this_computer),
        }
    }

    /// Presents `server`'s detail. Its Server Settings are read afresh: nothing typed for another
    /// server carries over.
    pub(super) fn select_server(&mut self, server: ServerChoice, cx: &mut Context<Self>) {
        self.selected_server = Some(server);
        self.search_selected_server = None;
        self.sync_server_location(cx);
        cx.notify();
    }

    /// The server a removal leaves selected: the local server, otherwise the next listed server,
    /// otherwise the one before it.
    pub(super) fn server_after_removing(&self, removed: &ServerChoice) -> Option<ServerChoice> {
        if self.local_server.is_some() {
            return Some(ServerChoice::Local);
        }
        let listed: Vec<ServerChoice> = self
            .listed_servers()
            .into_iter()
            .map(|server| server.choice)
            .collect();
        let index = listed.iter().position(|listed| listed == removed)?;
        listed
            .get(index + 1)
            .or_else(|| index.checked_sub(1).and_then(|before| listed.get(before)))
            .cloned()
    }

    /// Whether `row` belongs on the page for `selected`, the resolved selection.
    fn presents_server_row(&self, row: SettingsRowId, selected: Option<&ServerChoice>) -> bool {
        match row {
            // Some server is selected exactly when some server is listed.
            SettingsRowId::ServerList => selected.is_some(),
            SettingsRowId::ServerWorktreeLocation => {
                self.offers_server_location()
                    && match selected {
                        Some(ServerChoice::Local) => self
                            .local_server
                            .as_ref()
                            .and_then(|row| row.worktree_server())
                            .is_some(),
                        Some(ServerChoice::Ssh(_) | ServerChoice::Direct(_)) => true,
                        None => false,
                    }
            }
            // Servers whose saved connection the person removed are not selected servers, so the
            // row appears only while SpaceTerm still has something to tell them.
            SettingsRowId::RemovedServerOperations => self.has_removed_operations(),
            // An unpair the server hasn't confirmed outlives its connection, so it appears while
            // there is something to tell the person.
            SettingsRowId::PendingUnpairs => self.has_pending_unpairs(),
            row => row_presents(row, selected),
        }
    }

    /// The listed server a query names: its whole name, or the start of it from
    /// `NAME_QUERY_MINIMUM` characters, so a word inside a host name or a partial word names no
    /// server.
    fn server_named_by_query(&self) -> Option<ServerChoice> {
        let query = self.query.trim().to_lowercase();
        if query.chars().count() < NAME_QUERY_MINIMUM {
            return None;
        }
        let listed = self.listed_servers();
        let name = |server: &&ListedServer| server.name.to_lowercase();
        listed
            .iter()
            .find(|server| name(server) == query)
            .or_else(|| {
                listed
                    .iter()
                    .find(|server| name(server).starts_with(&query))
            })
            .map(|server| server.choice.clone())
    }

    /// Chooses the server the current search shows: one it names, or otherwise the kind of
    /// server whose rows it reaches when the person's choice has none of them. The person's own
    /// choice never changes, so clearing the query returns to it.
    pub(super) fn select_server_for_query(&mut self, reachable: &[SettingsRowId]) {
        self.search_selected_server = None;
        if self.query.trim().is_empty() {
            return;
        }
        if let Some(named) = self.server_named_by_query() {
            self.search_selected_server = Some(named);
            return;
        }
        let listed = self.listed_servers();
        let selected = self.selected_server();
        let detail = |row: &&SettingsRowId| {
            row.descriptor().section == SettingsSectionId::Servers
                && (LOCAL_ROWS.contains(row) || SSH_ROWS.contains(row) || DIRECT_ROWS.contains(row))
        };
        if reachable
            .iter()
            .filter(detail)
            .any(|row| row_presents(*row, selected.as_ref()))
        {
            return;
        }
        if let Some(server) = listed.into_iter().find(|server| {
            reachable
                .iter()
                .filter(detail)
                .any(|row| row_presents(*row, Some(&server.choice)))
        }) {
            self.search_selected_server = Some(server.choice);
        }
    }

    /// The rows of `reachable` that present the selected server, best first. A query naming a
    /// listed server adds that server's whole page after them, leading with its status, so a
    /// Setting the query matches by its own words still comes first.
    pub(super) fn server_search_matches(
        &self,
        reachable: Vec<SettingsRowId>,
    ) -> Vec<SettingsRowId> {
        let selected = self.selected_server();
        let rows: Vec<_> = reachable
            .into_iter()
            .filter(|row| self.presents_server_row(*row, selected.as_ref()))
            .collect();
        if let Some(selected) = selected
            .as_ref()
            .filter(|selected| self.server_named_by_query().as_ref() == Some(*selected))
        {
            // Rows that name the query outrank the server; rows it only fuzzily touches follow.
            let naming = self.permission_access.naming();
            let (mut ranked, loose): (Vec<_>, Vec<_>) = rows
                .into_iter()
                .partition(|row| row.descriptor().names(&self.query, naming));
            let leading = leading_row(selected);
            let page = std::iter::once(leading).chain(
                super::catalog::rows().map(|row| row.id).filter(|row| {
                    row.descriptor().section == SettingsSectionId::Servers
                        && self.presents_server_row(*row, Some(selected))
                        && !self.omitted_rows.contains(row)
                }),
            );
            for row in page.chain(loose) {
                if !ranked.contains(&row) {
                    ranked.push(row);
                }
            }
            return ranked;
        }
        rows
    }

    /// `rows` with the server list placed ahead of the first Servers row, so the detail always
    /// sits under the list that selects it.
    pub(super) fn with_server_list_first(
        &self,
        mut rows: Vec<SettingsRowId>,
    ) -> Vec<SettingsRowId> {
        let Some(first) = rows
            .iter()
            .position(|row| row.descriptor().section == SettingsSectionId::Servers)
        else {
            return rows;
        };
        if self.selected_server().is_none()
            || self.omitted_rows.contains(&SettingsRowId::ServerList)
        {
            return rows;
        }
        rows.retain(|row| *row != SettingsRowId::ServerList);
        rows.insert(first.min(rows.len()), SettingsRowId::ServerList);
        rows
    }

    /// The heading of one Servers group, naming the selected machine where the group acts on it.
    pub(super) fn server_group_heading(&self, group: &'static str) -> SharedString {
        let machine = self.selected_server_name();
        match group {
            SERVER_GROUP => machine,
            CONNECTION_GROUP => SharedString::from(format!("Connection to {machine}")),
            SERVER_SETTINGS_GROUP => SharedString::from(format!("Server Settings on {machine}")),
            other => SharedString::new_static(other),
        }
    }

    /// Moves the selection with the arrow keys while the list has keyboard focus.
    fn navigate_server_list(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if event.keystroke.modifiers.modified() {
            return;
        }
        let listed = self.listed_servers();
        let Some(current) = self
            .selected_server()
            .and_then(|selected| listed.iter().position(|server| server.choice == selected))
        else {
            return;
        };
        let next = match event.keystroke.key.as_str() {
            "up" => current.saturating_sub(1),
            "down" => (current + 1).min(listed.len() - 1),
            "home" => 0,
            "end" => listed.len() - 1,
            _ => return,
        };
        window.prevent_default();
        cx.stop_propagation();
        if next != current {
            self.select_server(listed[next].choice.clone(), cx);
        }
    }

    /// Every listed server, selectable by pointer and keyboard. The list is one keyboard stop:
    /// the arrow keys move the selection, as in a source list.
    pub(super) fn render_server_list(
        &mut self,
        appearance: &ChromeAppearance,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let listed = self.listed_servers();
        let selected = self.selected_server();
        let focus = self.server_list_focus.clone();
        let focused = focus.is_focused(window);
        let card = appearance.host_colors(spaceterm_ui::ControlHost::Card);
        // Keyboard focus emphasizes the selection in the accent color, as a source list does,
        // rather than drawing a ring around the list.
        let selection = if focused && window.last_input_was_keyboard() && appearance.active {
            crate::ui::selection_chip::emphasized_selection_colors(card)
        } else if appearance.active {
            appearance
                .unfocused_selection_colors(spaceterm_ui::ControlHost::Card)
                .clone()
        } else {
            card.clone()
        };
        let count = listed.len();
        let items = listed.into_iter().enumerate().map(|(index, server)| {
            let is_selected = selected.as_ref() == Some(&server.choice);
            let selector = format!("{LIST_SELECTOR}-{index}");
            let (foreground, secondary) = if is_selected {
                (
                    selection.row_selected_foreground,
                    selection.row_selected_secondary,
                )
            } else {
                (card.text, card.text_secondary)
            };
            let dot = match server.tone {
                StatusTone::Connected => card.success,
                StatusTone::Neutral => card.icon_muted,
                StatusTone::Warning => card.warning,
                StatusTone::Error => card.error,
            };
            let status = if server.working {
                ProgressRing::new(
                    SharedString::from(format!("{selector}-progress")),
                    server.state,
                    ProgressState::Indeterminate,
                )
                .size(ProgressSize::Compact)
                .into_any_element()
            } else {
                div()
                    .size(appearance.spacing(7.0))
                    .rounded_full()
                    .bg(gpui_color(dot))
                    .into_any_element()
            };
            let icon = match server.choice {
                ServerChoice::Local => IconName::AppWindow,
                ServerChoice::Ssh(_) => IconName::Globe,
                ServerChoice::Direct(_) => IconName::Lock,
            };
            let choice = server.choice.clone();
            let pressed = server.choice.clone();
            let selecting = cx.weak_entity();
            div()
                .id(SharedString::from(selector.clone()))
                .debug_selector({
                    let selector = selector.clone();
                    move || selector.clone()
                })
                .role(accesskit::Role::ListBoxOption)
                .aria_label(server.name.clone())
                .aria_description(SharedString::from(format!(
                    "{}, {}",
                    server.method, server.state
                )))
                .aria_selected(is_selected)
                .aria_position_in_set(index + 1)
                .aria_size_of_set(count)
                .when(is_selected && focused, |item| item.aria_active_descendant())
                .on_a11y_action(accesskit::Action::Click, move |_, _, cx| {
                    let choice = choice.clone();
                    let _ = selecting.update(cx, |settings, cx| settings.select_server(choice, cx));
                })
                .on_click(cx.listener(move |settings, _, _, cx| {
                    settings.select_server(pressed.clone(), cx);
                }))
                .flex()
                .flex_row()
                .items_center()
                .gap(appearance.spacing(10.0))
                .w_full()
                .px(appearance.spacing(8.0))
                .py(appearance.spacing(5.0))
                .rounded(RadiusRole::Control.pixels())
                .cursor_default()
                .when(is_selected, |item| {
                    item.bg(gpui_color(selection.row_selected_background))
                })
                .child(
                    div()
                        .flex_none()
                        .text_color(gpui_color(if is_selected {
                            selection.row_selected_icon
                        } else {
                            card.icon
                        }))
                        .child(Icon::inherited(
                            icon,
                            appearance.icons.metrics(IconRole::Row).glyph_size,
                        )),
                )
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .flex_1()
                        .min_w_0()
                        .child(
                            div()
                                .truncate()
                                .chrome_text(appearance.typography.style(TextRole::Body))
                                .text_color(gpui_color(foreground))
                                .child(server.name),
                        )
                        .child(
                            div()
                                .chrome_text(appearance.typography.style(TextRole::Secondary))
                                .text_color(gpui_color(secondary))
                                .child(server.method),
                        ),
                )
                .child(
                    div()
                        .flex_none()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap(appearance.spacing(6.0))
                        .child(status)
                        .child(
                            div()
                                .chrome_text(appearance.typography.style(TextRole::Secondary))
                                .text_color(gpui_color(secondary))
                                .child(server.state),
                        ),
                )
        });
        div()
            .id(LIST_SELECTOR)
            .debug_selector(|| LIST_SELECTOR.to_owned())
            .role(accesskit::Role::ListBox)
            .aria_label(SERVER_LIST_NAME)
            .track_focus(&focus)
            .on_key_down(cx.listener(Self::navigate_server_list))
            .flex()
            .flex_col()
            .w_full()
            .gap(px(2.0))
            .children(items)
            .into_any_element()
    }

    /// How SpaceTerm reaches the selected server, and whose authority admits it.
    pub(super) fn server_connection_presentation(&self) -> (&'static str, SharedString) {
        match self.selected_server() {
            Some(ServerChoice::Direct(_)) => (
                "Direct network",
                SharedString::from(format!(
                    "This device is paired with \u{201C}{}\u{201D} and connects to it over the \
                     network, verifying the server each time. The pairing grants SpaceTerm \
                     access only, never SSH or administrator access, and this device\u{2019}s \
                     key stays in the system\u{2019}s credential storage.",
                    self.selected_server_name()
                )),
            ),
            Some(ServerChoice::Ssh(destination)) => (
                "SSH",
                SharedString::from(format!(
                    "OpenSSH signs in to \u{201C}{}\u{201D} with this computer\u{2019}s SSH \
                     configuration, and SpaceTerm uses the server for that account. SpaceTerm \
                     adds no pairing; that account\u{2019}s SSH access decides who connects.",
                    destination.as_str()
                )),
            ),
            _ => (
                "This account",
                SharedString::new_static(
                    "The server runs as your account on this computer, and the operating system \
                     admits only programs running as your account. There\u{2019}s nothing to pair \
                     or remove.",
                ),
            ),
        }
    }

    /// The caption under one row of the selected server's detail, for the rows this module and
    /// the SSH rows present.
    pub(super) fn server_row_caption(
        &self,
        row: SettingsRowId,
    ) -> Option<(SharedString, CaptionTone)> {
        match row {
            SettingsRowId::ServerConnection => Some((
                self.server_connection_presentation().1,
                CaptionTone::Guidance,
            )),
            SettingsRowId::DirectNetworkAccess | SettingsRowId::DirectNetworkPairing => {
                self.direct_access_caption(row)
            }
            row if DIRECT_ROWS.contains(&row) => self.direct_route_caption(row),
            SettingsRowId::PendingUnpairs => self.pending_unpairs_caption(),
            SettingsRowId::RemovedServerOperations => Some((
                SharedString::new_static(super::ssh_maintenance::REMOVED_GUIDANCE),
                CaptionTone::Guidance,
            )),
            row if super::ssh_maintenance::maintenance_row(row).is_some() => {
                self.ssh_maintenance_caption(row)
            }
            row if SSH_ROWS.contains(&row) => self.ssh_server_caption(row),
            _ => None,
        }
    }

    pub(super) fn render_server_value(
        &self,
        row: SettingsRowId,
        value: impl Into<SharedString>,
        appearance: &ChromeAppearance,
    ) -> AnyElement {
        let colors = appearance.host_colors(spaceterm_ui::ControlHost::Card);
        let selector = format!("{}-value", row.descriptor().selector);
        let id = SharedString::from(selector.clone());
        div()
            .debug_selector(move || selector.clone())
            .chrome_text(appearance.typography.style(TextRole::Body))
            .text_color(gpui_color(colors.text_secondary))
            .child(gpui::Text::new(id.into(), value.into()))
            .into_any_element()
    }
}

/// The group of rows reporting the selected server's state.
pub(super) const SERVER_GROUP: &str = "Server";
/// The group of rows naming how SpaceTerm reaches the selected server.
pub(super) const CONNECTION_GROUP: &str = "Connection";
/// The group of preferences the selected server owns.
pub(super) const SERVER_SETTINGS_GROUP: &str = "Server Settings";

#[cfg(test)]
#[path = "server_list_tests.rs"]
mod tests;
