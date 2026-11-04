//! The servers Settings presents, the one selection Servers and Git share, and the two ways to
//! choose: the server picker in the page header and the All Servers list.
//!
//! The list names this computer's server first, then each server this device is paired with by
//! the name it gave. Choosing a server only changes
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
use super::machine_card::Tone;
use super::machine_selection;
use super::{AddMachineRequest, SettingsWindow};
use crate::core_context::MachineKey;
use crate::direct_connections::{DirectRouteId, DirectRouteStatus};
use crate::ui::appearance::{ChromeAppearance, gpui_color};
use crate::ui::chrome_icons::IconRole;
use crate::ui::chrome_typography::{ChromeTextStyleExt as _, TextRole};
use crate::ui::sidebar_window::form::{CardEnds, row_insets, row_separator};

pub(super) const PICKER_SELECTOR: &str = "settings-server-picker";
pub(super) const LIST_SELECTOR: &str = "settings-all-servers";
pub(super) const ADD_MACHINE_SELECTOR: &str = "settings-add-server";
/// The picker's name for assistive technology.
pub(super) const PICKER_NAME: &str = "Machine";
const PICKER_HINT: &str = "Choose or add a machine";
/// The list's name for assistive technology.
pub(super) const MACHINE_LIST_NAME: &str = "All Machines";
/// The fewest characters of a server's name that search treats as naming it.
const NAME_QUERY_MINIMUM: usize = 3;

/// The card of rows reporting the selected server. It has no heading: the picker names it.
pub(super) const CORE_GROUP: &str = "";
/// The card of rows about other devices reaching the selected server over the network.
pub(super) const NETWORK_GROUP: &str = "Network Access";
/// The card listing every server.
pub(super) const ALL_MACHINES_GROUP: &str = "All Machines";
/// The card of this computer's command, which ignores the picker.
pub(super) const COMMAND_LINE_GROUP: &str = "Command Line";
/// The card of the selected server's Git and its Server Settings on the Git page.
pub(super) const GIT_CORE_GROUP: &str = "Git on machine";

/// The server a Servers or Git row acts on.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(in crate::ui::settings_window) enum MachineChoice {
    Local,
    /// A server this device reaches over a paired direct network route.
    Direct(DirectRouteId),
}

impl MachineChoice {
    /// The identity the retained selection and last-known facts use.
    pub(in crate::ui::settings_window) fn key(&self) -> MachineKey {
        match self {
            Self::Local => MachineKey::Local,
            Self::Direct(route) => MachineKey::Direct(*route),
        }
    }

    fn from_key(key: &MachineKey) -> Option<Self> {
        Some(match key {
            MachineKey::Local => Self::Local,
            MachineKey::Direct(route) => Self::Direct(*route),
        })
    }

    /// Which group the server belongs to in the picker and the list.
    const fn kind(&self) -> u8 {
        match self {
            Self::Local => 0,
            Self::Direct(_) => 1,
        }
    }
}

/// One server the picker and the list offer, from what SpaceTerm already knows about it.
pub(super) struct ListedCore {
    pub(super) choice: MachineChoice,
    pub(super) name: SharedString,
    /// "machine · OS" or "address · OS", which tells apart servers that share a name.
    pub(super) detail: SharedString,
    /// A short status, such as "Connected".
    pub(super) status: SharedString,
    pub(super) tone: Tone,
    pub(super) working: bool,
    /// Another listed server has the same name, such as two paired servers named for one host.
    pub(super) shares_name: bool,
}

impl ListedCore {
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
    AddMachine,
    /// Opens Add Server with the typed SSH host filled in.
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

/// Rows that administer this computer's server. A paired server's card takes Stop, Restart and
/// Update over its service lane instead, and presents none of these.
const ACCOUNT_ROWS: [SettingsRowId; 4] = [
    SettingsRowId::CoreAutoStart,
    SettingsRowId::NetworkAccess,
    SettingsRowId::PairedDevices,
    SettingsRowId::EndAllSessions,
];

/// Whether `row` presents something about `server`. Rows outside Servers and Git always do.
pub(super) fn row_presents(row: SettingsRowId, core: Option<&MachineChoice>) -> bool {
    if ACCOUNT_ROWS.contains(&row) {
        matches!(core, Some(MachineChoice::Local))
    } else {
        true
    }
}

impl SettingsWindow {
    /// The saved direct routes, their statuses and names, from the registry's snapshot.
    pub(super) fn saved_direct_machines(&self) -> Vec<(DirectRouteId, DirectRouteStatus, String)> {
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
    fn listed_choices(&self) -> Vec<MachineChoice> {
        let local = self.local_core.is_some().then_some(MachineChoice::Local);
        let direct = self
            .saved_direct_machines()
            .into_iter()
            .map(|(route, _, _)| MachineChoice::Direct(route));
        local.into_iter().chain(direct).collect()
    }

    pub(super) fn listed_machines(&self, cx: &App) -> Vec<ListedCore> {
        let named: Vec<_> = self
            .listed_choices()
            .into_iter()
            .map(|choice| (self.machine_name(&choice), choice))
            .collect();
        let shared = |name: &SharedString| {
            named
                .iter()
                .filter(|(other, _)| other.to_lowercase() == name.to_lowercase())
                .count()
                > 1
        };
        let selected = self.selected_machine();
        named
            .iter()
            .map(|(name, choice)| {
                let state = self.core_state(choice);
                // The selected server's card names its own work, so its entry keeps its state.
                let (status, working) = match state.operation {
                    Some(verb) if selected.as_ref() != Some(choice) => {
                        (SharedString::new_static(verb), true)
                    }
                    _ => (state.short, state.working),
                };
                ListedCore {
                    name: name.clone(),
                    detail: self.core_detail(choice, cx),
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
    pub(in crate::ui::settings_window) fn selected_machine(&self) -> Option<MachineChoice> {
        let listed = self.listed_choices();
        let searching = !self.query.trim().is_empty();
        searching
            .then(|| self.search_selected_machine.clone())
            .flatten()
            .filter(|choice| listed.contains(choice))
            .or_else(|| {
                self.selected_machine
                    .clone()
                    .filter(|choice| listed.contains(choice))
            })
            .or_else(|| {
                listed
                    .contains(&MachineChoice::Local)
                    .then_some(MachineChoice::Local)
            })
            .or_else(|| listed.into_iter().next())
    }

    /// The name of `choice`, as the picker and the list show it.
    pub(super) fn machine_name(&self, choice: &MachineChoice) -> SharedString {
        match choice {
            MachineChoice::Local => SharedString::new_static(self.this_computer),
            MachineChoice::Direct(route) => self
                .saved_direct_machines()
                .into_iter()
                .find(|(saved, _, _)| saved == route)
                .map_or_else(
                    || SharedString::new_static("Paired machine"),
                    |(_, _, label)| label.into(),
                ),
        }
    }

    /// The name of the selected server, as the picker and the page's wording show it.
    pub(in crate::ui::settings_window) fn selected_machine_name(&self) -> SharedString {
        self.selected_machine().map_or_else(
            || SharedString::new_static(self.this_computer),
            |choice| self.machine_name(&choice),
        )
    }

    /// Presents `server` on Servers and Git, now and after relaunch. It connects to, starts and
    /// installs nothing. Its Server Settings are read afresh: nothing typed for another server
    /// carries over.
    pub(in crate::ui::settings_window) fn select_machine(
        &mut self,
        core: MachineChoice,
        cx: &mut Context<Self>,
    ) {
        // Choosing a server is the person's response to the shown one's failed change.
        self.machine_location.answered();
        self.selected_machine = Some(core.clone());
        self.search_selected_machine = None;
        machine_selection::select(core.key(), cx);
        self.sync_machine_location(cx);
        cx.notify();
    }

    /// Follows the retained selection and facts, which load after the window opens and which
    /// another window may change. Following them starts no connection: the rows that read a
    /// server only continue the reads already allowed for the selected one.
    pub(super) fn follow_machine_selection(&mut self, cx: &mut Context<Self>) {
        let retained = MachineChoice::from_key(&machine_selection::selected(cx));
        if retained != self.selected_machine {
            self.selected_machine = retained;
        }
        self.retained_status = self
            .listed_choices()
            .iter()
            .filter_map(|choice| {
                let key = choice.key();
                let status = machine_selection::facts(&key, cx)?.status;
                Some((key, status))
            })
            .collect();
        self.sync_machine_location(cx);
        cx.notify();
    }

    /// Tells the retained selection which servers exist now, so a removed server's choice and
    /// facts go with it. Only a change in the listed servers writes anything.
    pub(super) fn reconcile_machines(&mut self, cx: &mut Context<Self>) {
        let keys: Vec<MachineKey> = self
            .listed_choices()
            .iter()
            .map(MachineChoice::key)
            .collect();
        if self.reconciled_machines.as_ref() == Some(&keys) {
            return;
        }
        // The registry loads after the window opens. Until it has read its retained servers,
        // nothing it will list is known to be gone, so the retained choice and facts stay. An
        // empty inventory is a real answer.
        let direct_loaded = self
            .direct_routes
            .as_ref()
            .is_none_or(|rows| rows.connections.retained_inventory_loaded());
        if !direct_loaded {
            return;
        }
        self.reconciled_machines = Some(keys.clone());
        machine_selection::reconcile(keys, cx);
    }

    /// Whether `row` belongs on its page for `selected`, the resolved selection.
    fn presents_machine_row(&self, row: SettingsRowId, selected: Option<&MachineChoice>) -> bool {
        match row {
            SettingsRowId::CoreStatus
            | SettingsRowId::GitTool
            | SettingsRowId::MachineWorktreeLocation => selected.is_some(),
            // Devices appear once other devices may connect.
            SettingsRowId::PairedDevices => row_presents(row, selected) && self.network_access_on(),
            row => row_presents(row, selected),
        }
    }

    /// The listed server a query names: its whole name, or the start of it from
    /// `NAME_QUERY_MINIMUM` characters, so a word inside a host name or a partial word names no
    /// server.
    fn machine_named_by_query(&self) -> Option<MachineChoice> {
        let query = self.query.trim().to_lowercase();
        if query.chars().count() < NAME_QUERY_MINIMUM {
            return None;
        }
        let named: Vec<(MachineChoice, String)> = self
            .listed_choices()
            .into_iter()
            .map(|choice| {
                let name = self.machine_name(&choice).to_lowercase();
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
    pub(super) fn select_machine_for_query(&mut self, reachable: &[SettingsRowId]) {
        self.search_selected_machine = None;
        if self.query.trim().is_empty() {
            return;
        }
        if let Some(named) = self.machine_named_by_query() {
            self.search_selected_machine = Some(named);
            return;
        }
        let selected = self.selected_machine();
        let account_rows = |row: &&SettingsRowId| ACCOUNT_ROWS.contains(row);
        if reachable
            .iter()
            .filter(account_rows)
            .any(|row| row_presents(*row, selected.as_ref()))
            || !reachable.iter().any(|row| ACCOUNT_ROWS.contains(row))
        {
            return;
        }
        self.search_selected_machine = self
            .listed_choices()
            .into_iter()
            .find(|choice| matches!(choice, MachineChoice::Local));
    }

    /// The rows of `reachable` that present the selected server, best first. A query naming a
    /// listed server adds that server's Servers rows after them, so a Setting the query matches
    /// by its own words still comes first.
    pub(super) fn core_search_matches(&self, reachable: Vec<SettingsRowId>) -> Vec<SettingsRowId> {
        let selected = self.selected_machine();
        let rows: Vec<_> = reachable
            .into_iter()
            .filter(|row| self.presents_machine_row(*row, selected.as_ref()))
            .collect();
        let Some(selected) = selected
            .as_ref()
            .filter(|selected| self.machine_named_by_query().as_ref() == Some(*selected))
        else {
            return rows;
        };
        let naming = self.permission_access.naming();
        let (mut ranked, loose): (Vec<_>, Vec<_>) = rows
            .into_iter()
            .partition(|row| row.descriptor().names(&self.query, naming));
        let page = super::catalog::rows().map(|row| row.id).filter(|row| {
            row.descriptor().section == SettingsSectionId::Machines
                && self.presents_machine_row(*row, Some(selected))
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
    pub(super) fn core_group_heading(&self, group: &'static str) -> SharedString {
        match group {
            GIT_CORE_GROUP => {
                SharedString::from(format!("Git on {}", self.selected_machine_name()))
            }
            other => SharedString::new_static(other),
        }
    }

    /// The server picker at the head of Servers and Git: the selected server's name and a small
    /// chevron, with no bezel. It searches the servers, then offers Add Server, and connecting to
    /// a typed host it doesn't list. A `flush` picker ends its chevron on the rows' control edge;
    /// beside client-drawn Window Controls it keeps its padding clear of them.
    pub(super) fn render_machine_picker(
        &mut self,
        appearance: &ChromeAppearance,
        flush: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let listed = self.listed_machines(cx);
        let selected = self.selected_machine();
        // The closed picker shows only a name, so a name another server shares also says where
        // the selected one is.
        let hint = listed
            .iter()
            .find(|core| Some(&core.choice) == selected.as_ref())
            .filter(|core| core.shares_name && !core.detail.is_empty())
            .map_or_else(
                || SharedString::new_static(PICKER_HINT),
                |core| SharedString::from(format!("{}. {PICKER_HINT}", core.detail)),
            );
        let names: Vec<String> = listed.iter().map(|core| core.name.to_lowercase()).collect();
        let card = appearance.host_colors(spaceterm_ui::ControlHost::Card);
        let mut previous_kind = None;
        let items = listed
            .into_iter()
            .enumerate()
            .map(|(index, core)| {
                let kind = core.choice.kind();
                let starts_group = previous_kind.is_some_and(|previous| previous != kind);
                previous_kind = Some(kind);
                let dot = tone_color(core.tone, card);
                let description = core.picker_description();
                let item = ComboBoxItem::new(core.choice, core.name)
                    .description(description)
                    .keywords([core.detail])
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
                    ComboBoxCommand::new(PickerCommand::AddMachine, "Add Machine\u{2026}")
                        .debug_selector(format!("{PICKER_SELECTOR}-add-server")),
                ];
                if looks_like_host(query) && !names.iter().any(|name| name.contains(&lowered)) {
                    commands.push(
                        ComboBoxCommand::new(
                            PickerCommand::ConnectHost,
                            format!("Add \u{201C}{query}\u{201D} over SSH\u{2026}"),
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
        // This computer is always listed and Add Server always remains, so the picker never
        // shows an empty state. A search that finds no server says so above Add Server.
        .copy(
            ComboBoxCopy::new("Search machines", "Search")
                .no_match("No machines match")
                .commands_heading("Add"),
        )
        .debug_selector(PICKER_SELECTOR)
        .on_accept(move |acceptance, _, cx| {
            let choice = acceptance.item_id().clone();
            let _ = owner.update(cx, |settings, cx| settings.select_machine(choice, cx));
        })
        .on_command(move |activation, window, cx| {
            let request = match activation.command() {
                PickerCommand::AddMachine => AddMachineRequest::Ssh(None),
                PickerCommand::ConnectHost => {
                    AddMachineRequest::Ssh(Some(activation.query().trim().to_owned()))
                }
            };
            let _ = commanding.update(cx, |settings, cx| {
                settings.begin_add_machine(request, window, cx);
            });
        })
        .into_any_element()
    }

    /// Moves the selection with the arrow keys while the list has keyboard focus, as a source
    /// list does.
    fn navigate_machine_list(
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
            .selected_machine()
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
            self.select_machine(listed[next].clone(), cx);
        }
    }

    /// Every server: its status dot, name, "machine · OS" and short status, with a checkmark on
    /// the selected one. Choosing a row selects it, exactly as the picker does. The list is one
    /// keyboard stop, whose arrow keys move the selection.
    pub(super) fn render_all_machines(
        &mut self,
        ends: CardEnds,
        appearance: &ChromeAppearance,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let listed = self.listed_machines(cx);
        let selected = self.selected_machine();
        let focus = self.machine_list_focus.clone();
        let focused = focus.is_focused(window);
        let card = appearance.host_colors(spaceterm_ui::ControlHost::Card);
        let count = listed.len();
        let items = listed.into_iter().enumerate().map(|(index, core)| {
            let is_selected = selected.as_ref() == Some(&core.choice);
            let selector = format!("{LIST_SELECTOR}-{index}");
            let indicator = if core.working {
                ProgressRing::new(
                    SharedString::from(format!("{selector}-progress")),
                    core.status.clone(),
                    ProgressState::Indeterminate,
                )
                .size(ProgressSize::Compact)
                .into_any_element()
            } else {
                status_dot(tone_color(core.tone, card), appearance.spacing(8.0)).into_any_element()
            };
            let choice = core.choice.clone();
            let pressed = core.choice.clone();
            let selecting = cx.weak_entity();
            div()
                .id(SharedString::from(selector.clone()))
                .debug_selector({
                    let selector = selector.clone();
                    move || selector.clone()
                })
                .role(accesskit::Role::ListBoxOption)
                .aria_label(core.name.clone())
                .aria_description(SharedString::from(format!(
                    "{}, {}",
                    core.detail, core.status
                )))
                .aria_selected(is_selected)
                .aria_position_in_set(index + 1)
                .aria_size_of_set(count)
                .when(is_selected && focused, |item| item.aria_active_descendant())
                .on_a11y_action(accesskit::Action::Click, move |_, _, cx| {
                    let choice = choice.clone();
                    let _ =
                        selecting.update(cx, |settings, cx| settings.select_machine(choice, cx));
                })
                .on_click(cx.listener(move |settings, _, _, cx| {
                    settings.select_machine(pressed.clone(), cx);
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
                                .child(core.name),
                        )
                        .child(
                            div()
                                .truncate()
                                .chrome_text(appearance.typography.style(TextRole::Secondary))
                                .text_color(gpui_color(card.text_muted))
                                .child(core.detail),
                        ),
                )
                .child(
                    div()
                        .flex_none()
                        .chrome_text(appearance.typography.style(TextRole::Secondary))
                        .text_color(gpui_color(card.text_secondary))
                        .child(core.status),
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
            .aria_label(MACHINE_LIST_NAME)
            .track_focus(&focus)
            .on_key_down(cx.listener(Self::navigate_machine_list))
            .flex()
            .flex_col()
            .w_full()
            .children(items)
            .into_any_element()
    }

    /// The list's add action: a plus, "Add Server…" and what it does. It is the visible way in
    /// for a person who never opens the picker.
    pub(super) fn render_add_machine_row(
        &mut self,
        ends: CardEnds,
        appearance: &ChromeAppearance,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let card = appearance.host_colors(spaceterm_ui::ControlHost::Card);
        let label = SettingsRowId::AddMachine
            .descriptor()
            .label(self.permission_access.naming());
        let description = add_machine_description();
        let opening = cx.weak_entity();
        let activating = cx.weak_entity();
        div()
            .id(ADD_MACHINE_SELECTOR)
            .debug_selector(|| ADD_MACHINE_SELECTOR.to_owned())
            .role(accesskit::Role::Button)
            .aria_label(SharedString::new_static(label))
            .aria_description(SharedString::new_static(description))
            .track_focus(&self.add_machine_focus)
            .on_a11y_action(accesskit::Action::Click, move |_, window, cx| {
                let _ = activating.update(cx, |settings, cx| {
                    settings.begin_add_machine(AddMachineRequest::Ssh(None), window, cx);
                });
            })
            .on_click(move |_, window, cx| {
                let _ = opening.update(cx, |settings, cx| {
                    settings.begin_add_machine(AddMachineRequest::Ssh(None), window, cx);
                });
            })
            .on_key_down(cx.listener(|settings, event: &KeyDownEvent, window, cx| {
                if event.keystroke.modifiers.modified() {
                    return;
                }
                if matches!(event.keystroke.key.as_str(), "enter" | "space") {
                    window.prevent_default();
                    cx.stop_propagation();
                    settings.begin_add_machine(AddMachineRequest::Ssh(None), window, cx);
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
                    .debug_selector(|| format!("{ADD_MACHINE_SELECTOR}-icon"))
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
pub(super) const fn add_machine_description() -> &'static str {
    "Set up over SSH or join with an invitation."
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
#[path = "machine_list_tests.rs"]
pub(in crate::ui::settings_window) mod tests;
