//! The one layout every Privacy permission row shares: a subtitle under the label and exactly one
//! trailing control, which is either the row's next step as a button or its state as text.

use gpui::prelude::*;
use gpui::{AnyElement, App, SharedString, Window, accesskit, div};
use spaceterm_ui::{ProgressRing, ProgressSize, ProgressState};

use super::machine_list::status_dot;
use crate::ui::appearance::{ChromeAppearance, gpui_color};
use crate::ui::chrome_typography::{ChromeTextStyleExt as _, TextRole};
use crate::ui::sidebar_window::form::{CaptionTone, FormRow, action_button};

/// The next step a permission row offers.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum AccessAction {
    /// SpaceTerm asks for the permission itself: the system prompt or a Permission Setup.
    Allow,
    /// Only System Settings can change the permission.
    OpenSettings,
    /// Ends the running Permission Setup.
    Cancel,
    /// Reads the permission again after a failed read.
    CheckAgain,
}

impl AccessAction {
    pub(super) const fn label(self) -> &'static str {
        match self {
            Self::Allow => "Allow\u{2026}",
            Self::OpenSettings => "Open System Settings",
            Self::Cancel => "Cancel",
            Self::CheckAgain => "Check Again",
        }
    }
}

/// A row's one trailing control.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum AccessControl {
    /// The row's next step.
    Action(AccessAction),
    /// The row's state, when nothing is left to do. An allowed state leads with the success dot.
    Status { label: &'static str, allowed: bool },
}

/// What the subtitle under the label reports.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum CaptionKind {
    /// What the permission is for, or how to get it.
    Guidance,
    /// What SpaceTerm waits for, led by a spinner.
    Waiting,
    /// What went wrong and the step that recovers.
    Error,
}

/// One permission row's complete presentation, derived from its state so every row reads one way.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct AccessPresentation {
    pub(super) control: AccessControl,
    pub(super) caption: SharedString,
    pub(super) caption_kind: CaptionKind,
    /// Whether the label offers the reset of the grant's holder entry.
    pub(super) resettable: bool,
}

/// The selectors one row's trailing control renders under.
pub(super) struct AccessSelectors {
    pub(super) control: &'static str,
    pub(super) status: &'static str,
    pub(super) allow: &'static str,
    pub(super) open_settings: &'static str,
    pub(super) cancel: &'static str,
    pub(super) check_again: &'static str,
    /// The spinner leading the subtitle while the row waits.
    pub(super) progress: &'static str,
}

impl AccessSelectors {
    pub(super) const fn action(&self, action: AccessAction) -> &'static str {
        match action {
            AccessAction::Allow => self.allow,
            AccessAction::OpenSettings => self.open_settings,
            AccessAction::Cancel => self.cancel,
            AccessAction::CheckAgain => self.check_again,
        }
    }
}

/// The row's trailing control. `on_action` runs the button's action.
pub(super) fn render_control(
    selectors: &'static AccessSelectors,
    control: AccessControl,
    appearance: &ChromeAppearance,
    on_action: impl Fn(AccessAction, &mut Window, &mut App) + 'static,
) -> AnyElement {
    let content = match control {
        AccessControl::Action(action) => action_button(
            selectors.action(action),
            action.label(),
            true,
            move |window, cx| on_action(action, window, cx),
        )
        .into_any_element(),
        AccessControl::Status { label, allowed } => {
            let colors = appearance.host_colors(spaceterm_ui::ControlHost::Card);
            // Assistive technology announces the state when it changes, as a status.
            div()
                .id(selectors.status)
                .debug_selector(|| selectors.status.to_owned())
                .role(accesskit::Role::Status)
                .aria_label(SharedString::new_static(label))
                .flex()
                .flex_row()
                .items_center()
                .gap(appearance.spacing(6.0))
                .children(allowed.then(|| {
                    status_dot(colors.success, appearance.spacing(8.0)).into_any_element()
                }))
                .child(
                    div()
                        .chrome_text(appearance.typography.style(TextRole::Body))
                        .text_color(gpui_color(colors.text_secondary))
                        .child(label),
                )
                .into_any_element()
        }
    };
    div()
        .debug_selector(|| selectors.control.to_owned())
        .flex()
        .flex_row()
        .items_center()
        .justify_end()
        .child(content)
        .into_any_element()
}

/// Puts the row's subtitle under its label: guidance, an error in the error color, or what the
/// row waits for, led by a spinner on one line.
pub(super) fn caption(
    row: FormRow,
    selectors: &'static AccessSelectors,
    presentation: &AccessPresentation,
) -> FormRow {
    let caption = presentation.caption.clone();
    match presentation.caption_kind {
        CaptionKind::Guidance => row.description(caption),
        CaptionKind::Error => row.caption(caption, CaptionTone::Error),
        CaptionKind::Waiting => {
            let spinner = ProgressRing::new(
                SharedString::new_static(selectors.progress),
                caption.clone(),
                ProgressState::Indeterminate,
            )
            .size(ProgressSize::Compact)
            .debug_selector(selectors.progress)
            .into_any_element();
            row.description(caption).status_line(Some(spinner))
        }
    }
}
