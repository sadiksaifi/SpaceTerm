use gpui::prelude::*;
use gpui::{Context, Entity, IntoElement, Render, Rgba, Window, div, px, rgba};
use spaceterm_ui::{
    Alert, AlertAccessory, AlertIntent, AlertSuppression, Button, ButtonSize, ButtonVariant,
    DeterminateProgress, Dialog, DialogCloseDecision, DialogInitialFocus, DialogSize, ModalAction,
    ModalActionEmphasis, ModalActionIntent, ModalActionRole, ModalId, ProgressCancelDecision,
    ProgressCancellation, ProgressDialog, ProgressState, TextInput, TextInputEscapeBehavior,
    TextInputReturnBehavior,
};

use crate::theme::{ACTIVE_THEME, Color};

const PANEL_WIDTH: f32 = 292.0;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DialogAction {
    Apply,
    Cancel,
}

struct DialogBody {
    name: Entity<TextInput>,
}

impl Render for DialogBody {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .flex_col()
            .gap(px(8.0))
            .child(
                div()
                    .text_size(px(12.0))
                    .text_color(gpui_color(ACTIVE_THEME.text))
                    .child("Display name"),
            )
            .child(
                div()
                    .h(px(32.0))
                    .w_full()
                    .px(px(8.0))
                    .flex()
                    .items_center()
                    .rounded(px(5.0))
                    .border(px(1.0))
                    .border_color(gpui_color(ACTIVE_THEME.border))
                    .bg(gpui_color(ACTIVE_THEME.element_background))
                    .child(self.name.clone()),
            )
            .child(
                div()
                    .text_size(px(11.0))
                    .text_color(gpui_color(ACTIVE_THEME.text_muted))
                    .child("Try pointer input, Tab, Return, and Escape."),
            )
    }
}

pub(super) struct ComponentShowcase;

impl ComponentShowcase {
    pub(super) fn new() -> Self {
        Self
    }

    fn show_alert(&mut self, intent: AlertIntent, window: &Window, cx: &mut Context<Self>) {
        let alert = match intent {
            AlertIntent::Informational => Alert::new(
                ModalId::new("showcase-informational-alert"),
                "Informational Alert preview",
                "Workspace Ready",
                "Your workspace is configured and ready to use.",
                vec![
                    ModalAction::new("ok", "OK", ModalActionRole::Affirmative, "showcase-info-ok")
                        .default_action(true),
                ],
            )
            .detail(
                "This preview shows the informational marker, detail text, and default action.",
            ),
            AlertIntent::Warning => Alert::new(
                ModalId::new("showcase-warning-alert"),
                "Warning Alert preview",
                "Replace Existing Settings?",
                "The existing workspace settings will be replaced.",
                vec![
                    ModalAction::new(
                        "replace",
                        "Replace",
                        ModalActionRole::Affirmative,
                        "showcase-warning-replace",
                    )
                    .default_action(true),
                    ModalAction::new(
                        "cancel",
                        "Cancel",
                        ModalActionRole::Cancel,
                        "showcase-warning-cancel",
                    ),
                ],
            )
            .detail("You can safely cancel this visual preview. No settings are changed.")
            .accessory(AlertAccessory::icon("Settings replacement warning", None))
            .suppression(AlertSuppression::new(
                "Do not show this warning again",
                false,
            ))
            .help_action(ModalAction::new(
                "help",
                "Help",
                ModalActionRole::Help,
                "showcase-warning-help",
            )),
            AlertIntent::Critical => Alert::new(
                ModalId::new("showcase-critical-alert"),
                "Critical Alert preview",
                "Delete Workspace?",
                "This action permanently removes the workspace from SpaceTerm.",
                vec![
                    ModalAction::new(
                        "delete",
                        "Delete",
                        ModalActionRole::Affirmative,
                        "showcase-critical-delete",
                    )
                    .with_intent(ModalActionIntent::Destructive)
                    .with_emphasis(ModalActionEmphasis::Prominent)
                    .default_action(true),
                    ModalAction::new(
                        "cancel",
                        "Cancel",
                        ModalActionRole::Cancel,
                        "showcase-critical-cancel",
                    ),
                ],
            )
            .detail("Preview only. Activating Delete does not change application state."),
        }
        .intent(intent);

        if let Err(error) = alert.present(window, cx, |_, _| {}) {
            eprintln!("failed to present modal showcase Alert: {error}");
        }
    }

    fn show_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let input = cx.new(|cx| {
            TextInput::new(
                "showcase-dialog-name",
                "Display name",
                "SpaceTerm User",
                window,
                cx,
            )
            .placeholder("Enter a display name")
            .return_behavior(TextInputReturnBehavior::Propagate)
            .escape_behavior(TextInputEscapeBehavior::Propagate)
            .debug_selector("showcase-dialog-name")
        });
        let initial_focus = input.read(cx).focus_handle();
        let body = cx.new(|_| DialogBody { name: input });
        let dialog = Dialog::new(
            ModalId::new("showcase-form-dialog"),
            "Form Dialog preview",
            "Edit Profile",
            vec![
                ModalAction::new(
                    DialogAction::Apply,
                    "Apply",
                    ModalActionRole::Affirmative,
                    "showcase-dialog-apply",
                )
                .with_emphasis(ModalActionEmphasis::Prominent)
                .default_action(true),
                ModalAction::new(
                    DialogAction::Cancel,
                    "Cancel",
                    ModalActionRole::Cancel,
                    "showcase-dialog-cancel",
                ),
            ],
            DialogInitialFocus::Body(initial_focus),
        )
        .description("A caller-owned form body inside the reusable Dialog surface.")
        .size(DialogSize::Regular)
        .body(body);

        if let Err(error) =
            dialog.present(window, cx, |_, _, _| DialogCloseDecision::Allow, |_, _| {})
        {
            eprintln!("failed to present modal showcase Dialog: {error}");
        }
    }

    fn show_progress(&mut self, determinate: bool, window: &Window, cx: &mut Context<Self>) {
        let progress = if determinate {
            let Ok(value) = DeterminateProgress::new(0.42) else {
                eprintln!("failed to construct the modal showcase progress value");
                return;
            };
            ProgressState::Determinate(value)
        } else {
            ProgressState::Indeterminate
        };
        let (id, accessibility_title, title, status, detail) = if determinate {
            (
                "showcase-determinate-progress",
                "Determinate Progress Dialog preview",
                "Preparing Workspace",
                "Copying application resources",
                "42 of 100 resources copied",
            )
        } else {
            (
                "showcase-indeterminate-progress",
                "Indeterminate Progress Dialog preview",
                "Connecting to Workspace",
                "Waiting for the remote environment",
                "This preview stays open until you cancel it.",
            )
        };
        let progress_dialog = ProgressDialog::new(
            ModalId::new(id),
            accessibility_title,
            title,
            status,
            progress,
            ProgressCancellation::Cancellable(ModalAction::new(
                "cancel",
                "Cancel",
                ModalActionRole::Cancel,
                "showcase-progress-cancel",
            )),
        )
        .detail(detail);

        if let Err(error) = progress_dialog.present(
            window,
            cx,
            |_, _, _| ProgressCancelDecision::Allow,
            |_, _| {},
        ) {
            eprintln!("failed to present modal showcase ProgressDialog: {error}");
        }
    }
}

impl Render for ComponentShowcase {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let showcase = cx.entity().downgrade();
        let info_showcase = showcase.clone();
        let warning_showcase = showcase.clone();
        let critical_showcase = showcase.clone();
        let dialog_showcase = showcase.clone();
        let determinate_showcase = showcase.clone();
        let indeterminate_showcase = showcase;

        div()
            .id("modal-showcase")
            .debug_selector(|| "modal-showcase".to_owned())
            .absolute()
            .right(px(12.0))
            .bottom(px(12.0))
            .w(px(PANEL_WIDTH))
            .p(px(12.0))
            .flex()
            .flex_col()
            .gap(px(10.0))
            .rounded(px(8.0))
            .border(px(1.0))
            .border_color(gpui_color(ACTIVE_THEME.border))
            .bg(gpui_color(ACTIVE_THEME.elevated_surface_background))
            .occlude()
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(2.0))
                    .child(
                        div()
                            .text_size(px(13.0))
                            .text_color(gpui_color(ACTIVE_THEME.text))
                            .child("Modal showcase"),
                    )
                    .child(
                        div()
                            .text_size(px(11.0))
                            .text_color(gpui_color(ACTIVE_THEME.text_muted))
                            .child("Temporary visual test controls"),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_row()
                    .gap(px(6.0))
                    .child(
                        Button::new("showcase-info-alert", "Info")
                            .variant(ButtonVariant::Secondary)
                            .size(ButtonSize::Small)
                            .on_activate(move |_, window, cx| {
                                let _ = info_showcase.update(cx, |showcase, cx| {
                                    showcase.show_alert(AlertIntent::Informational, window, cx);
                                });
                            }),
                    )
                    .child(
                        Button::new("showcase-warning-alert", "Warning")
                            .variant(ButtonVariant::Outline)
                            .size(ButtonSize::Small)
                            .on_activate(move |_, window, cx| {
                                let _ = warning_showcase.update(cx, |showcase, cx| {
                                    showcase.show_alert(AlertIntent::Warning, window, cx);
                                });
                            }),
                    )
                    .child(
                        Button::new("showcase-critical-alert", "Critical")
                            .variant(ButtonVariant::Destructive)
                            .size(ButtonSize::Small)
                            .on_activate(move |_, window, cx| {
                                let _ = critical_showcase.update(cx, |showcase, cx| {
                                    showcase.show_alert(AlertIntent::Critical, window, cx);
                                });
                            }),
                    ),
            )
            .child(
                Button::new("showcase-dialog", "Open Form Dialog")
                    .variant(ButtonVariant::Primary)
                    .size(ButtonSize::Regular)
                    .full_width(true)
                    .on_activate(move |_, window, cx| {
                        let _ = dialog_showcase.update(cx, |showcase, cx| {
                            showcase.show_dialog(window, cx);
                        });
                    }),
            )
            .child(
                div()
                    .flex()
                    .flex_row()
                    .gap(px(6.0))
                    .child(
                        Button::new("showcase-determinate-progress", "42% Progress")
                            .variant(ButtonVariant::Secondary)
                            .size(ButtonSize::Small)
                            .on_activate(move |_, window, cx| {
                                let _ = determinate_showcase.update(cx, |showcase, cx| {
                                    showcase.show_progress(true, window, cx);
                                });
                            }),
                    )
                    .child(
                        Button::new("showcase-indeterminate-progress", "Indeterminate")
                            .variant(ButtonVariant::Secondary)
                            .size(ButtonSize::Small)
                            .on_activate(move |_, window, cx| {
                                let _ = indeterminate_showcase.update(cx, |showcase, cx| {
                                    showcase.show_progress(false, window, cx);
                                });
                            }),
                    ),
            )
    }
}

fn gpui_color(color: Color) -> Rgba {
    rgba(color.rgba_hex())
}
