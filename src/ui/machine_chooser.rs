//! The Machine chooser that New Remote Workspace opens first: the saved paired Machines in a
//! Command Palette, with Add Machine as its button. Choosing a connected Machine hands its route
//! to the Directory Picker on that Machine.
//!
//! Adding a Machine belongs to Settings > Machines > Add Machine, so the chooser only opens that
//! journey.
use gpui::prelude::*;
use gpui::{Context, Entity, EventEmitter, Render, SharedString, Window, div};
use spaceterm_ui::{
    CommandPalette, CommandPaletteAccessory, CommandPaletteActivationPolicy, CommandPaletteEmpty,
    CommandPaletteEvent, CommandPaletteItem, CommandPaletteLifecycleEvent,
    CommandPalettePrimaryAction, CommandPaletteReplacementFocus, Icon, IconName,
};

use crate::direct_connections::DirectRouteId;

const PLACEHOLDER: &str = "Choose a Machine\u{2026}";
const ADD_MACHINE_ACTION: &str = "machine-chooser-add-machine";
const ADD_MACHINE: &str = "Add Machine";
/// Why a saved Machine can't take a new Workspace now.
pub(crate) const NOT_CONNECTED: &str = "Not connected";
const NO_MACHINES: &str = "No Machines";
const NO_MACHINES_DESCRIPTION: &str = "Add a Machine to create Remote Workspaces on it.";
const NO_MACHINES_MATCH: &str = "No Machines match";
const NONE_CONNECTED: &str = "No Machine is connected.";

/// A saved paired Machine the chooser lists.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct MachineChoice {
    pub(crate) route: DirectRouteId,
    pub(crate) name: SharedString,
    /// The address the route reaches, kept only while another saved Machine shares the name.
    pub(crate) address: Option<SharedString>,
    /// Whether this window presents the Machine connected, so it can take a new Workspace.
    pub(crate) connected: bool,
}

impl MachineChoice {
    /// The name a row shows: the Machine's own, followed by its address when another saved
    /// Machine shares that name, so the person sees which one each row reaches.
    pub(crate) fn display_name(&self) -> SharedString {
        match &self.address {
            Some(address) => format!("{} ({address})", self.name).into(),
            None => self.name.clone(),
        }
    }
}

/// The address an endpoint such as `quic://192.168.1.20:9231` reaches, as a person reads it:
/// `192.168.1.20:9231`.
pub(crate) fn endpoint_address(endpoint: &str) -> &str {
    let rest = endpoint.strip_prefix("quic://").unwrap_or(endpoint);
    rest.split('/').next().unwrap_or(rest)
}

/// What the chooser lists.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum MachineChoices {
    /// The desktop is still reading its saved Machines.
    Loading,
    /// The saved Machines, in presentation order.
    Read(Vec<MachineChoice>),
}

/// How one chooser session ended.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum MachineChooserEvent {
    /// The person chose a connected Machine. The chooser closed for its Directory Picker, which
    /// takes the focus owner from [`MachineChooser::take_replacement`].
    Chosen(DirectRouteId),
    /// The person asked to add a Machine. The chooser closed.
    AddMachine,
    /// The chooser closed without a choice.
    Dismissed,
}

pub(super) struct MachineChooser {
    palette: Entity<CommandPalette<DirectRouteId>>,
    choices: Option<MachineChoices>,
    opening: bool,
    open: bool,
    /// The focus owner the Directory Picker restores, once a Machine was chosen.
    replacement: Option<CommandPaletteReplacementFocus>,
    /// The session already reported how it ended, so its close reports nothing more.
    ended: bool,
}

impl EventEmitter<MachineChooserEvent> for MachineChooser {}

impl MachineChooser {
    pub(super) fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let palette = cx.new(|cx| {
            let mut palette = CommandPalette::new(PLACEHOLDER, Vec::new(), window, cx);
            // A chosen Machine hands the keyboard to its Directory Picker, so the chooser closes
            // itself rather than restoring focus first.
            palette.set_activation(CommandPaletteActivationPolicy::Continue, cx);
            palette.set_primary_action(
                Some(
                    CommandPalettePrimaryAction::new(ADD_MACHINE_ACTION, ADD_MACHINE)
                        .leading_icon(|foreground, size| {
                            Icon::new(IconName::Plus, size, foreground).into_any_element()
                        })
                        // Adding a Machine leaves the search, so no key reaches it.
                        .button_only()
                        .debug_selector(ADD_MACHINE_ACTION),
                ),
                cx,
            );
            palette
        });
        cx.subscribe_in(
            &palette,
            window,
            |chooser, _, event: &CommandPaletteEvent<DirectRouteId>, window, cx| {
                chooser.reduce_palette_event(event, window, cx);
            },
        )
        .detach();
        Self {
            palette,
            choices: None,
            opening: false,
            open: false,
            replacement: None,
            ended: false,
        }
    }

    pub(super) fn open(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if self.open || self.opening {
            return false;
        }
        self.opening = true;
        self.ended = false;
        self.replacement = None;
        self.palette
            .update(cx, |palette, cx| palette.open(window, cx))
    }

    pub(super) const fn blocks_terminal_input(&self) -> bool {
        self.open || self.opening
    }

    /// Lists `choices`. The first connected Machine is selected, so Return goes straight on.
    pub(super) fn set_choices(&mut self, choices: MachineChoices, cx: &mut Context<Self>) {
        if self.choices.as_ref() == Some(&choices) {
            return;
        }
        self.palette.update(cx, |palette, cx| match &choices {
            MachineChoices::Loading => palette.set_loading(true, cx),
            MachineChoices::Read(machines) => {
                palette.set_items(machines.iter().enumerate().map(machine_item).collect(), cx);
                palette.set_empty(empty_state(machines), cx);
                palette.set_results_note(
                    (!machines.is_empty() && machines.iter().all(|machine| !machine.connected))
                        .then(|| NONE_CONNECTED.into()),
                    cx,
                );
                palette.set_loading(false, cx);
            }
        });
        self.choices = Some(choices);
    }

    /// The focus owner the chosen Machine's Directory Picker restores when it closes.
    pub(super) fn take_replacement(&mut self) -> Option<CommandPaletteReplacementFocus> {
        self.replacement.take()
    }

    /// Ends the session without a choice, as another chooser replaces it.
    pub(super) fn cancel(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.ended = true;
        self.palette.update(cx, |palette, cx| {
            palette.dismiss_without_restoring_focus(window, cx);
        });
    }

    fn reduce_palette_event(
        &mut self,
        event: &CommandPaletteEvent<DirectRouteId>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            CommandPaletteEvent::Lifecycle(CommandPaletteLifecycleEvent::Opened) => {
                self.opening = false;
                self.open = true;
            }
            // Another chooser replacing this one, such as the Workspace Switcher, also ends it.
            CommandPaletteEvent::Lifecycle(CommandPaletteLifecycleEvent::Closed(_)) => {
                self.opening = false;
                self.open = false;
                if !std::mem::replace(&mut self.ended, true) {
                    cx.emit(MachineChooserEvent::Dismissed);
                }
                cx.notify();
            }
            CommandPaletteEvent::Activated(activation) => {
                let route = *activation.item_id();
                if self.ended {
                    return;
                }
                self.ended = true;
                self.replacement = self.palette.update(cx, |palette, cx| {
                    palette.dismiss_for_replacement(window, cx)
                });
                cx.emit(MachineChooserEvent::Chosen(route));
            }
            CommandPaletteEvent::HeaderAction(action) if action == ADD_MACHINE_ACTION => {
                if self.ended {
                    return;
                }
                self.ended = true;
                self.palette
                    .update(cx, |palette, cx| palette.dismiss(window, cx));
                cx.emit(MachineChooserEvent::AddMachine);
            }
            _ => {}
        }
    }
}

/// One saved Machine's row. A Machine this window isn't connected to can't take a Workspace, so
/// its row is disabled and says why.
fn machine_item((position, machine): (usize, &MachineChoice)) -> CommandPaletteItem<DirectRouteId> {
    let item = CommandPaletteItem::new(machine.route, machine.display_name())
        .leading_icon(|foreground, size| {
            Icon::new(IconName::Globe, size, foreground).into_any_element()
        })
        .debug_selector(format!("machine-chooser-machine-{position}"));
    if machine.connected {
        item
    } else {
        item.disabled(true)
            .trailing(CommandPaletteAccessory::Status(NOT_CONNECTED.into()))
    }
}

/// With no saved Machine the chooser says so; otherwise an unmatched query says it matched none.
fn empty_state(machines: &[MachineChoice]) -> CommandPaletteEmpty {
    if machines.is_empty() {
        CommandPaletteEmpty::new(NO_MACHINES)
            .description(NO_MACHINES_DESCRIPTION)
            .icon(|foreground, size| {
                Icon::new(IconName::Globe, size, foreground).into_any_element()
            })
    } else {
        CommandPaletteEmpty::no_match(NO_MACHINES_MATCH)
    }
}

impl Render for MachineChooser {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .when(
                self.blocks_terminal_input(),
                super::directory_picker::block_hierarchy_actions,
            )
            .child(self.palette.clone())
    }
}

#[cfg(test)]
mod tests;
