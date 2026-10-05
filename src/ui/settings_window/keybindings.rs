//! The Keybindings section: the Shortcut each Command resolves to, and a recorder to change it.
//!
//! [`crate::keybindings`] owns Keymap policy. These rows present the draft's resolved Keymap,
//! refuse a Reserved Shortcut in the recorder with its reason, and edit the retained overrides
//! through the Settings draft, which the keymap runtime applies to every window as it changes.
//! A search above the rows narrows them by Command name or by a Shortcut pressed into it.

use std::collections::BTreeMap;
use std::rc::Rc;

use gpui::prelude::*;
use gpui::{AnyElement, App, Context, Entity, Keystroke, Modifiers, SharedString, Window, div};
use spaceterm_ui::{
    CapturedKey, ChordCapture, IconName, SearchField, SearchFieldToggle, ShortcutRecorder,
    ShortcutRecorderEvent, TextInput, TextInputEvent, TextInputVariant,
};

use super::{SettingsRowId, SettingsWindow, control_selector};
use crate::desktop_profile::DesktopPresentation;
use crate::keybindings::runtime::KeymapRuntime;
use crate::keybindings::{
    Command, KeybindingPreferences, KeybindingState, Reservation, ResolvedKeymap, Shortcut,
    ShortcutRejection, SystemReservation, TerminalConvention,
};
use crate::ui::appearance::{ChromeAppearance, gpui_color};
use crate::ui::sidebar_window::form::{CaptionTone, row_horizontal_inset};

/// What the field shows for a Command without a Shortcut.
const UNASSIGNED_LABEL: &str = "None";
const RECORDING_PLACEHOLDER: &str = "Type Shortcut";
const SEARCH_PLACEHOLDER: &str = "Search Keybindings";
const SEARCH_BY_SHORTCUT: &str = "Search by Shortcut";

/// The recorder of every Command, the one notice the latest recording left behind, and the search
/// that narrows the rows.
pub(super) struct ShortcutRows {
    recorders: BTreeMap<Command, Entity<ShortcutRecorder>>,
    notice: Option<RowNotice>,
    search: ShortcutSearch,
}

/// A notice and the keybindings it describes. It shows only while the draft still has them, so a
/// Reload, a reset, or an external change that replaces them retires it.
struct RowNotice {
    command: Command,
    notice: ShortcutNotice,
    keybindings: KeybindingPreferences,
}

/// The Keybindings search: text matched against Command names and Shortcuts, or one chord pressed
/// into the field while it records.
struct ShortcutSearch {
    input: Entity<TextInput>,
    /// The chord the field recorded, while the field still shows it.
    chord: Option<SearchedChord>,
    /// Present while the field records chords rather than taking text.
    capture: Option<ChordCapture>,
}

/// One chord pressed into the search, and the Shortcut it is when a Command can use it.
#[derive(Clone, Debug)]
struct SearchedChord {
    text: SharedString,
    /// The Shortcut to find, or why no Command can use the chord, which then finds nothing.
    shortcut: Result<Shortcut, SharedString>,
}

/// What the Keybindings search narrows the rows to.
enum ShortcutQuery<'a> {
    Everything,
    Text(&'a str),
    Chord(&'a SearchedChord),
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
    /// Error for a Reserved Shortcut, refused or inactive; Warning for a Shortcut that moved
    /// between Commands.
    pub(super) tone: CaptionTone,
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
                // The section is longer than the window, so Tab can reach a row scrolled out of
                // sight.
                cx.on_focus(
                    &recorder.read(cx).focus_handle(),
                    window,
                    move |settings, _, cx| {
                        settings.scroll_row_into_view(SettingsRowId::Shortcut(command), cx);
                    },
                )
                .detach();
                (command, recorder)
            })
            .collect();
        let input = cx.new(|cx| {
            TextInput::new(
                "settings-keybindings-search",
                SEARCH_PLACEHOLDER,
                String::new(),
                window,
                cx,
            )
            .placeholder(SEARCH_PLACEHOLDER)
            .variant(TextInputVariant::Bare)
            .input_length_limit(Some(128))
            .emit_programmatic_changes(true)
            .debug_selector("settings-keybindings-search")
        });
        cx.subscribe_in(
            &input,
            window,
            |settings, input, event: &TextInputEvent, window, cx| match event {
                TextInputEvent::ValueChanged(_) => {
                    let search = &mut settings.shortcuts.search;
                    let value = input.read(cx).value();
                    // A chord stays the query only while the field still shows it, so editing or
                    // clearing the text searches by text again.
                    if search
                        .chord
                        .as_ref()
                        .is_some_and(|chord| chord.text.as_ref() != value)
                    {
                        search.chord = None;
                    }
                    cx.notify();
                }
                TextInputEvent::Cancelled => {
                    if input.read(cx).value().is_empty() {
                        settings.focus_handle.focus(window, cx);
                    } else {
                        input.update(cx, |input, cx| {
                            input.clear(cx);
                        });
                    }
                }
                TextInputEvent::FocusLost => settings.shortcuts.end_search_capture(cx),
                _ => {}
            },
        )
        .detach();
        Self {
            recorders,
            notice: None,
            search: ShortcutSearch {
                input,
                chord: None,
                capture: None,
            },
        }
    }

    pub(super) fn dismiss_notice(&mut self) {
        self.notice = None;
    }

    /// Returns the search field to taking text, keeping whatever it found.
    pub(super) fn end_search_capture(&mut self, cx: &mut App) {
        if self.search.capture.take().is_none() {
            return;
        }
        self.search.input.update(cx, |input, cx| {
            input.set_editable(true, cx);
            input.set_placeholder(SEARCH_PLACEHOLDER, cx);
        });
    }

    fn query<'a>(&'a self, cx: &'a App) -> ShortcutQuery<'a> {
        if let Some(chord) = &self.search.chord {
            return ShortcutQuery::Chord(chord);
        }
        let text = self.search.input.read(cx).value().trim();
        if text.is_empty() {
            ShortcutQuery::Everything
        } else {
            ShortcutQuery::Text(text)
        }
    }
}

/// Accepts a chord the Keymap can assign, and explains a refusal in the host's notation.
fn validate(keystroke: &Keystroke, cx: &App) -> Result<(), SharedString> {
    assignable(keystroke, cx).map(|_| ())
}

/// The Shortcut a chord is when the Keymap can assign it, or why it can't, in the host's notation.
fn assignable(keystroke: &Keystroke, cx: &App) -> Result<Shortcut, SharedString> {
    let presentation = DesktopPresentation::get(cx);
    let chord = presentation.format_keystroke(keystroke);
    let profile = KeymapRuntime::profile(cx);
    let shortcut_modifiers = profile.terminal_conventions().shortcut_modifiers();
    let shortcut = Shortcut::from_keystroke(keystroke).map_err(rejection_message)?;
    profile.check(&shortcut).map_err(|reservation| {
        reservation_message(reservation, &chord, shortcut_modifiers, presentation)
    })?;
    Ok(shortcut)
}

fn rejection_message(rejection: ShortcutRejection) -> SharedString {
    match rejection {
        ShortcutRejection::FunctionModifier => "Shortcuts can't use the Fn key.".into(),
        ShortcutRejection::Malformed
        | ShortcutRejection::Chord
        | ShortcutRejection::ModifierOnly
        | ShortcutRejection::UnsupportedKey => "SpaceTerm can't use this key in a shortcut.".into(),
    }
}

fn reservation_message(
    reservation: Reservation,
    chord: &str,
    shortcut_modifiers: Modifiers,
    presentation: &DesktopPresentation,
) -> SharedString {
    match reservation {
        Reservation::Terminal(TerminalConvention::TextInput) => {
            let primary = presentation.format_modifiers(shortcut_modifiers);
            format!("{chord} is sent to the terminal. Add {primary} to use it as a shortcut.")
        }
        Reservation::Terminal(
            TerminalConvention::ControlCharacter
            | TerminalConvention::Meta
            | TerminalConvention::ControlNavigation,
        ) => format!("{chord} is reserved for programs running in the terminal."),
        Reservation::System(reason) => format!(
            "{chord} is {}.",
            system_reservation_text(reason, presentation)
        ),
    }
    .into()
}

/// Who reserves a System Reserved Shortcut, and for what: "reserved by <owner> for <feature>".
fn system_reservation_text(
    reason: SystemReservation,
    presentation: &DesktopPresentation,
) -> String {
    let owner = match reason {
        #[cfg(feature = "developer-tools")]
        SystemReservation::DeveloperWorkbench | SystemReservation::AppearancePreview => {
            crate::application_identity::ApplicationIdentity::current().display_name()
        }
        _ => presentation.wording().operating_system_name,
    };
    format!(
        "reserved by {owner} for {}",
        system_reservation_label(reason)
    )
}

/// The standard command or system feature a System Reserved Shortcut belongs to.
fn system_reservation_label(reason: SystemReservation) -> &'static str {
    match reason {
        SystemReservation::Copy => "Copy",
        SystemReservation::Paste => "Paste",
        SystemReservation::PasteSelection => "Paste Selection",
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
        SystemReservation::Settings => "Settings",
        SystemReservation::KeyboardNavigation => "keyboard navigation",
        SystemReservation::DockHiding => "hiding the Dock",
        SystemReservation::Zoom => "Zoom",
        SystemReservation::InvertColors => "inverting colors",
        SystemReservation::Contrast => "adjusting contrast",
        SystemReservation::VoiceOver => "VoiceOver",
        SystemReservation::AccessibilityShortcuts => "Accessibility Shortcuts",
        SystemReservation::DesktopShortcut => "desktop shortcuts",
        SystemReservation::MoveWindowToWorkspace => "moving windows between workspaces",
        SystemReservation::ScreenRecording => "screen recording",
        SystemReservation::InputMethod => "the input method",
        SystemReservation::Restart => "Restart",
        SystemReservation::ShutDown => "Shut Down",
        #[cfg(feature = "developer-tools")]
        SystemReservation::DeveloperWorkbench => "the Developer Workbench",
        #[cfg(feature = "developer-tools")]
        SystemReservation::AppearancePreview => "Toggle Appearance",
    }
}

impl SettingsWindow {
    /// The search above the Keybindings rows, with its toggle for searching by a pressed Shortcut.
    pub(super) fn render_shortcut_search(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let owner = cx.weak_entity();
        let toggle = SearchFieldToggle::new(
            IconName::Keyboard,
            SEARCH_BY_SHORTCUT,
            self.shortcuts.search.capture.is_some(),
            move |window, cx| {
                let _ = owner.update(cx, |settings, cx| {
                    settings.toggle_shortcut_search(window, cx);
                });
            },
        )
        .debug_selector("settings-keybindings-search-by-shortcut");
        div()
            .w_full()
            .child(
                SearchField::new(
                    "settings-keybindings-search-frame",
                    self.shortcuts.search.input.clone(),
                )
                .debug_selectors(
                    "settings-keybindings-search-frame",
                    "settings-keybindings-search-clear",
                )
                .toggle(toggle),
            )
            .into_any_element()
    }

    /// Keeps the Keybindings rows the search finds.
    pub(super) fn retain_found_shortcuts(&self, rows: &mut Vec<SettingsRowId>, cx: &App) {
        let presentation = DesktopPresentation::get(cx);
        let keymap = self.resolved_keymap(cx);
        let query = self.shortcuts.query(cx);
        rows.retain(|row| {
            let SettingsRowId::Shortcut(command) = *row else {
                return true;
            };
            match &query {
                ShortcutQuery::Everything => true,
                ShortcutQuery::Chord(chord) => chord
                    .shortcut
                    .as_ref()
                    .is_ok_and(|shortcut| keymap.shortcuts(command).contains(shortcut)),
                ShortcutQuery::Text(text) => {
                    let label = command.label().to_lowercase();
                    let shortcut = keymap
                        .shortcut(command)
                        .map(|shortcut| presentation.format(shortcut).to_lowercase());
                    text.split_whitespace().all(|term| {
                        let term = term.to_lowercase();
                        label.contains(&term)
                            || shortcut.as_ref().is_some_and(|chord| chord.contains(&term))
                    })
                }
            }
        });
    }

    /// Explains a search that found no Keybindings rows.
    pub(super) fn render_no_shortcuts_found(
        &self,
        appearance: &ChromeAppearance,
        cx: &App,
    ) -> Option<AnyElement> {
        let message = self.no_shortcuts_found(cx)?;
        // A Reserved Shortcut reads in the same error color the recorder refuses it in.
        let reserved = matches!(
            self.shortcuts.query(cx),
            ShortcutQuery::Chord(SearchedChord {
                shortcut: Err(_),
                ..
            })
        );
        let color = if reserved {
            appearance.colors.error
        } else {
            appearance.colors.text_muted
        };
        Some(
            div()
                .debug_selector(|| "settings-keybindings-no-results".to_owned())
                .px(row_horizontal_inset(appearance))
                .text_color(gpui_color(color))
                .child(message)
                .into_any_element(),
        )
    }

    /// What a search that finds nothing says.
    pub(super) fn no_shortcuts_found(&self, cx: &App) -> Option<SharedString> {
        Some(match self.shortcuts.query(cx) {
            ShortcutQuery::Everything => return None,
            // A Reserved Shortcut is explained the way the recorder refuses it, which says more than
            // that nothing uses it.
            ShortcutQuery::Chord(SearchedChord {
                shortcut: Err(reason),
                ..
            }) => reason.clone(),
            ShortcutQuery::Chord(chord) => format!("No command uses {}.", chord.text).into(),
            ShortcutQuery::Text(text) => format!("No commands match “{text}”.").into(),
        })
    }

    /// Switches the search between taking text and recording the Shortcut to search for.
    fn toggle_shortcut_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.shortcuts.search.capture.is_some() {
            self.shortcuts.end_search_capture(cx);
            cx.notify();
            return;
        }
        let input = self.shortcuts.search.input.clone();
        let focus = input.update(cx, |input, cx| {
            input.clear(cx);
            input.set_editable(false, cx);
            input.set_placeholder(RECORDING_PLACEHOLDER, cx);
            input.focus_handle()
        });
        focus.focus(window, cx);
        self.shortcuts.search.chord = None;
        self.shortcuts.search.capture = Some(ChordCapture::start(
            focus,
            window,
            cx,
            Self::capture_shortcut_search,
        ));
        cx.notify();
    }

    /// Applies one key pressed while the search records: a chord becomes the query and recording
    /// continues, so the next chord replaces it.
    fn capture_shortcut_search(
        &mut self,
        key: CapturedKey,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let input = self.shortcuts.search.input.clone();
        match key {
            CapturedKey::Chord(keystroke) => {
                let text = DesktopPresentation::get(cx).format_keystroke(&keystroke);
                self.shortcuts.search.chord = Some(SearchedChord {
                    text: text.clone(),
                    shortcut: assignable(&keystroke, cx),
                });
                input.update(cx, |input, cx| {
                    input.set_value(text.to_string(), cx);
                });
            }
            CapturedKey::Erase => {
                self.shortcuts.search.chord = None;
                input.update(cx, |input, cx| {
                    input.clear(cx);
                });
            }
            CapturedKey::Escape | CapturedKey::Traverse | CapturedKey::FocusLost => {
                self.shortcuts.end_search_capture(cx);
            }
        }
        cx.notify();
    }

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
        let keybindings = &self.editor.document().keybindings;
        let resolved = self.resolved_keymap(cx);
        let notice = self
            .shortcuts
            .notice
            .as_ref()
            .filter(|notice| notice.keybindings == *keybindings)
            .map(|notice| (notice.command, &notice.notice));
        let (text, tone) = match notice {
            Some((owner, ShortcutNotice::Refused(reason))) if owner == command => {
                (reason.clone(), CaptionTone::Error)
            }
            // A reassignment is marked on both rows, so the Command that lost its Shortcut is as
            // easy to find as the one that took it.
            Some((owner, ShortcutNotice::Reassigned { from })) if owner == command => (
                format!("Removed from {}.", from.label()).into(),
                CaptionTone::Warning,
            ),
            Some((owner, ShortcutNotice::Reassigned { from })) if *from == command => (
                format!("Its shortcut is now assigned to {}.", owner.label()).into(),
                CaptionTone::Warning,
            ),
            _ => match (resolved.state(command), resolved.inactive_override(command)) {
                (
                    state @ (KeybindingState::Default
                    | KeybindingState::Unassigned
                    | KeybindingState::Displaced { .. }),
                    Some(reservation),
                ) => {
                    let chord = keybindings
                        .get(command)
                        .and_then(Option::as_ref)
                        .map_or_else(
                            || "Its shortcut".into(),
                            |shortcut| presentation.format(shortcut),
                        );
                    let reason = match reservation {
                        Reservation::Terminal(_) => "reserved for terminal input".to_owned(),
                        Reservation::System(reason) => {
                            system_reservation_text(reason, presentation)
                        }
                    };
                    // Keep the inactive override's reason when another Command owns the default.
                    let (outcome, tone) = match state {
                        KeybindingState::Default => (
                            ", so the default shortcut is active".to_owned(),
                            CaptionTone::Warning,
                        ),
                        KeybindingState::Displaced { by } => (
                            format!(". Its default shortcut is assigned to {}", by.label()),
                            CaptionTone::Warning,
                        ),
                        _ => (" and isn't active".to_owned(), CaptionTone::Error),
                    };
                    (format!("{chord} is {reason} here{outcome}.").into(), tone)
                }
                (KeybindingState::Displaced { by }, _) => (
                    format!("Its default shortcut is assigned to {}.", by.label()).into(),
                    CaptionTone::Warning,
                ),
                // Only a default can be blocked: a Reserved override is inactive instead.
                (KeybindingState::TerminalBlocked(_), _) => (
                    "Its default shortcut is reserved for terminal input and isn't active.".into(),
                    CaptionTone::Error,
                ),
                (KeybindingState::Blocked(reason), _) => (
                    format!(
                        "Its default shortcut is {} and isn't active.",
                        system_reservation_text(reason, presentation)
                    )
                    .into(),
                    CaptionTone::Error,
                ),
                (
                    KeybindingState::Default
                    | KeybindingState::Overridden
                    | KeybindingState::Unassigned,
                    _,
                ) => return None,
            },
        };
        Some(ShortcutDescription { text, tone })
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
                self.shortcuts.notice = Some(RowNotice {
                    command,
                    notice: ShortcutNotice::Refused(reason.clone()),
                    keybindings: self.editor.document().keybindings.clone(),
                });
                cx.notify();
                return;
            }
            ShortcutRecorderEvent::Cancelled => {
                if self
                    .shortcuts
                    .notice
                    .as_ref()
                    .is_some_and(|notice| notice.command == command)
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
        let notice = match assigned {
            Some(Ok(reassignment)) => reassignment
                .displaced
                .map(|from| ShortcutNotice::Reassigned { from }),
            Some(Err(reservation)) => Some(ShortcutNotice::Refused(reservation_message(
                reservation,
                "This shortcut",
                profile.terminal_conventions().shortcut_modifiers(),
                DesktopPresentation::get(cx),
            ))),
            None => None,
        };
        self.shortcuts.notice = notice.map(|notice| RowNotice {
            command,
            notice,
            keybindings: self.editor.document().keybindings.clone(),
        });
        cx.notify();
    }
}

#[cfg(test)]
mod tests;
