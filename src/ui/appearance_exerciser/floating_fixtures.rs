//! Native acceptance support using production controls over synthetic high-contrast content.

use std::time::Duration;

use gpui::prelude::*;
use gpui::{App, Context, Entity, Render, Window, div, px, rgba};
use spaceterm_ui::{
    Alert, AlertIntent, Button, ButtonSize, ButtonVariant, ComboBox, ComboBoxItem, CommandPalette,
    CommandPaletteItem, ContextMenu, Dialog, DialogCloseDecision, DialogInitialFocus, Menu,
    MenuEntry, MenuSize, ModalAction, ModalActionRole, ModalId, Picker, PickerOption,
    ProgressCancelDecision, ProgressCancellation, ProgressDialog, ProgressState, TextInput,
    Tooltip,
};

use super::{AppearanceExerciser, ExerciserWindows};
use crate::appearance::{AppearanceMode, ChromeDensity, SettingsDocument};

/// Acceptance controls whose palette is hosted by the window's transient layer.
pub(super) struct FloatingFixtures {
    picker_value: u8,
    combo_value: Option<u8>,
    palette: Entity<CommandPalette<u8>>,
    status: &'static str,
}

#[derive(Clone, Copy)]
enum PreviewChoice {
    Light,
    Dark,
    Compact,
    Comfortable,
    Opaque,
    DefaultTransparency,
    MaximumTransparency,
    BlurOn,
    BlurOff,
}

impl PreviewChoice {
    fn apply(self, document: &mut SettingsDocument) {
        match self {
            Self::Light => document.preferences.mode = AppearanceMode::Light,
            Self::Dark => document.preferences.mode = AppearanceMode::Dark,
            Self::Compact => document.preferences.chrome.density = ChromeDensity::Compact,
            Self::Comfortable => {
                document.preferences.chrome.density = ChromeDensity::Comfortable;
            }
            Self::Opaque => document.preferences.background.transparency = 0.0,
            Self::DefaultTransparency => {
                document.preferences.background.transparency = SettingsDocument::default()
                    .preferences
                    .background
                    .transparency;
            }
            Self::MaximumTransparency => document.preferences.background.transparency = 1.0,
            Self::BlurOn => document.preferences.background.blur = true,
            Self::BlurOff => document.preferences.background.blur = false,
        }
    }
}

fn apply_preview(choice: PreviewChoice, cx: &mut App) -> Result<(), &'static str> {
    if !cx.has_global::<ExerciserWindows>() {
        return Err("Open these fixtures from the Appearance Exerciser.");
    }
    let exerciser = cx.global::<ExerciserWindows>().appearance;
    exerciser
        .update(cx, |exerciser, _, cx| {
            exerciser.ensure_preview(cx)?;
            let settings = AppearanceExerciser::settings(cx);
            let mut document = (*settings.snapshot().candidate).clone();
            choice.apply(&mut document);
            let Some(preview) = exerciser.preview.as_ref() else {
                return Err("Appearance preview is unavailable.");
            };
            settings
                .update_preview(preview, document)
                .map_err(|_| "Appearance preview update was rejected.")?;
            exerciser.export_settings_to_editor(cx);
            cx.notify();
            Ok(())
        })
        .map_err(|_| "The Appearance Exerciser window is unavailable.")?
}

fn menu_entries() -> Vec<MenuEntry<u8>> {
    vec![
        MenuEntry::action("Open synthetic item", 1),
        MenuEntry::checkbox("Checked synthetic item", true, 2),
        MenuEntry::separator(),
        MenuEntry::action("Unavailable synthetic item", 3).disabled(true),
        MenuEntry::submenu(
            "Nested actions",
            vec![MenuEntry::action("Nested synthetic item", 4)],
        ),
    ]
}

impl FloatingFixtures {
    pub(super) fn palette(&self) -> Entity<CommandPalette<u8>> {
        self.palette.clone()
    }

    pub(super) fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        cx.observe_global_in::<super::appearance_runtime::InstalledAppearance>(
            window,
            |_, _, cx| cx.notify(),
        )
        .detach();
        let palette = cx.new(|cx| {
            CommandPalette::new(
                "Search synthetic commands",
                vec![
                    CommandPaletteItem::new(1, "Open synthetic item")
                        .description("Hover the selected result to inspect the combined state"),
                    CommandPaletteItem::new(2, "Open another synthetic item")
                        .description("Secondary text must remain legible over the pattern"),
                    CommandPaletteItem::new(3, "Unavailable synthetic item").disabled(true),
                ],
                window,
                cx,
            )
        });
        Self {
            picker_value: 1,
            combo_value: Some(1),
            palette,
            status: "All content below is synthetic. Preview changes are not saved.",
        }
    }

    fn show_alert(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let _ = Alert::new(
            ModalId::new("floating-acceptance-alert"),
            "Synthetic alert",
            "Review the floating surface",
            "Check the warning, text, action states, and separation from the patterned content.",
            vec![ModalAction::new(
                (),
                "Close",
                ModalActionRole::Cancel,
                "fixture-alert-close",
            )],
        )
        .intent(AlertIntent::Warning)
        .present(window, cx, |_, _| {});
    }

    fn show_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let body = cx.new(|cx| TooltipDialogBody::new(window, cx));
        let _ = Dialog::new(
            ModalId::new("floating-acceptance-dialog"),
            "Synthetic dialog",
            "Dialog controls and hover help",
            vec![ModalAction::new(
                (),
                "Close",
                ModalActionRole::Cancel,
                "fixture-dialog-close",
            )],
            DialogInitialFocus::Action(()),
        )
        .description("Hover the help control. Only this dialog's tooltip may appear.")
        .body(body)
        .present(window, cx, |_, _, _| DialogCloseDecision::Allow, |_, _| {});
    }

    fn show_progress(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let _ = ProgressDialog::<()>::new(
            ModalId::new("floating-acceptance-progress"),
            "Synthetic progress",
            "Progress without actions",
            "Check that no empty action footer remains.",
            ProgressState::Indeterminate,
            ProgressCancellation::programmatic_only(Duration::from_secs(5)),
        )
        .detail("This fixture closes after five seconds.")
        .present(
            window,
            cx,
            |_, _, _| ProgressCancelDecision::Deny,
            |_, _| {},
        );
    }
}

impl Render for FloatingFixtures {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let appearance = super::super::appearance::chrome(cx).clone();
        let preview_controls = div()
            .flex()
            .flex_wrap()
            .gap(appearance.spacing(6.0))
            .children([
                ("fixture-light", "Light", PreviewChoice::Light),
                ("fixture-dark", "Dark", PreviewChoice::Dark),
                ("fixture-compact", "Compact", PreviewChoice::Compact),
                ("fixture-comfortable", "Comfortable", PreviewChoice::Comfortable),
                ("fixture-opaque", "Transparency 0", PreviewChoice::Opaque),
                ("fixture-default", "Default transparency", PreviewChoice::DefaultTransparency),
                ("fixture-maximum", "Transparency 1", PreviewChoice::MaximumTransparency),
                ("fixture-blur-on", "Blur on", PreviewChoice::BlurOn),
                ("fixture-blur-off", "Blur off", PreviewChoice::BlurOff),
            ].into_iter().map(|(id, label, choice)| {
                let weak = cx.weak_entity();
                Button::new(id, label)
                    .size(ButtonSize::Small)
                    .variant(ButtonVariant::Secondary)
                    .on_activate(move |_, _, cx| {
                        let result = apply_preview(choice, cx);
                        let _ = weak.update(cx, |fixture, cx| {
                            fixture.status = result.map_or_else(|error| error, |()| "Preview applied. Use the Appearance Exerciser to cancel or commit.");
                            cx.notify();
                        });
                    })
            }));
        let weak = cx.weak_entity();
        let picker = Picker::new(
            "fixture-picker",
            "Synthetic picker",
            self.picker_value,
            vec![
                PickerOption::new(1, "Picker: first option"),
                PickerOption::new(2, "Picker: second option"),
                PickerOption::new(3, "Picker: unavailable option").disabled(true),
            ],
        )
        .ok()
        .map(|picker| {
            picker.on_change(move |change, _, cx| {
                let _ = weak.update(cx, |fixture, cx| {
                    fixture.picker_value = *change.value();
                    cx.notify();
                });
            })
        });
        let weak = cx.weak_entity();
        let combo = ComboBox::new(
            "fixture-combo",
            "Synthetic combo box",
            self.combo_value,
            "Choose a synthetic result",
            vec![
                ComboBoxItem::new(1, "ComboBox: first result").description("Selected description"),
                ComboBoxItem::new(2, "ComboBox: second result").description("Another description"),
                ComboBoxItem::new(3, "ComboBox: unavailable result").disabled(true),
            ],
        )
        .on_accept(move |accepted, _, cx| {
            let _ = weak.update(cx, |fixture, cx| {
                fixture.combo_value = Some(*accepted.item_id());
                cx.notify();
            });
        });
        let palette = self.palette.clone();
        let modal_button =
            |id: &'static str,
             label: &'static str,
             handler: fn(&mut Self, &mut Window, &mut Context<Self>)| {
                let weak = cx.weak_entity();
                Button::new(id, label)
                    .size(ButtonSize::Small)
                    .on_activate(move |_, window, cx| {
                        let _ = weak.update(cx, |fixture, cx| handler(fixture, window, cx));
                    })
            };
        let controls = div()
            .flex()
            .flex_wrap()
            .gap(appearance.spacing(8.0))
            .children(
                [
                    ("fixture-small-menu", "Small Menu", MenuSize::Small),
                    ("fixture-regular-menu", "Regular Menu", MenuSize::Regular),
                    ("fixture-wide-menu", "Wide Menu", MenuSize::Wide),
                ]
                .into_iter()
                .map(|(id, label, size)| {
                    Menu::new(id, label, menu_entries())
                        .size(size)
                        .on_activate(|_, _, _| {})
                }),
            )
            .when_some(picker, |controls, picker| controls.child(picker))
            .child(combo)
            .child(
                ContextMenu::new(
                    "fixture-context-menu",
                    "Synthetic context menu",
                    Button::new("fixture-context-target", "Right-click for ContextMenu")
                        .size(ButtonSize::Small),
                    menu_entries(),
                )
                .on_activate(|_, _, _| {}),
            )
            .child(
                Button::new("fixture-open-palette", "CommandPalette")
                    .size(ButtonSize::Small)
                    .on_activate(move |_, window, cx| {
                        palette.update(cx, |palette, cx| {
                            palette.open(window, cx);
                        });
                    }),
            )
            .child(
                Button::new("fixture-tooltip-target", "Hover for Tooltip")
                    .size(ButtonSize::Small)
                    .on_activate(|_, _, _| {})
                    .tooltip(
                        Tooltip::new("fixture-tooltip", "Synthetic tooltip over detailed content")
                            .detail("The foreground and edge must stay legible.")
                            .debug_selector("fixture-floating-tooltip"),
                    ),
            )
            .child(modal_button("fixture-alert", "Alert", Self::show_alert))
            .child(modal_button(
                "fixture-dialog",
                "Dialog + Tooltip",
                Self::show_dialog,
            ))
            .child(modal_button(
                "fixture-progress",
                "ProgressDialog",
                Self::show_progress,
            ));

        // Deliberately hostile synthetic content, rather than another themed application surface.
        let pattern = div().absolute().inset_0().overflow_hidden().flex().flex_col()
            .children((0..24).map(|row| {
                let (background, foreground) = if row % 2 == 0 {
                    (rgba(0xf5f5f5ff), rgba(0x111111ff))
                } else {
                    (rgba(0x111111ff), rgba(0xf5f5f5ff))
                };
                div().h(px(18.0)).flex_none().bg(background).text_color(foreground)
                    .text_size(px(12.0)).overflow_hidden()
                    .child("SYNTHETIC 0123456789 ABCDEFGHIJKLMNOPQRSTUVWXYZ · fine text behind a floating surface · ".repeat(4))
            }));
        div()
            .flex()
            .flex_col()
            .gap(appearance.spacing(10.0))
            .child("Floating surface acceptance")
            .child(preview_controls)
            .child(self.status)
            .child(
                div()
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .bg(rgba(appearance.colors.panel_background.rgba_hex()))
                            .text_color(rgba(appearance.colors.text.rgba_hex()))
                            .p(appearance.spacing(8.0))
                            .child(controls),
                    )
                    .child(div().relative().h(px(432.0)).child(pattern)),
            )
    }
}

struct TooltipDialogBody {
    input: Entity<TextInput>,
}

impl TooltipDialogBody {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        Self {
            input: cx.new(|cx| {
                TextInput::new(
                    "fixture-dialog-input",
                    "Synthetic dialog field",
                    "Editable synthetic text",
                    window,
                    cx,
                )
            }),
        }
    }
}

impl Render for TooltipDialogBody {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let appearance = super::super::appearance::chrome(cx);
        div()
            .flex()
            .flex_col()
            .gap(appearance.spacing(10.0))
            .child(
                spaceterm_ui::field_frame(
                    "fixture-dialog-field",
                    &self.input.read(cx).focus_handle(),
                    spaceterm_ui::FieldState::default(),
                    cx,
                )
                .h(appearance.height(32.0, 13.0))
                .child(self.input.clone()),
            )
            .child(
                Button::new("fixture-dialog-help", "Hover for dialog-owned help")
                    .on_activate(|_, _, _| {})
                    .tooltip(
                        Tooltip::new(
                            "fixture-dialog-tooltip",
                            "This tooltip belongs to the dialog",
                        )
                        .detail("Background tooltips must remain suppressed.")
                        .debug_selector("fixture-modal-owned-tooltip"),
                    ),
            )
            .child(
                ComboBox::new(
                    "fixture-dialog-combo",
                    "Dialog nested control",
                    Some(1_u8),
                    "Choose",
                    vec![
                        ComboBoxItem::new(1, "Nested selected value"),
                        ComboBoxItem::new(2, "Nested alternative"),
                    ],
                )
                .on_accept(|_, _, _| {}),
            )
    }
}
