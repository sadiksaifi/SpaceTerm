//! The Advanced section: the settings file, Settings Document export and import, and Reset All.
//! The settings file is read-only here because SpaceTerm follows external edits.

use std::sync::Arc;

use gpui::prelude::*;
use gpui::{AnyElement, Entity, Window, div};
use spaceterm_ui::{
    Alert, AlertIntent, FieldState, ModalAction, ModalActionEmphasis, ModalActionIntent,
    ModalActionRole, ModalId, TextArea,
};

use crate::settings::{SettingsDocument, SettingsDocumentError, export_settings, parse_settings};
use crate::ui::appearance::{ChromeAppearance, gpui_color};
use crate::ui::chrome_geometry::RadiusRole;
use crate::ui::chrome_typography::{ChromeTextStyleExt as _, TextRole};
use crate::ui::settings_file::SettingsFile;

use super::SettingsWindow;
use super::import::{ImportError, read_selected_document};
use crate::ui::sidebar_window::form::action_button;

/// How many lines of the settings file show before the view scrolls.
const SETTINGS_FILE_ROWS: usize = 16;

/// The read-only view of the settings file, kept for the window's life so its scroll position
/// survives changing sections.
pub(super) struct SettingsFileView {
    pub(super) area: Entity<TextArea>,
    /// The document the text shows. A replaced draft is a changed document.
    shown: Option<Arc<SettingsDocument>>,
}

impl SettingsFileView {
    pub(super) fn new(window: &mut Window, cx: &mut Context<SettingsWindow>) -> Self {
        let area = cx.new(|cx| {
            TextArea::new("settings-file", "Settings file", String::new(), window, cx)
                .editable(false)
                .line_numbers(true)
                .rows(SETTINGS_FILE_ROWS)
                .input_length_limit(None)
                // Every Settings Document prints as text, so the view is empty only when that
                // text exceeds what a text area holds.
                .placeholder("The settings file is too large to show here. Edit JSON opens it.")
                .debug_selector("settings-file-text")
        });
        Self { area, shown: None }
    }
}

impl SettingsWindow {
    /// The settings file, where it lives, and the actions that reload it and open it in the
    /// person's editor.
    pub(super) fn render_settings_file(
        &mut self,
        appearance: &ChromeAppearance,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        self.follow_settings_file(cx);
        let location = SettingsFile::location(cx);
        let area = self.settings_file.area.clone();
        let focus = area.read(cx).focus_handle();
        let colors = appearance.host_colors(spaceterm_ui::ControlHost::Card);
        let frame = spaceterm_ui::field_frame(
            "settings-file-frame",
            &focus,
            FieldState::default(),
            RadiusRole::Control.pixels(),
            cx,
        )
        .debug_selector(|| "settings-file-frame".to_owned())
        .w_full()
        .min_w_0()
        .px(appearance.spacing(8.0))
        .py(appearance.spacing(6.0))
        .chrome_text(appearance.typography.style(TextRole::Secondary))
        .font_family(crate::bundled_font::FAMILY)
        .child(area);
        let owner = cx.weak_entity();
        let reload = action_button(
            "settings-file-reload",
            "Reload",
            !self.editor.is_writing(),
            move |_, cx| {
                let _ = owner.update(cx, |settings, cx| settings.editor.reload_file(cx));
            },
        );
        let owner = cx.weak_entity();
        let edit = action_button(
            "settings-file-edit",
            "Edit JSON",
            location.is_some(),
            move |_, cx| {
                let _ = owner.update(cx, |settings, cx| settings.edit_settings_file(cx));
            },
        );
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
                    .items_center()
                    .w_full()
                    .gap(appearance.spacing(12.0))
                    .child(
                        div()
                            .debug_selector(|| "settings-file-location".to_owned())
                            .flex_1()
                            .min_w_0()
                            .chrome_text(appearance.typography.style(TextRole::Secondary))
                            .text_color(gpui_color(colors.text_muted))
                            .truncate()
                            .children(location),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .flex_none()
                            .gap(appearance.spacing(6.0))
                            .child(reload)
                            .child(edit),
                    ),
            )
            .into_any_element()
    }

    /// Keeps the text equal to the document as the settings file stores it.
    fn follow_settings_file(&mut self, cx: &mut Context<Self>) {
        let document = self.editor.shared_document();
        if self
            .settings_file
            .shown
            .as_ref()
            .is_some_and(|shown| Arc::ptr_eq(shown, &document))
        {
            return;
        }
        // A document that cannot show must not leave an earlier document on show, which would
        // read as the settings in effect.
        let text = export_settings(&document).unwrap_or_default();
        self.settings_file.shown = Some(document);
        self.settings_file.area.update(cx, |area, cx| {
            if area.value() != text && !area.set_value(text, cx) {
                area.set_value(String::new(), cx);
            }
        });
    }

    /// Opens the settings file in the person's editor, writing it first if it does not exist.
    pub(super) fn edit_settings_file(&mut self, cx: &mut Context<Self>) {
        self.editor.write_file(cx);
        SettingsFile::open(cx);
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

fn present_import_failure(
    detail: &'static str,
    window: &mut Window,
    cx: &mut Context<SettingsWindow>,
) {
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
