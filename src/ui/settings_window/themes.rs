//! The Themes section's fallback notice, and settings export.

use gpui::prelude::*;
use gpui::{AnyElement, Window, div, px};
use spaceterm_ui::{Alert, AlertIntent, ModalAction, ModalActionRole, ModalId};

use crate::appearance::AppearanceDiagnostic;
use crate::ui::appearance::ChromeAppearance;
use crate::ui::chrome_geometry::{HAIRLINE, RadiusRole};
use crate::ui::chrome_icons::IconRole;
use crate::ui::chrome_typography::{ChromeTextStyleExt as _, TextRole};

use super::SettingsWindow;
use crate::ui::appearance::gpui_color;

impl SettingsWindow {
    /// The warning the Themes page carries when something selected could not be resolved.
    ///
    /// It is a notice at the top of the page rather than a labeled row: when nothing is wrong
    /// there is nothing to say, and a row whose value reads "everything is fine" is noise.
    pub(super) fn render_diagnostics_notice(
        &mut self,
        appearance: &ChromeAppearance,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let diagnostics = crate::ui::appearance_runtime::current(cx)
            .diagnostics
            .clone();
        if diagnostics.is_empty() {
            return None;
        }
        Some(
            div()
                .debug_selector(|| "settings-diagnostics-notice".to_owned())
                .flex()
                .flex_row()
                .items_start()
                .gap(appearance.spacing(8.0))
                .p(appearance.spacing(10.0))
                // The notice spans the column the cards under it span, so the page has one edge.
                .rounded(RadiusRole::Card.pixels())
                .bg(gpui_color(appearance.surface(
                    crate::appearance::SurfaceRole::Surface,
                    appearance.colors.warning_background,
                )))
                .border(px(HAIRLINE))
                .border_color(gpui_color(appearance.colors.warning_border))
                // The same glyph the window's own banner carries, at the same size: they are the
                // same kind of warning, one about the page and one about the window.
                .child(div().flex_none().mt(px(1.0)).child(spaceterm_ui::Icon::new(
                    spaceterm_ui::IconName::TriangleAlert,
                    appearance.icons.metrics(IconRole::Status).glyph_size,
                    gpui_color(appearance.colors.warning),
                )))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .min_w_0()
                        .flex_1()
                        .gap(appearance.spacing(3.0))
                        .children(diagnostics.into_iter().map(|diagnostic| {
                            div()
                                .chrome_text(appearance.typography.style(TextRole::Secondary))
                                .text_color(gpui_color(appearance.colors.warning))
                                .whitespace_normal()
                                .child(diagnostic_message(diagnostic))
                        })),
                )
                .into_any_element(),
        )
    }

    /// Writes the Settings Document where the person chooses. Closing the save panel is the
    /// confirmation, so only a failure speaks.
    pub(super) fn begin_document_export(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Ok(contents) = self.editor.export_document() else {
            present_export_failure("Your settings could not be exported.", window, cx);
            return;
        };
        let directory = std::env::current_dir().unwrap_or_else(|_| std::env::temp_dir());
        let receiver = cx.prompt_for_new_path(&directory, Some("SpaceTerm-settings.json"));
        cx.spawn_in(window, async move |owner, cx| {
            let Ok(Ok(Some(path))) = receiver.await else {
                return;
            };
            let written = cx
                .background_executor()
                .spawn(async move { std::fs::write(&path, contents) })
                .await;
            if written.is_err() {
                let _ = owner.update_in(cx, |_, window, cx| {
                    present_export_failure("That file could not be written.", window, cx);
                });
            }
        })
        .detach();
    }
}

fn present_export_failure(
    detail: &'static str,
    window: &mut Window,
    cx: &mut Context<SettingsWindow>,
) {
    let _ = Alert::new(
        ModalId::new("settings-export-failed"),
        "Export failed",
        "Export Failed",
        detail,
        vec![ModalAction::new(
            (),
            "OK",
            ModalActionRole::Cancel,
            "settings-export-failed-ok",
        )],
    )
    .intent(AlertIntent::Warning)
    .present(window, cx, |_, _| {});
}

/// Content-free wording for one resolution diagnostic.
fn diagnostic_message(diagnostic: AppearanceDiagnostic) -> &'static str {
    match diagnostic {
        AppearanceDiagnostic::SystemAppearanceUnavailable => {
            "SpaceTerm cannot read the system light or dark setting, so Auto is using the dark slot."
        }
        AppearanceDiagnostic::TerminalThemeUnavailable { .. } => {
            "The terminal theme you selected is not installed. A built-in theme is in use."
        }
        AppearanceDiagnostic::TerminalFontUnavailable => {
            "The terminal font you selected is not available. A monospace fallback is in use."
        }
        AppearanceDiagnostic::TerminalFontNotMonospace => {
            "The terminal font you selected is not monospaced. A monospace fallback is in use."
        }
    }
}
