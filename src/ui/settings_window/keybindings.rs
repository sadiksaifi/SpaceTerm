//! The Keybindings section: the Shortcut each Command resolves to, and a recorder to change it.
//!
//! [`crate::keybindings`] owns Keymap policy. These rows present the draft's resolved Keymap,
//! refuse a Reserved Shortcut in the recorder with its reason, and edit the retained overrides
//! through the Settings draft, which the keymap runtime applies to every window as it changes.

use std::collections::BTreeMap;
use std::rc::Rc;

use gpui::prelude::*;
use gpui::{AnyElement, App, Context, Entity, Keystroke, Modifiers, SharedString, Window};
use spaceterm_ui::{ShortcutRecorder, ShortcutRecorderEvent};

use super::{SettingsRowId, SettingsWindow, control_selector};
use crate::desktop_profile::DesktopPresentation;
use crate::keybindings::runtime::KeymapRuntime;
use crate::keybindings::{
    Command, KeybindingState, Reservation, ResolvedKeymap, Shortcut, ShortcutRejection,
    SystemReservation, TerminalConvention,
};

/// What the field shows for a Command without a Shortcut.
const UNASSIGNED_LABEL: &str = "None";
const RECORDING_PLACEHOLDER: &str = "Type Shortcut";

/// The recorder of every Command and the one notice the latest recording left behind.
pub(super) struct ShortcutRows {
    recorders: BTreeMap<Command, Entity<ShortcutRecorder>>,
    notice: Option<(Command, ShortcutNotice)>,
}

/// What the latest recording reports under its row, until the next one.
#[derive(Clone, Debug, PartialEq)]
pub(super) enum ShortcutNotice {
    /// The recorder refused a chord, and the text says why.
    Refused(SharedString),
    /// The recorded Shortcut was taken from another Command, which is now Unassigned.
    Reassigned { from: Command },
}

/// One line under a Keybindings row.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct ShortcutDescription {
    pub(super) text: SharedString,
    pub(super) error: bool,
}

impl ShortcutRows {
    pub(super) fn new(window: &mut Window, cx: &mut Context<SettingsWindow>) -> Self {
        let presentation = Rc::new(DesktopPresentation::get(cx).clone());
        let recorders = Command::ALL
            .into_iter()
            .map(|command| {
                let selector = control_selector(SettingsRowId::Shortcut(command));
                let presentation = Rc::clone(&presentation);
                let recorder = cx.new(|cx| {
                    ShortcutRecorder::new(
                        SharedString::from(selector.clone()),
                        command.label(),
                        window,
                        cx,
                    )
                    .empty_label(UNASSIGNED_LABEL)
                    .recording_placeholder(RECORDING_PLACEHOLDER)
                    .validator(validate)
                    .modifier_formatter(move |modifiers: Modifiers| {
                        presentation.format_modifiers(modifiers)
                    })
                    .debug_selector(selector)
                });
                cx.subscribe_in(
                    &recorder,
                    window,
                    move |settings, _, event: &ShortcutRecorderEvent, _, cx| {
                        settings.apply_recording(command, event, cx);
                    },
                )
                .detach();
                (command, recorder)
            })
            .collect();
        Self {
            recorders,
            notice: None,
        }
    }

    pub(super) fn dismiss_notice(&mut self) {
        self.notice = None;
    }

    #[cfg(test)]
    pub(super) fn recorder(&self, command: Command) -> &Entity<ShortcutRecorder> {
        &self.recorders[&command]
    }
}

/// Accepts a chord the Keymap can assign, and explains a refusal in the host's notation.
fn validate(keystroke: &Keystroke, cx: &App) -> Result<(), SharedString> {
    let presentation = DesktopPresentation::get(cx);
    let chord = presentation.format_keystroke(keystroke);
    let shortcut = Shortcut::from_keystroke(keystroke)
        .map_err(|rejection| rejection_message(rejection, &chord, presentation))?;
    KeymapRuntime::profile(cx)
        .check(&shortcut)
        .map_err(|reservation| reservation_message(reservation, &chord, presentation))
}

fn rejection_message(
    rejection: ShortcutRejection,
    chord: &str,
    presentation: &DesktopPresentation,
) -> SharedString {
    match rejection {
        ShortcutRejection::TerminalReserved(convention) => {
            reservation_message(Reservation::Terminal(convention), chord, presentation)
        }
        ShortcutRejection::FunctionModifier => "Shortcuts can't use the Fn key.".into(),
        ShortcutRejection::Malformed
        | ShortcutRejection::Chord
        | ShortcutRejection::ModifierOnly
        | ShortcutRejection::UnsupportedKey => {
            "SpaceTerm can't use this key in a shortcut.".into()
        }
    }
}

fn reservation_message(
    reservation: Reservation,
    chord: &str,
    presentation: &DesktopPresentation,
) -> SharedString {
    match reservation {
        Reservation::Terminal(TerminalConvention::TextInput) => {
            let primary = presentation.format_modifiers(Modifiers::command());
            format!("{chord} types into the terminal. Add {primary} to use it as a shortcut.")
        }
        Reservation::Terminal(
            TerminalConvention::ControlCharacter
            | TerminalConvention::Meta
            | TerminalConvention::ControlNavigation,
        ) => format!("{chord} is reserved for programs running in the terminal."),
        Reservation::System(reason) => format!(
            "{chord} is reserved by {} for {}.",
            presentation.wording().operating_system_name,
            system_reservation_label(reason),
        ),
    }
    .into()
}

/// The standard command or system feature a System Reserved Shortcut belongs to.
fn system_reservation_label(reason: SystemReservation) -> &'static str {
    match reason {
        SystemReservation::Copy => "Copy",
        SystemReservation::Paste => "Paste",
        SystemReservation::Cut => "Cut",
        SystemReservation::Undo => "Undo",
        SystemReservation::Redo => "Redo",
        SystemReservation::SelectAll => "Select All",
        SystemReservation::Quit => "Quit",
        SystemReservation::Hide => "Hide",
        SystemReservation::HideOthers => "Hide Others",
        SystemReservation::Minimize => "Minimize",
        SystemReservation::MinimizeAll => "Minimize All",
        SystemReservation::FullScreen => "Full Screen",
        SystemReservation::AppSwitcher => "switching apps",
        SystemReservation::WindowCycling => "switching windows",
        SystemReservation::Spotlight => "Spotlight",
        SystemReservation::CharacterViewer => "the Character Viewer",
        SystemReservation::ForceQuit => "Force Quit",
        SystemReservation::LockScreen => "Lock Screen",
        SystemReservation::LogOut => "Log Out",
        SystemReservation::Screenshot => "screenshots",
        SystemReservation::Help => "Help",
    }
}

impl SettingsWindow {
    /// The Keymap the draft resolves to, which is what every window applies while Settings edits.
    fn resolved_keymap(&self, cx: &App) -> ResolvedKeymap {
        KeymapRuntime::profile(cx).resolve(&self.editor.document().keybindings)
    }

    pub(super) fn render_shortcut(
        &mut self,
        command: Command,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let value = self
            .resolved_keymap(cx)
            .shortcut(command)
            .map(|shortcut| DesktopPresentation::get(cx).format(shortcut));
        let disabled = !self.editor.editable();
        let recorder = self.shortcuts.recorders[&command].clone();
        recorder.update(cx, |recorder, cx| {
            recorder.set_value(value, cx);
            recorder.set_disabled(disabled, cx);
        });
        recorder.into_any_element()
    }

    /// The row's notice, or why its Command has no active Shortcut.
    pub(super) fn shortcut_description(
        &self,
        command: Command,
        cx: &App,
    ) -> Option<ShortcutDescription> {
        let presentation = DesktopPresentation::get(cx);
        let (text, error) = match self.shortcuts.notice.as_ref() {
            Some((owner, ShortcutNotice::Refused(reason))) if *owner == command => {
                (reason.clone(), true)
            }
            Some((owner, ShortcutNotice::Reassigned { from })) if *owner == command => {
                (format!("Removed from {}.", from.label()).into(), false)
            }
            _ => match self.resolved_keymap(cx).state(command) {
                KeybindingState::Displaced { by } => (
                    format!("Its default shortcut is assigned to {}.", by.label()).into(),
                    false,
                ),
                KeybindingState::Blocked(reason) => {
                    let chord = self
                        .editor
                        .document()
                        .keybindings
                        .get(command)
                        .and_then(Option::as_ref)
                        .map(|shortcut| presentation.format(shortcut))?;
                    (
                        format!(
                            "{chord} is reserved by {} for {} and isn't active.",
                            presentation.wording().operating_system_name,
                            system_reservation_label(reason),
                        )
                        .into(),
                        true,
                    )
                }
                KeybindingState::Default
                | KeybindingState::Overridden
                | KeybindingState::Unassigned => return None,
            },
        };
        Some(ShortcutDescription { text, error })
    }

    /// Whether the Command's Keybinding differs from its default, including a default another
    /// Command's Shortcut displaced.
    pub(super) fn shortcut_differs(&self, command: Command, cx: &App) -> bool {
        self.editor.document().keybindings.is_overridden(command)
            || matches!(
                self.resolved_keymap(cx).state(command),
                KeybindingState::Displaced { .. }
            )
    }

    /// Restores the Command's default, reclaiming it from any Command that took it.
    pub(super) fn reset_shortcut(&mut self, command: Command, cx: &mut Context<Self>) {
        let profile = KeymapRuntime::profile(cx);
        self.shortcuts.dismiss_notice();
        self.edit(|draft| profile.reset(&mut draft.keybindings, command), cx);
        cx.notify();
    }

    fn apply_recording(
        &mut self,
        command: Command,
        event: &ShortcutRecorderEvent,
        cx: &mut Context<Self>,
    ) {
        let shortcut = match event {
            ShortcutRecorderEvent::Rejected(reason) => {
                self.shortcuts.notice = Some((command, ShortcutNotice::Refused(reason.clone())));
                cx.notify();
                return;
            }
            ShortcutRecorderEvent::Cancelled => {
                if self
                    .shortcuts
                    .notice
                    .as_ref()
                    .is_some_and(|(owner, _)| *owner == command)
                {
                    self.shortcuts.dismiss_notice();
                    cx.notify();
                }
                return;
            }
            ShortcutRecorderEvent::Cleared => None,
            ShortcutRecorderEvent::Recorded(keystroke) => {
                // The validator accepted this chord, so it parses.
                let Ok(shortcut) = Shortcut::from_keystroke(keystroke) else {
                    return;
                };
                Some(shortcut)
            }
        };
        let profile = KeymapRuntime::profile(cx);
        let mut assigned = None;
        self.edit(
            |draft| assigned = Some(profile.assign(&mut draft.keybindings, command, shortcut)),
            cx,
        );
        self.shortcuts.notice = match assigned {
            Some(Ok(reassignment)) => reassignment
                .displaced
                .map(|from| (command, ShortcutNotice::Reassigned { from })),
            Some(Err(reservation)) => Some((
                command,
                ShortcutNotice::Refused(reservation_message(
                    reservation,
                    "This shortcut",
                    DesktopPresentation::get(cx),
                )),
            )),
            None => None,
        };
        cx.notify();
    }
}
