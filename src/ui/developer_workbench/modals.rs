//! The Modals section: each modal family over controls it must obscure.
//!
//! The obscured input, combo box, and menu stay in the section while a modal is open. None of
//! them may receive interaction through the scrim, and closing the modal returns focus to the
//! control that held it.

use std::time::Duration;

use gpui::prelude::*;
use gpui::{AnyElement, Entity, Window, div};
use spaceterm_ui::{
    Alert, AlertIntent, Button, ButtonSize, ButtonVariant, ComboBox, ComboBoxItem, ContextMenu,
    Dialog, DialogCloseDecision, DialogInitialFocus, FieldState, MenuEntry, ModalAction,
    ModalActionRole, ModalId, ProgressCancelDecision, ProgressCancellation, ProgressDialog,
    ProgressState, SegmentedControl, SegmentedOption, Switch, TextInput, TextInputContentMode,
    TextInputVariant, Tooltip,
};

use super::DeveloperWorkbench;
use crate::ui::appearance::settings::SettingsAppearance;
use crate::ui::chrome_geometry::RadiusRole;
use crate::ui::sidebar_window::form::{FormGroup, FormRow};

/// How long the progress fixture stays open. It has no actions, so it closes itself.
const PROGRESS_DURATION: Duration = Duration::from_secs(5);

/// Presents one modal fixture over the Workbench.
type Presenter = fn(&mut Window, &mut Context<DeveloperWorkbench>);

pub(super) struct ModalFixtures {
    pub(super) obscured_input: Entity<TextInput>,
}

impl ModalFixtures {
    pub(super) fn new(window: &mut Window, cx: &mut Context<DeveloperWorkbench>) -> Self {
        Self {
            obscured_input: cx.new(|cx| {
                TextInput::new(
                    "workbench-obscured-input",
                    "Obscured input fixture",
                    "nonsecret-sample",
                    window,
                    cx,
                )
                .variant(TextInputVariant::Standard)
                .content_mode(TextInputContentMode::Obscured)
            }),
        }
    }

    pub(super) fn render(
        &self,
        surface: &SettingsAppearance,
        window: &Window,
        cx: &mut Context<DeveloperWorkbench>,
    ) -> Vec<AnyElement> {
        let appearance = &surface.chrome;
        let present = |id: &'static str, label: &'static str, present: Presenter| {
            let owner = cx.weak_entity();
            Button::new(id, label)
                .variant(ButtonVariant::Outline)
                .debug_selector(id)
                .on_activate(move |_, window, cx| {
                    let _ = owner.update(cx, |_, cx| present(window, cx));
                })
        };
        let modals = vec![
            FormRow::new(
                "workbench-row-modals-alert",
                "Alert",
                present("workbench-show-alert", "Show Alert", show_alert),
            )
            .description("A warning with semantic colors over the scrim.")
            .render(appearance, window, cx)
            .into_any_element(),
            FormRow::new(
                "workbench-row-modals-dialog",
                "Dialog",
                present("workbench-show-dialog", "Show Dialog", show_dialog),
            )
            .description("Nested controls and a tooltip only the dialog may show.")
            .render(appearance, window, cx)
            .into_any_element(),
            FormRow::new(
                "workbench-row-modals-progress",
                "Progress",
                present("workbench-show-progress", "Show Progress", show_progress),
            )
            .description("No actions and no empty footer. It closes after five seconds.")
            .render(appearance, window, cx)
            .into_any_element(),
        ];
        let input = self.obscured_input.clone();
        let focus = input.read(cx).focus_handle();
        let obscured = vec![
            FormRow::new(
                "workbench-row-obscured-field",
                "Secure field",
                spaceterm_ui::field_frame(
                    "workbench-obscured-field-frame",
                    &focus,
                    FieldState::default(),
                    RadiusRole::Control.pixels(),
                    cx,
                )
                .w(appearance.spacing(220.0))
                .h(appearance.spacing(28.0))
                .px(appearance.spacing(8.0))
                .child(input),
            )
            .render(appearance, window, cx)
            .into_any_element(),
            FormRow::new(
                "workbench-row-obscured-combo",
                "Combo box",
                ComboBox::new(
                    "workbench-obscured-combo",
                    "Obscured combo box fixture",
                    None,
                    "Choose a state",
                    vec![
                        ComboBoxItem::new(1_u8, "Normal"),
                        ComboBoxItem::new(2_u8, "Selected"),
                        ComboBoxItem::new(3_u8, "Warning"),
                    ],
                )
                .on_accept(|_, _, _| {}),
            )
            .render(appearance, window, cx)
            .into_any_element(),
            FormRow::new(
                "workbench-row-obscured-menu",
                "Menu",
                ContextMenu::new(
                    "workbench-obscured-menu",
                    "Obscured menu fixture",
                    Button::new("workbench-obscured-menu-trigger", "Secondary-Click Here")
                        .size(ButtonSize::Small),
                    vec![
                        MenuEntry::action("Normal item", 1_u8),
                        MenuEntry::action("Selected item", 2_u8),
                    ],
                )
                .on_activate(|_, _, _| {}),
            )
            .render(appearance, window, cx)
            .into_any_element(),
        ];
        vec![
            FormGroup::new("workbench-group-modals-families".to_owned(), "Modals", modals)
                .render(surface)
                .into_any_element(),
            FormGroup::new(
                "workbench-group-modals-obscured".to_owned(),
                "Obscured controls",
                obscured,
            )
            .render(surface)
            .into_any_element(),
        ]
    }
}

fn show_alert(window: &mut Window, cx: &mut Context<DeveloperWorkbench>) {
    let _ = Alert::new(
        ModalId::new("workbench-alert"),
        "Alert fixture",
        "Review the floating surface",
        "Check the warning, text, action states, and separation from the content behind it.",
        vec![ModalAction::new(
            (),
            "Close",
            ModalActionRole::Cancel,
            "workbench-alert-close",
        )],
    )
    .intent(AlertIntent::Warning)
    .present(window, cx, |_, _| {});
}

fn show_dialog(window: &mut Window, cx: &mut Context<DeveloperWorkbench>) {
    let body = cx.new(|cx| DialogBody::new(window, cx));
    let _ = Dialog::new(
        ModalId::new("workbench-dialog"),
        "Dialog fixture",
        "Dialog controls and help",
        vec![ModalAction::new(
            (),
            "Close",
            ModalActionRole::Cancel,
            "workbench-dialog-close",
        )],
        DialogInitialFocus::Action(()),
    )
    .description("Hover the help control. Only this dialog's tooltip may appear.")
    .body(body)
    .present(window, cx, |_, _, _| DialogCloseDecision::Allow, |_, _| {});
}

fn show_progress(window: &mut Window, cx: &mut Context<DeveloperWorkbench>) {
    let _ = ProgressDialog::<()>::new(
        ModalId::new("workbench-progress"),
        "Progress fixture",
        "Progress without actions",
        "Check that no empty action footer remains.",
        ProgressState::Indeterminate,
        ProgressCancellation::programmatic_only(PROGRESS_DURATION),
    )
    .detail("This fixture closes after five seconds.")
    .present(
        window,
        cx,
        |_, _, _| ProgressCancelDecision::Deny,
        |_, _| {},
    );
}

/// Controls nested inside a modal or a popover: a field, help with a tooltip, a combo box, button
/// variants, a switch, and a segmented control.
pub(super) struct DialogBody {
    input: Entity<TextInput>,
    switch_on: bool,
    second_segment: bool,
}

impl DialogBody {
    pub(super) fn new<T: 'static>(window: &mut Window, cx: &mut Context<T>) -> Self {
        Self {
            input: cx.new(|cx| {
                TextInput::new(
                    "workbench-dialog-input",
                    "Dialog field",
                    "Editable text",
                    window,
                    cx,
                )
            }),
            switch_on: false,
            second_segment: false,
        }
    }
}

impl Render for DialogBody {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let appearance = crate::ui::appearance::chrome(cx);
        div()
            .flex()
            .flex_col()
            .gap(appearance.spacing(10.0))
            .child(
                spaceterm_ui::field_frame(
                    "workbench-dialog-field",
                    &self.input.read(cx).focus_handle(),
                    FieldState::default(),
                    RadiusRole::Control.pixels(),
                    cx,
                )
                .h(appearance.spacing(32.0))
                .child(self.input.clone()),
            )
            .child(
                Button::new("workbench-dialog-help", "Hover for Help")
                    .on_activate(|_, _, _| {})
                    .tooltip(
                        Tooltip::new("workbench-dialog-tooltip", "This tooltip belongs here")
                            .detail("Tooltips behind a modal stay suppressed.")
                            .debug_selector("workbench-modal-owned-tooltip"),
                    ),
            )
            .child(
                ComboBox::new(
                    "workbench-dialog-combo",
                    "Nested combo box",
                    Some(1_u8),
                    "Choose",
                    vec![
                        ComboBoxItem::new(1, "Selected value"),
                        ComboBoxItem::new(2, "Alternative"),
                    ],
                )
                .on_accept(|_, _, _| {}),
            )
            .child(
                div().flex().gap(appearance.spacing(8.0)).children(
                    [
                        ("Outline", ButtonVariant::Outline, false),
                        ("Ghost", ButtonVariant::Ghost, false),
                        ("Disabled", ButtonVariant::Secondary, true),
                    ]
                    .into_iter()
                    .enumerate()
                    .map(|(index, (label, variant, disabled))| {
                        Button::new(("workbench-dialog-variant", index), label)
                            .variant(variant)
                            .disabled(disabled)
                            .on_activate(|_, _, _| {})
                    }),
                ),
            )
            .child(
                Switch::new("workbench-dialog-switch", "Switch", self.switch_on).on_change(
                    cx.listener(|body, change: &spaceterm_ui::SwitchChange, _, cx| {
                        body.switch_on = change.requested();
                        cx.notify();
                    }),
                ),
            )
            .children(
                SegmentedControl::new(
                    "workbench-dialog-segments",
                    "Segmented control",
                    &self.second_segment,
                    vec![
                        SegmentedOption::new(false, "First"),
                        SegmentedOption::new(true, "Second"),
                    ],
                )
                .ok()
                .map(|control| {
                    control.on_change(cx.listener(
                        |body, change: &spaceterm_ui::SegmentedChange<bool>, _, cx| {
                            body.second_segment = *change.requested();
                            cx.notify();
                        },
                    ))
                }),
            )
    }
}
