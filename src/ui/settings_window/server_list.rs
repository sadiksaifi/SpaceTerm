//! The servers Settings presents, the one selection Servers and Git share, and the two ways to
//! choose: the server picker in the page header and the All Servers list.
//!
//! The list names this computer's server first, then each saved SSH connection by destination,
//! then each server this device is paired with by the name it gave. Choosing a server only changes
//! what Servers and Git present: it connects to, starts and installs nothing, and the choice is
//! retained across launches. Everything here reads what SpaceTerm already knows, so rendering
//! reaches no computer and asks for no credential.

use gpui::prelude::*;
use gpui::{AnyElement, App, Context, KeyDownEvent, SharedString, Window, accesskit, div};
use spaceterm_ui::{
    ComboBox, ComboBoxCommand, ComboBoxCopy, ComboBoxItem, Icon, IconName, ProgressRing,
    ProgressSize, ProgressState,
};

use super::catalog::{SettingsRowId, SettingsSectionId};
use super::server_card::Tone;
use super::server_selection;
use super::{AddServerRequest, SettingsWindow};
use crate::direct_connections::{DirectRouteId, DirectRouteStatus};
use crate::domain::SshDestination;
use crate::server_context::ServerKey;
use crate::ssh::server_connection::RemoteConnectionStatus;
use crate::ui::appearance::{ChromeAppearance, gpui_color};
use crate::ui::chrome_icons::IconRole;
use crate::ui::chrome_typography::{ChromeTextStyleExt as _, TextRole};
use crate::ui::sidebar_window::form::{CardEnds, row_insets, row_separator};

pub(super) const PICKER_SELECTOR: &str = "settings-server-picker";
pub(super) const LIST_SELECTOR: &str = "settings-all-servers";
pub(super) const ADD_SERVER_SELECTOR: &str = "settings-add-server";
/// The picker's name for assistive technology.
pub(super) const PICKER_NAME: &str = "Server";
const PICKER_HINT: &str = "Choose or add a server";
/// The list's name for assistive technology.
pub(super) const SERVER_LIST_NAME: &str = "All Servers";
/// The fewest characters of a server's name that search treats as naming it.
const NAME_QUERY_MINIMUM: usize = 3;

/// The card of rows reporting the selected server. It has no heading: the picker names it.
pub(super) const SERVER_GROUP: &str = "";
/// The card of rows about other devices reaching the selected server over the network.
pub(super) const NETWORK_GROUP: &str = "Network Access";
/// The card listing every server.
pub(super) const ALL_SERVERS_GROUP: &str = "All Servers";
/// The card of the selected server's Git and its Server Settings on the Git page.
pub(super) const GIT_SERVER_GROUP: &str = "Git on server";

/// The server a Servers or Git row acts on.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(in crate::ui::settings_window) enum ServerChoice {
    Local,
    Ssh(SshDestination),
    /// A server this device reaches over a paired direct network route.
    Direct(DirectRouteId),
}

impl ServerChoice {
    /// The identity the retained selection and last-known facts use.
    pub(in crate::ui::settings_window) fn key(&self) -> ServerKey {
        match self {
            Self::Local => ServerKey::Local,
            Self::Ssh(destination) => ServerKey::Ssh(destination.as_str().to_owned()),
            Self::Direct(route) => ServerKey::Direct(*route),
        }
    }

    fn from_key(key: &ServerKey) -> Option<Self> {
        Some(match key {
            ServerKey::Local => Self::Local,
            ServerKey::Ssh(destination) => {
                Self::Ssh(SshDestination::new(destination.clone()).ok()?)
            }
            ServerKey::Direct(route) => Self::Direct(*route),
        })
    }

    /// Which group the server belongs to in the picker and the list.
    const fn kind(&self) -> u8 {
        match self {
            Self::Local => 0,
            Self::Ssh(_) => 1,
            Self::Direct(_) => 2,
        }
    }
}

/// One server the picker and the list offer, from what SpaceTerm already knows about it.
pub(super) struct ListedServer {
    pub(super) choice: ServerChoice,
    pub(super) name: SharedString,
    /// "machine · OS" or "address · OS", which tells apart servers that share a name.
    pub(super) detail: SharedString,
    /// A short status, such as "Connected · SSH".
    pub(super) status: SharedString,
    pub(super) tone: Tone,
    pub(super) working: bool,
    /// Another listed server has the same name, such as two paired servers named for one host.
    pub(super) shares_name: bool,
}

impl ListedServer {
    /// What the picker shows under the name: the short status, and where the server is when
    /// another server shares its name, so the two read apart.
    fn picker_description(&self) -> SharedString {
        if self.shares_name && !self.detail.is_empty() {
            SharedString::from(format!("{} \u{b7} {}", self.detail, self.status))
        } else {
            self.status.clone()
        }
    }
}

/// What the picker offers after its servers.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PickerCommand {
    AddServer,
    /// Opens Add Server with the typed host filled in.
    ConnectHost,
}

/// Whether `query` reads as an SSH host the person might mean, such as `penguin`,
/// `me@build.local` or `10.0.0.4`, rather than a few letters of a server's name.
pub(super) fn looks_like_host(query: &str) -> bool {
    let query = query.trim();
    !query.is_empty()
        && query.len() <= 255
        && !query.starts_with('-')
        && query
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || ".-_@:[]".contains(character))
}

/// Rows that present a server reached over SSH or this account, and never a paired one, whose
/// connection carries no authority over its computer's services or network.
const ACCOUNT_ROWS: [SettingsRowId; 3] = [
    SettingsRowId::ServerAutoStart,
    SettingsRowId::NetworkAccess,
    SettingsRowId::PairedDevices,
];

/// Whether `row` presents something about `server`. Rows outside Servers and Git always do.
pub(super) fn row_presents(row: SettingsRowId, server: Option<&ServerChoice>) -> bool {
    if ACCOUNT_ROWS.contains(&row) {
        !matches!(server, Some(ServerChoice::Direct(_)) | None)
    } else {
        true
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

    /// Every server SpaceTerm presents, in the order the picker and the list show them.
    fn listed_choices(&self) -> Vec<ServerChoice> {
        let local = self.local_server.is_some().then_some(ServerChoice::Local);
        let ssh = self
            .saved_ssh_servers()
            .into_iter()
            .map(|(destination, _)| ServerChoice::Ssh(destination));
        let direct = self
            .saved_direct_servers()
            .into_iter()
            .map(|(route, _, _)| ServerChoice::Direct(route));
        local.into_iter().chain(ssh).chain(direct).collect()
    }

    pub(super) fn listed_servers(&self, cx: &App) -> Vec<ListedServer> {
        let named: Vec<_> = self
            .listed_choices()
            .into_iter()
            .map(|choice| (self.server_name(&choice), choice))
            .collect();
        let shared = |name: &SharedString| {
            named
                .iter()
                .filter(|(other, _)| other.to_lowercase() == name.to_lowercase())
                .count()
                > 1
        };
        let selected = self.selected_server();
        named
            .iter()
            .map(|(name, choice)| {
                let state = self.server_state(choice);
                // The selected server's card names its own work, so its entry keeps its state.
                let (status, working) = match state.operation {
                    Some(verb) if selected.as_ref() != Some(choice) => {
                        (SharedString::new_static(verb), true)
                    }
                    _ => (state.short, state.working),
                };
                ListedServer {
                    name: name.clone(),
                    detail: self.server_detail(choice, cx),
                    status,
                    tone: state.tone,
                    working,
                    shares_name: shared(name),
                    choice: choice.clone(),
                }
            })
            .collect()
    }

    /// The server Servers and Git present: the one a search names while it lasts, otherwise the
    /// retained choice while it is listed, otherwise this computer's, otherwise the first listed.
    pub(in crate::ui::settings_window) fn selected_server(&self) -> Option<ServerChoice> {
        let listed = self.listed_choices();
        let searching = !self.query.trim().is_empty();
        searching
            .then(|| self.search_selected_server.clone())
            .flatten()
            .filter(|choice| listed.contains(choice))
            .or_else(|| {
                self.selected_server
                    .clone()
                    .filter(|choice| listed.contains(choice))
            })
            .or_else(|| {
                listed
                    .contains(&ServerChoice::Local)
                    .then_some(ServerChoice::Local)
            })
            .or_else(|| listed.into_iter().next())
    }

    /// The name of `choice`, as the picker and the list show it.
    pub(super) fn server_name(&self, choice: &ServerChoice) -> SharedString {
        match choice {
            ServerChoice::Local => SharedString::new_static(self.this_computer),
            ServerChoice::Ssh(destination) => SharedString::from(destination.as_str().to_owned()),
            ServerChoice::Direct(route) => self
                .saved_direct_servers()
                .into_iter()
                .find(|(saved, _, _)| saved == route)
                .map_or_else(
                    || SharedString::new_static("Paired server"),
                    |(_, _, label)| label.into(),
                ),
        }
    }

    /// The name of the selected server, as the picker and the page's wording show it.
    pub(in crate::ui::settings_window) fn selected_server_name(&self) -> SharedString {
        self.selected_server().map_or_else(
            || SharedString::new_static(self.this_computer),
            |choice| self.server_name(&choice),
        )
    }

    /// Presents `server` on Servers and Git, now and after relaunch. It connects to, starts and
    /// installs nothing. Its Server Settings are read afresh: nothing typed for another server
    /// carries over.
    pub(in crate::ui::settings_window) fn select_server(
        &mut self,
        server: ServerChoice,
        cx: &mut Context<Self>,
    ) {
        self.selected_server = Some(server.clone());
        self.search_selected_server = None;
        server_selection::select(server.key(), cx);
        self.sync_server_location(cx);
        cx.notify();
    }

    /// Follows the retained selection and facts, which load after the window opens and which
    /// another window may change. Following them starts no connection: the rows that read a
    /// server only continue the reads already allowed for the selected one.
    pub(super) fn follow_server_selection(&mut self, cx: &mut Context<Self>) {
        let retained = ServerChoice::from_key(&server_selection::selected(cx));
        if retained != self.selected_server {
            self.selected_server = retained;
        }
        self.retained_status = self
            .listed_choices()
            .iter()
            .filter_map(|choice| {
                let key = choice.key();
                let status = server_selection::facts(&key, cx)?.status;
                Some((key, status))
            })
            .collect();
        self.sync_server_location(cx);
        cx.notify();
    }

    /// Tells the retained selection which servers exist now, so a removed server's choice and
    /// facts go with it. Only a change in the listed servers writes anything.
    pub(super) fn reconcile_servers(&mut self, cx: &mut Context<Self>) {
        let keys: Vec<ServerKey> = self
            .listed_choices()
            .iter()
            .map(ServerChoice::key)
            .collect();
        if self.reconciled_servers.as_ref() == Some(&keys) {
            return;
        }
        // Registries load after the window opens. Until each present one has read its retained
        // servers, nothing it will list is known to be gone, so the retained choice and facts
        // stay. An empty inventory is a real answer.
        let ssh_loaded = self
            .ssh_servers
            .as_ref()
            .is_none_or(|rows| rows.connections.retained_inventory_loaded());
        let direct_loaded = self
            .direct_routes
            .as_ref()
            .is_none_or(|rows| rows.connections.retained_inventory_loaded());
        if !(ssh_loaded && direct_loaded) {
            return;
        }
        self.reconciled_servers = Some(keys.clone());
        server_selection::reconcile(keys, cx);
    }

    /// Whether `row` belongs on its page for `selected`, the resolved selection.
    fn presents_server_row(&self, row: SettingsRowId, selected: Option<&ServerChoice>) -> bool {
        match row {
            SettingsRowId::ServerStatus
            | SettingsRowId::EndAllSessions
            | SettingsRowId::GitTool
            | SettingsRowId::ServerWorktreeLocation => selected.is_some(),
            // Devices appear once other devices may connect.
            SettingsRowId::PairedDevices => row_presents(row, selected) && self.network_access_on(),
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
        let named: Vec<(ServerChoice, String)> = self
            .listed_choices()
            .into_iter()
            .map(|choice| {
                let name = self.server_name(&choice).to_lowercase();
                (choice, name)
            })
            .collect();
        named
            .iter()
            .find(|(_, name)| *name == query)
            .or_else(|| named.iter().find(|(_, name)| name.starts_with(&query)))
            .map(|(choice, _)| choice.clone())
    }

    /// Chooses the server the current search shows: one it names, or otherwise a server whose
    /// rows it reaches when the person's choice has none of them. The person's own choice never
    /// changes, so clearing the query returns to it.
    pub(super) fn select_server_for_query(&mut self, reachable: &[SettingsRowId]) {
        self.search_selected_server = None;
        if self.query.trim().is_empty() {
            return;
        }
        if let Some(named) = self.server_named_by_query() {
            self.search_selected_server = Some(named);
            return;
        }
        let selected = self.selected_server();
        let account_rows = |row: &&SettingsRowId| ACCOUNT_ROWS.contains(row);
        if reachable
            .iter()
            .filter(account_rows)
            .any(|row| row_presents(*row, selected.as_ref()))
            || !reachable.iter().any(|row| ACCOUNT_ROWS.contains(row))
        {
            return;
        }
        self.search_selected_server = self
            .listed_choices()
            .into_iter()
            .find(|choice| !matches!(choice, ServerChoice::Direct(_)));
    }

    /// The rows of `reachable` that present the selected server, best first. A query naming a
    /// listed server adds that server's Servers rows after them, so a Setting the query matches
    /// by its own words still comes first.
    pub(super) fn server_search_matches(
        &self,
        reachable: Vec<SettingsRowId>,
    ) -> Vec<SettingsRowId> {
        let selected = self.selected_server();
        let rows: Vec<_> = reachable
            .into_iter()
            .filter(|row| self.presents_server_row(*row, selected.as_ref()))
            .collect();
        let Some(selected) = selected
            .as_ref()
            .filter(|selected| self.server_named_by_query().as_ref() == Some(*selected))
        else {
            return rows;
        };
        let naming = self.permission_access.naming();
        let (mut ranked, loose): (Vec<_>, Vec<_>) = rows
            .into_iter()
            .partition(|row| row.descriptor().names(&self.query, naming));
        let page = super::catalog::rows().map(|row| row.id).filter(|row| {
            row.descriptor().section == SettingsSectionId::Servers
                && self.presents_server_row(*row, Some(selected))
                && !self.omitted_rows.contains(row)
        });
        for row in page.chain(loose) {
            if !ranked.contains(&row) {
                ranked.push(row);
            }
        }
        ranked
    }

    /// The heading of one Servers or Git group. The server card has none, because the picker
    /// names the server.
    pub(super) fn server_group_heading(&self, group: &'static str) -> SharedString {
        match group {
            GIT_SERVER_GROUP => {
                SharedString::from(format!("Git on {}", self.selected_server_name()))
            }
            other => SharedString::new_static(other),
        }
    }

    /// The server picker at the head of Servers and Git: the selected server's name and a small
    /// chevron, with no bezel. It searches the servers, then offers Add Server, and connecting to
    /// a typed host it doesn't list. A `flush` picker ends its chevron on the rows' control edge;
    /// beside client-drawn Window Controls it keeps its padding clear of them.
    pub(super) fn render_server_picker(
        &mut self,
        appearance: &ChromeAppearance,
        flush: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let listed = self.listed_servers(cx);
        let selected = self.selected_server();
        // The closed picker shows only a name, so a name another server shares also says where
        // the selected one is.
        let hint = listed
            .iter()
            .find(|server| Some(&server.choice) == selected.as_ref())
            .filter(|server| server.shares_name && !server.detail.is_empty())
            .map_or_else(
                || SharedString::new_static(PICKER_HINT),
                |server| SharedString::from(format!("{}. {PICKER_HINT}", server.detail)),
            );
        let names: Vec<String> = listed
            .iter()
            .map(|server| server.name.to_lowercase())
            .collect();
        let card = appearance.host_colors(spaceterm_ui::ControlHost::Card);
        let mut previous_kind = None;
        let items = listed
            .into_iter()
            .enumerate()
            .map(|(index, server)| {
                let kind = server.choice.kind();
                let starts_group = previous_kind.is_some_and(|previous| previous != kind);
                previous_kind = Some(kind);
                let dot = tone_color(server.tone, card);
                let description = server.picker_description();
                let item = ComboBoxItem::new(server.choice, server.name)
                    .description(description)
                    .keywords([server.detail])
                    .leading_icon(move |_, size| status_dot(dot, size * 0.5).into_any_element())
                    .debug_selector(format!("{PICKER_SELECTOR}-{index}"));
                if starts_group {
                    item.starts_group()
                } else {
                    item
                }
            })
            .collect();
        let owner = cx.weak_entity();
        let commanding = cx.weak_entity();
        ComboBox::with_commands(
            PICKER_SELECTOR,
            PICKER_NAME,
            selected,
            self.this_computer,
            items,
            move |query: &str| {
                let query = query.trim();
                let lowered = query.to_lowercase();
                let mut commands = vec![
                    ComboBoxCommand::new(PickerCommand::AddServer, "Add Server\u{2026}")
                        .debug_selector(format!("{PICKER_SELECTOR}-add-server")),
                ];
                if looks_like_host(query) && !names.iter().any(|name| name.contains(&lowered)) {
                    commands.push(
                        ComboBoxCommand::new(
                            PickerCommand::ConnectHost,
                            format!("Connect to \u{201C}{query}\u{201D} over SSH\u{2026}"),
                        )
                        .debug_selector(format!("{PICKER_SELECTOR}-connect-host")),
                    );
                }
                commands
            },
        )
        .ghost_trigger()
        .hug(true)
        .flush(flush)
        .accessibility_description(hint)
        .copy(ComboBoxCopy::new(
            "Search servers",
            "Search",
            "Loading\u{2026}",
            "No servers match",
        ))
        .debug_selector(PICKER_SELECTOR)
        .on_accept(move |acceptance, _, cx| {
            let choice = acceptance.item_id().clone();
            let _ = owner.update(cx, |settings, cx| settings.select_server(choice, cx));
        })
        .on_command(move |activation, window, cx| {
            let request = match activation.command() {
                PickerCommand::AddServer => AddServerRequest::Ssh(None),
                PickerCommand::ConnectHost => {
                    AddServerRequest::Ssh(Some(activation.query().trim().to_owned()))
                }
            };
            let _ = commanding.update(cx, |settings, cx| {
                settings.begin_add_server(request, window, cx);
            });
        })
        .into_any_element()
    }

    /// Moves the selection with the arrow keys while the list has keyboard focus, as a source
    /// list does.
    fn navigate_server_list(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if event.keystroke.modifiers.modified() {
            return;
        }
        let listed = self.listed_choices();
        let Some(current) = self
            .selected_server()
            .and_then(|selected| listed.iter().position(|choice| *choice == selected))
        else {
            return;
        };
        let last = listed.len().saturating_sub(1);
        let next = match event.keystroke.key.as_str() {
            "up" => current.saturating_sub(1),
            "down" => (current + 1).min(last),
            "home" => 0,
            "end" => last,
            _ => return,
        };
        window.prevent_default();
        cx.stop_propagation();
        if next != current {
            self.select_server(listed[next].clone(), cx);
        }
    }

    /// Every server: its status dot, name, "machine · OS" and short status, with a checkmark on
    /// the selected one. Choosing a row selects it, exactly as the picker does. The list is one
    /// keyboard stop, whose arrow keys move the selection.
    pub(super) fn render_all_servers(
        &mut self,
        ends: CardEnds,
        appearance: &ChromeAppearance,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let listed = self.listed_servers(cx);
        let selected = self.selected_server();
        let focus = self.server_list_focus.clone();
        let focused = focus.is_focused(window);
        let card = appearance.host_colors(spaceterm_ui::ControlHost::Card);
        let count = listed.len();
        let items = listed.into_iter().enumerate().map(|(index, server)| {
            let is_selected = selected.as_ref() == Some(&server.choice);
            let selector = format!("{LIST_SELECTOR}-{index}");
            let indicator = if server.working {
                ProgressRing::new(
                    SharedString::from(format!("{selector}-progress")),
                    server.status.clone(),
                    ProgressState::Indeterminate,
                )
                .size(ProgressSize::Compact)
                .into_any_element()
            } else {
                status_dot(tone_color(server.tone, card), appearance.spacing(8.0))
                    .into_any_element()
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
                    server.detail, server.status
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
                .relative()
                .flex()
                .flex_row()
                .items_center()
                .gap(appearance.spacing(ITEM_GAP))
                .w_full()
                .map(|item| row_insets(item, appearance))
                .map(|item| ends.nth(index, count).round(item))
                .cursor_default()
                .when(
                    is_selected && focused && window.last_input_was_keyboard(),
                    |item| item.bg(gpui_color(card.row_hover_background)),
                )
                .hover(|item| item.bg(gpui_color(card.row_hover_background)))
                .when(index > 0, |item| item.child(row_separator(appearance)))
                .child(
                    leading_slot(appearance)
                        .debug_selector({
                            let selector = format!("{selector}-indicator");
                            move || selector.clone()
                        })
                        .child(indicator),
                )
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .flex_1()
                        .min_w_0()
                        .gap(appearance.spacing(2.0))
                        .child(
                            div()
                                .truncate()
                                .chrome_text(appearance.typography.style(TextRole::Body))
                                .text_color(gpui_color(card.text))
                                .child(server.name),
                        )
                        .child(
                            div()
                                .truncate()
                                .chrome_text(appearance.typography.style(TextRole::Secondary))
                                .text_color(gpui_color(card.text_muted))
                                .child(server.detail),
                        ),
                )
                .child(
                    div()
                        .flex_none()
                        .chrome_text(appearance.typography.style(TextRole::Secondary))
                        .text_color(gpui_color(card.text_secondary))
                        .child(server.status),
                )
                .child(
                    div()
                        .debug_selector(move || format!("{selector}-check"))
                        .flex_none()
                        .flex()
                        .justify_end()
                        .w(row_glyph(appearance))
                        .text_color(gpui_color(card.text_accent))
                        .when(is_selected, |mark| {
                            mark.child(Icon::inherited(IconName::Check, row_glyph(appearance)))
                        }),
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
            .children(items)
            .into_any_element()
    }

    /// The list's add action: a plus, "Add Server…" and what it does. It is the visible way in
    /// for a person who never opens the picker.
    pub(super) fn render_add_server_row(
        &mut self,
        ends: CardEnds,
        appearance: &ChromeAppearance,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let card = appearance.host_colors(spaceterm_ui::ControlHost::Card);
        let label = SettingsRowId::AddServer
            .descriptor()
            .label(self.permission_access.naming());
        let description = add_server_description();
        let opening = cx.weak_entity();
        let activating = cx.weak_entity();
        div()
            .id(ADD_SERVER_SELECTOR)
            .debug_selector(|| ADD_SERVER_SELECTOR.to_owned())
            .role(accesskit::Role::Button)
            .aria_label(SharedString::new_static(label))
            .aria_description(SharedString::new_static(description))
            .track_focus(&self.add_server_focus)
            .on_a11y_action(accesskit::Action::Click, move |_, window, cx| {
                let _ = activating.update(cx, |settings, cx| {
                    settings.begin_add_server(AddServerRequest::Ssh(None), window, cx);
                });
            })
            .on_click(move |_, window, cx| {
                let _ = opening.update(cx, |settings, cx| {
                    settings.begin_add_server(AddServerRequest::Ssh(None), window, cx);
                });
            })
            .on_key_down(cx.listener(|settings, event: &KeyDownEvent, window, cx| {
                if event.keystroke.modifiers.modified() {
                    return;
                }
                if matches!(event.keystroke.key.as_str(), "enter" | "space") {
                    window.prevent_default();
                    cx.stop_propagation();
                    settings.begin_add_server(AddServerRequest::Ssh(None), window, cx);
                }
            }))
            .flex()
            .flex_row()
            .items_center()
            .gap(appearance.spacing(ITEM_GAP))
            .w_full()
            .map(|row| row_insets(row, appearance))
            .map(|row| ends.round(row))
            .cursor_default()
            .hover(|row| row.bg(gpui_color(card.row_hover_background)))
            .child(
                leading_slot(appearance)
                    .debug_selector(|| format!("{ADD_SERVER_SELECTOR}-icon"))
                    .text_color(gpui_color(card.text_accent))
                    .child(Icon::inherited(IconName::Plus, row_glyph(appearance))),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w_0()
                    .gap(appearance.spacing(2.0))
                    .child(
                        div()
                            .chrome_text(appearance.typography.style(TextRole::Body))
                            .text_color(gpui_color(card.text_accent))
                            .child(label),
                    )
                    .child(
                        div()
                            .chrome_text(appearance.typography.style(TextRole::Secondary))
                            .text_color(gpui_color(card.text_muted))
                            .child(description),
                    ),
            )
            .into_any_element()
    }
}

/// The space between a list row's status, its text, and its checkmark.
const ITEM_GAP: f32 = 8.0;

/// The size of the glyphs a list row leads and ends with.
fn row_glyph(appearance: &ChromeAppearance) -> gpui::Pixels {
    appearance.icons.metrics(IconRole::Row).glyph_size
}

/// The column a list row's status dot or plus starts in, on the card's row edge so it lines up
/// with the labels of the rows around it.
fn leading_slot(appearance: &ChromeAppearance) -> gpui::Div {
    div()
        .flex_none()
        .w(row_glyph(appearance))
        .flex()
        .items_center()
        .justify_start()
}

/// What Add Server does, under its label in the All Servers list.
pub(super) const fn add_server_description() -> &'static str {
    "Connect over SSH or with an invitation."
}

/// The color of a status dot of `tone`.
pub(super) fn tone_color(
    tone: Tone,
    colors: &crate::appearance::ChromeColors,
) -> crate::appearance::Color {
    match tone {
        Tone::Live => colors.success,
        Tone::Idle => colors.icon_muted,
        Tone::Attention => colors.warning,
        Tone::Problem => colors.error,
    }
}

/// A round status dot. Its meaning is announced by the text beside it.
pub(super) fn status_dot(color: crate::appearance::Color, size: gpui::Pixels) -> gpui::Div {
    div()
        .flex_none()
        .size(size)
        .rounded_full()
        .bg(gpui_color(color))
}

#[cfg(test)]
#[path = "server_list_tests.rs"]
pub(in crate::ui::settings_window) mod tests;
