//! The Advanced section: Settings JSON, export and import of the Settings Document, and Reset All.
//!
//! Settings JSON is the one exception to instant apply (ADR 0005). A half-typed document is not a
//! setting, so the editor rests read-only and following the Settings Document, and an edit takes
//! effect only when the person applies it. Apply checks the text with the rules a Settings Document
//! meets at load time and changes nothing when the text fails them.

use std::sync::Arc;

use gpui::prelude::*;
use gpui::{AnyElement, Entity, SharedString, Window, div};
use spaceterm_ui::{
    Alert, AlertIntent, ButtonVariant, FieldState, ModalAction, ModalActionEmphasis,
    ModalActionIntent, ModalActionRole, ModalId, TextArea, TextAreaEvent,
};

use crate::appearance::{SettingsDocument, SettingsDocumentError, SettingsJsonError, parse_settings};
use crate::ui::appearance::{ChromeAppearance, gpui_color};
use crate::ui::chrome_geometry::RadiusRole;
use crate::ui::chrome_typography::{ChromeTextStyleExt as _, TextRole};

use super::SettingsWindow;
use super::controls::action_button;
use super::import::{ImportError, read_selected_document};

/// How many lines of Settings JSON show before the editor scrolls.
const SETTINGS_JSON_ROWS: usize = 16;

/// The Settings JSON editor, kept for the window's life so an edit survives changing sections.
pub(super) struct SettingsJsonEditor {
    pub(super) area: Entity<TextArea>,
    /// Whether the person is editing. At rest the text follows the Settings Document.
    pub(super) editing: bool,
    /// Why the latest Apply was refused. Editing the text withdraws it.
    pub(super) error: Option<SettingsJsonError>,
}

impl SettingsJsonEditor {
    pub(super) fn new(window: &mut Window, cx: &mut Context<SettingsWindow>) -> Self {
        let area = cx.new(|cx| {
            TextArea::new("settings-json", "Settings JSON", String::new(), window, cx)
                .editable(false)
                .line_numbers(true)
                .rows(SETTINGS_JSON_ROWS)
                .input_length_limit(None)
                .debug_selector("settings-json-editor")
        });
        cx.subscribe(&area, |settings, _, event: &TextAreaEvent, cx| {
            if matches!(event, TextAreaEvent::ValueChanged { .. })
                && settings.settings_json.error.take().is_some()
            {
                cx.notify();
            }
        })
        .detach();
        Self {
            area,
            editing: false,
            error: None,
        }
    }
}

impl SettingsWindow {
    /// The Settings JSON editor with its caption and actions.
    pub(super) fn render_settings_json(
        &mut self,
        appearance: &ChromeAppearance,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        self.follow_settings_json(cx);
        let editing = self.settings_json.editing;
        let error = self.settings_json.error;
        let editable = self.editor.editable();
        let area = self.settings_json.area.clone();
        let focus = area.read(cx).focus_handle();
        let colors = appearance.host_colors(spaceterm_ui::ControlHost::Card);
        let frame = spaceterm_ui::field_frame(
            "settings-json-frame",
            &focus,
            FieldState::default().invalid(error.is_some()),
            RadiusRole::Control.pixels(),
            cx,
        )
        .debug_selector(|| "settings-json-frame".to_owned())
        .w_full()
        .min_w_0()
        .px(appearance.spacing(8.0))
        .py(appearance.spacing(6.0))
        .chrome_text(appearance.typography.style(TextRole::Secondary))
        .font_family(crate::bundled_font::FAMILY)
        .child(area);
        let (caption, caption_color) = match error {
            Some(error) => (settings_json_error_message(error), colors.error),
            None if editing => (
                SharedString::from("Changes take effect when you apply them."),
                colors.text_muted,
            ),
            None => (
                SharedString::from("Every setting and keyboard shortcut. Installed themes are not included."),
                colors.text_muted,
            ),
        };
        let owner = cx.weak_entity();
        let actions = if editing {
            let cancel = owner.clone();
            vec![
                action_button("settings-json-cancel", "Cancel", true, move |window, cx| {
                    let _ = cancel.update(cx, |settings, cx| {
                        settings.end_settings_json_edit(cx);
                        settings.focus_handle.focus(window, cx);
                    });
                }),
                action_button("settings-json-apply", "Apply", editable, move |window, cx| {
                    let _ = owner.update(cx, |settings, cx| {
                        settings.apply_settings_json(window, cx);
                    });
                })
                .variant(ButtonVariant::Primary),
            ]
        } else {
            vec![action_button(
                "settings-json-edit",
                "Edit JSON",
                editable,
                move |window, cx| {
                    let _ = owner.update(cx, |settings, cx| {
                        settings.begin_settings_json_edit(window, cx);
                    });
                },
            )]
        };
        div()
            .flex()
            .flex_col()
            .w_full()
            .min_w_0()
            .gap(appearance.spacing(8.0))
            .child(frame)
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_start()
                    .w_full()
                    .gap(appearance.spacing(12.0))
                    .child(
                        div()
                            .debug_selector(|| "settings-json-caption".to_owned())
                            .flex_1()
                            .min_w_0()
                            .chrome_text(appearance.typography.style(TextRole::Secondary))
                            .text_color(gpui_color(caption_color))
                            .whitespace_normal()
                            .child(caption),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .flex_none()
                            .gap(appearance.spacing(6.0))
                            .children(actions),
                    ),
            )
            .into_any_element()
    }

    /// Keeps the resting text equal to the Settings Document's Settings JSON.
    fn follow_settings_json(&mut self, cx: &mut Context<Self>) {
        if self.settings_json.editing {
            return;
        }
        let Ok(json) = self.editor.document().settings_json() else {
            return;
        };
        self.settings_json.area.update(cx, |area, cx| {
            if area.value() != json {
                area.set_value(json, cx);
            }
        });
    }

    fn begin_settings_json_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.editor.editable() {
            return;
        }
        self.settings_json.editing = true;
        let focus = self.settings_json.area.update(cx, |area, cx| {
            area.set_editable(true, cx);
            area.focus_handle()
        });
        focus.focus(window, cx);
        cx.notify();
    }

    /// Leaves editing and returns the text to the Settings Document's.
    pub(super) fn end_settings_json_edit(&mut self, cx: &mut Context<Self>) {
        if !self.settings_json.editing {
            return;
        }
        self.settings_json.editing = false;
        self.settings_json.error = None;
        self.settings_json
            .area
            .update(cx, |area, cx| area.set_editable(false, cx));
        self.follow_settings_json(cx);
        cx.notify();
    }

    /// Applies the edited text, or explains why not and puts the caret where the fault is.
    fn apply_settings_json(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.editor.editable() {
            return;
        }
        let text = self.settings_json.area.read(cx).value().to_owned();
        let mut result = Ok(());
        self.editor.edit(
            |document| result = document.apply_settings_json(&text),
            cx,
        );
        match result {
            Ok(()) => {
                self.end_settings_json_edit(cx);
                self.focus_handle.focus(window, cx);
            }
            Err(error) => {
                self.settings_json.error = Some(error);
                let focus = self.settings_json.area.update(cx, |area, cx| {
                    if let Some(at) = error.position() {
                        area.move_caret_to_position(at.line, at.column, cx);
                    }
                    area.focus_handle()
                });
                focus.focus(window, cx);
                cx.notify();
            }
        }
    }

    /// Asks for an exported Settings Document and offers to replace every setting with it.
    pub(super) fn begin_settings_import(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(opener) = cx
            .try_global::<crate::app::SelectedFileAccess>()
            .map(|access| Arc::clone(&access.0))
        else {
            present_import_failure("File import is unavailable.", window, cx);
            return;
        };
        let selection = cx.prompt_for_paths(gpui::PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Import".into()),
        });
        cx.spawn_in(window, async move |owner, cx| {
            let Ok(Ok(Some(paths))) = selection.await else {
                return;
            };
            let Some(path) = paths.into_iter().next() else {
                return;
            };
            let read = cx
                .background_executor()
                .spawn(async move { read_selected_document(&path, opener.as_ref()) })
                .await;
            let _ = owner.update_in(cx, |settings, window, cx| {
                settings.finish_settings_import(read, window, cx);
            });
        })
        .detach();
    }

    /// Checks the chosen file as a whole Settings Document, then confirms replacing everything.
    pub(super) fn finish_settings_import(
        &mut self,
        read: Result<Vec<u8>, ImportError>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let imported = match read.map(|bytes| parse_settings(&bytes)) {
            Ok(Ok(document)) => document,
            Ok(Err(SettingsDocumentError::UnsupportedVersion)) => {
                return present_import_failure(
                    "That file comes from a version of SpaceTerm this one cannot read.",
                    window,
                    cx,
                );
            }
            Ok(Err(_)) => {
                return present_import_failure(
                    "That file is not a valid SpaceTerm settings file.",
                    window,
                    cx,
                );
            }
            Err(ImportError::TooLarge) => {
                return present_import_failure(
                    "That file is too large to be a settings file.",
                    window,
                    cx,
                );
            }
            Err(ImportError::Unreadable) => {
                return present_import_failure("That file could not be read.", window, cx);
            }
        };
        self.confirm_settings_import(imported, window, cx);
    }

    fn confirm_settings_import(
        &mut self,
        imported: SettingsDocument,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let owner = cx.weak_entity();
        let result = Alert::new(
            ModalId::new("settings-import"),
            "Import settings",
            "Import Settings",
            "Every setting, keyboard shortcut, and installed terminal theme is replaced by the ones in this file.",
            vec![
                ModalAction::new(
                    true,
                    "Replace",
                    ModalActionRole::Affirmative,
                    "settings-import-confirm",
                )
                .with_intent(ModalActionIntent::Destructive)
                .with_emphasis(ModalActionEmphasis::Prominent),
                ModalAction::new(
                    false,
                    "Cancel",
                    ModalActionRole::Cancel,
                    "settings-import-cancel",
                ),
            ],
        )
        .intent(AlertIntent::Critical)
        .detail("This cannot be undone. Export your settings first to keep a copy.")
        .present(window, cx, move |outcome, cx| {
            if !matches!(
                outcome,
                spaceterm_ui::AlertOutcome::Activated {
                    action_id: true,
                    ..
                }
            ) {
                return;
            }
            let _ = owner.update(cx, |settings, cx| {
                settings.shortcuts.dismiss_notice();
                settings.end_settings_json_edit(cx);
                settings
                    .editor
                    .edit(|document| document.replace_settings(imported), cx);
                cx.notify();
            });
        });
        if result.is_err() {
            eprintln!("failed to present the SpaceTerm settings import confirmation");
        }
    }
}

fn present_import_failure(detail: &'static str, window: &mut Window, cx: &mut Context<SettingsWindow>) {
    let _ = Alert::new(
        ModalId::new("settings-import-failed"),
        "Import failed",
        "Import Failed",
        detail,
        vec![ModalAction::new(
            (),
            "OK",
            ModalActionRole::Cancel,
            "settings-import-failed-ok",
        )],
    )
    .intent(AlertIntent::Warning)
    .present(window, cx, |_, _| {});
}

/// Content-free wording for a refused Apply. A position names where, never what was there.
pub(super) fn settings_json_error_message(error: SettingsJsonError) -> SharedString {
    let located = |at: crate::appearance::JsonPosition, fault: &str| {
        SharedString::from(format!("Line {}, column {} {fault}", at.line, at.column))
    };
    match error {
        SettingsJsonError::TooLarge => "The JSON is too large.".into(),
        SettingsJsonError::UnexpectedEnd => "The JSON ends before it is complete.".into(),
        SettingsJsonError::Syntax(at) => located(at, "is not valid JSON."),
        SettingsJsonError::DuplicateKey(at) => located(at, "repeats a key."),
        SettingsJsonError::TooDeep(at) => located(at, "is nested too deeply."),
        SettingsJsonError::Structure(at) => located(at, "does not match the settings format."),
        SettingsJsonError::InvalidPreferences => {
            "A setting has a value SpaceTerm does not accept.".into()
        }
        SettingsJsonError::InvalidKeybindings => "A keyboard shortcut is not valid.".into(),
    }
}
