//! The Color Schemes section: what is installed, where schemes come from, and what fell back.

use std::collections::BTreeSet;

use gpui::prelude::*;
use gpui::{AnyElement, App, SharedString, Window, div, px};
use spaceterm_ui::{
    Alert, AlertIntent, ModalAction, ModalActionEmphasis, ModalActionIntent, ModalActionRole,
    ModalId,
};

use crate::appearance::{AppearanceDiagnostic, SchemeId, SchemeKind, SchemeSummary, ZedImportKind};
use crate::settings::SchemeImport;
use crate::ui::appearance::ChromeAppearance;

use super::SettingsWindow;
use super::controls::{action_button, badge, gpui_color, swatch_strip};
use super::import::{ImportError as SchemeReadError, read_interchange_document};

/// The greatest number of scheme rows the section draws at once.
///
/// The document already bounds installed schemes at 128. Drawing every one of them in a fixed-size
/// window would be unreadable, so the section presents a bounded head and says how many remain.
const VISIBLE_SCHEMES: usize = 12;

/// A weak-owner handler, so a button outlives one render without borrowing the window.
fn owned(
    cx: &mut Context<SettingsWindow>,
    handler: impl Fn(&mut SettingsWindow, &mut Window, &mut Context<SettingsWindow>) + 'static,
) -> impl Fn(&mut Window, &mut App) + 'static {
    let owner = cx.weak_entity();
    move |window, cx| {
        let _ = owner.update(cx, |settings, cx| handler(settings, window, cx));
    }
}

/// The choice a removal confirmation returns.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RemovalChoice {
    Remove,
    Cancel,
}

impl SettingsWindow {
    pub(super) fn render_installed_schemes(
        &mut self,
        appearance: &ChromeAppearance,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let chrome = self.scheme_summaries(SchemeKind::Chrome, cx);
        let terminal = self.scheme_summaries(SchemeKind::Terminal, cx);
        let total = chrome.len() + terminal.len();
        let last = total.min(VISIBLE_SCHEMES).saturating_sub(1);
        let rows = chrome
            .into_iter()
            .chain(terminal)
            .take(VISIBLE_SCHEMES)
            .enumerate()
            .map(|(index, summary)| self.render_scheme_row(&summary, index < last, appearance, cx))
            .collect::<Vec<_>>();
        let remaining = total.saturating_sub(rows.len());
        // One bordered list reads as a single surface, so the rows line up instead of floating in
        // the section.
        div()
            .flex()
            .flex_col()
            .w_full()
            .rounded(px(7.0))
            .overflow_hidden()
            .border_1()
            .border_color(gpui_color(appearance.colors.border))
            .bg(gpui_color(appearance.colors.panel_background))
            .children(rows)
            .when(remaining > 0, |list| {
                list.child(
                    div()
                        .px(appearance.spacing(10.0))
                        .py(appearance.spacing(6.0))
                        .border_t_1()
                        .border_color(gpui_color(appearance.colors.border))
                        .text_size(appearance.text_size(11.0))
                        .text_color(gpui_color(appearance.colors.text_muted))
                        .child(SharedString::from(format!(
                            "{remaining} more installed, visible in your settings file"
                        ))),
                )
            })
            .into_any_element()
    }

    fn render_scheme_row(
        &self,
        summary: &SchemeSummary,
        separated: bool,
        appearance: &ChromeAppearance,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let selected = self.selected_scheme_ids(cx).contains(&summary.id);
        let removable = !summary.builtin && self.editor.editable();
        let id = summary.id.clone();
        let name = summary.name.clone();
        let row_selector = format!("settings-scheme-row-{}", summary.id.as_str());
        div()
            .debug_selector(move || row_selector.clone())
            .flex()
            .flex_row()
            .items_center()
            .w_full()
            .gap(appearance.spacing(8.0))
            .px(appearance.spacing(10.0))
            .h(appearance.height(32.0, 12.0))
            .when(separated, |row| {
                row.border_b_1()
                    .border_color(gpui_color(appearance.colors.border_variant))
            })
            .child(swatch_strip(
                format!("settings-scheme-swatches-{}", summary.id.as_str()),
                &summary.swatches,
                appearance,
            ))
            .child(
                // The name takes one line and ends in an ellipsis. Wrapping it would let a long
                // name grow the row and push the classifications out of their columns.
                div()
                    .min_w_0()
                    .flex_1()
                    .truncate()
                    .text_size(appearance.text_size(12.0))
                    .text_color(gpui_color(appearance.colors.text))
                    .child(SharedString::from(summary.name.clone())),
            )
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .flex_none()
                    .justify_end()
                    .gap(appearance.spacing(4.0))
                    .children(selected.then(|| badge("In use", appearance)))
                    .child(badge(
                        match summary.kind {
                            SchemeKind::Chrome => "Application",
                            SchemeKind::Terminal => "Terminal",
                        },
                        appearance,
                    ))
                    .child(badge(
                        match summary.appearance {
                            crate::appearance::Appearance::Light => "Light",
                            crate::appearance::Appearance::Dark => "Dark",
                        },
                        appearance,
                    )),
            )
            .child(
                div()
                    .w(appearance.text_size(72.0))
                    .flex_none()
                    .flex()
                    .flex_row()
                    .justify_end()
                    .when(!summary.builtin, |slot| {
                        slot.child(
                            spaceterm_ui::Button::new(
                                SharedString::from(format!(
                                    "settings-scheme-remove-{}",
                                    summary.id.as_str()
                                )),
                                "Remove",
                            )
                            .variant(spaceterm_ui::ButtonVariant::Ghost)
                            .size(spaceterm_ui::ButtonSize::Compact)
                            .disabled(!removable)
                            .tab_stop(true)
                            .debug_selector(format!(
                                "settings-scheme-remove-{}",
                                summary.id.as_str()
                            ))
                            .on_activate(cx.listener(
                                move |window, _, gpui_window, cx| {
                                    window.confirm_scheme_removal(
                                        id.clone(),
                                        name.clone(),
                                        gpui_window,
                                        cx,
                                    );
                                },
                            )),
                        )
                    })
                    .when(summary.builtin, |slot| {
                        slot.child(
                            div()
                                .text_size(appearance.text_size(10.0))
                                .text_color(gpui_color(appearance.colors.text_muted))
                                .child("Built-in"),
                        )
                    }),
            )
            .into_any_element()
    }

    pub(super) fn render_scheme_interchange(
        &mut self,
        appearance: &ChromeAppearance,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let editable = self.editor.editable();
        div()
            .flex()
            .flex_col()
            .w_full()
            .gap(appearance.spacing(6.0))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .flex_wrap()
                    .gap(appearance.spacing(8.0))
                    .child(action_button(
                        "settings-scheme-import",
                        "Import…",
                        editable,
                        owned(cx, |window, gpui_window, cx| {
                            window.begin_import(gpui_window, cx);
                        }),
                    ))
                    .child(action_button(
                        "settings-scheme-export",
                        "Export Schemes in Use…",
                        true,
                        owned(cx, |window, gpui_window, cx| {
                            window.begin_scheme_export(gpui_window, cx);
                        }),
                    ))
                    .child(action_button(
                        "settings-document-export",
                        "Export All Settings…",
                        true,
                        owned(cx, |window, gpui_window, cx| {
                            window.begin_document_export(gpui_window, cx);
                        }),
                    )),
            )
            .children(self.interchange_status.clone().map(|status| {
                div()
                    .debug_selector(|| "settings-interchange-status".to_owned())
                    .text_size(appearance.text_size(11.0))
                    .text_color(gpui_color(appearance.colors.text_secondary))
                    .whitespace_normal()
                    .child(status)
            }))
            .into_any_element()
    }

    pub(super) fn render_appearance_diagnostics(
        &mut self,
        appearance: &ChromeAppearance,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let diagnostics = crate::ui::appearance_runtime::current(cx)
            .diagnostics
            .clone();
        if diagnostics.is_empty() {
            return div()
                .debug_selector(|| "settings-diagnostics-empty".to_owned())
                .text_size(appearance.text_size(11.0))
                .text_color(gpui_color(appearance.colors.text_muted))
                .child("Everything you selected is available.")
                .into_any_element();
        }
        div()
            .debug_selector(|| "settings-diagnostics-list".to_owned())
            .flex()
            .flex_col()
            .w_full()
            .gap(appearance.spacing(3.0))
            .children(diagnostics.into_iter().map(|diagnostic| {
                div()
                    .flex()
                    .flex_row()
                    .items_start()
                    .gap(appearance.spacing(6.0))
                    .child(div().flex_none().mt(px(2.0)).child(spaceterm_ui::Icon::new(
                        spaceterm_ui::IconName::TriangleAlert,
                        appearance.text_size(11.0),
                        gpui_color(appearance.colors.warning),
                    )))
                    .child(
                        div()
                            .min_w_0()
                            .flex_1()
                            .text_size(appearance.text_size(11.0))
                            .text_color(gpui_color(appearance.colors.text_secondary))
                            .whitespace_normal()
                            .child(diagnostic_message(diagnostic)),
                    )
            }))
            .into_any_element()
    }

    fn scheme_summaries(&self, kind: SchemeKind, cx: &mut Context<Self>) -> Vec<SchemeSummary> {
        let _ = cx;
        self.editor.scheme_summaries(kind).unwrap_or_default()
    }

    /// The scheme identities currently selected by either domain, in any appearance slot.
    fn selected_scheme_ids(&self, cx: &mut Context<Self>) -> BTreeSet<SchemeId> {
        let _ = cx;
        let preferences = &self.editor.document().preferences;
        let mut selected = BTreeSet::new();
        for selection in [&preferences.chrome.scheme, &preferences.terminal.scheme] {
            match selection {
                crate::appearance::SchemeSelection::Fixed { id, .. } => {
                    selected.insert(id.clone());
                }
                crate::appearance::SchemeSelection::System { light, dark } => {
                    selected.insert(light.clone());
                    selected.insert(dark.clone());
                }
            }
        }
        selected
    }

    fn confirm_scheme_removal(
        &mut self,
        id: SchemeId,
        name: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let in_use = self.selected_scheme_ids(cx).contains(&id);
        let detail = if in_use {
            "It is in use, so SpaceTerm will fall back to a built-in scheme."
        } else {
            "This cannot be undone. You can import the scheme again from a file."
        };
        let owner = cx.weak_entity();
        let result = Alert::new(
            ModalId::new("settings-remove-scheme"),
            "Remove color scheme",
            "Remove Color Scheme",
            format!("Remove “{name}”? {detail}"),
            vec![
                ModalAction::new(
                    RemovalChoice::Remove,
                    "Remove",
                    ModalActionRole::Affirmative,
                    "settings-remove-scheme-confirm",
                )
                .with_intent(ModalActionIntent::Destructive)
                .with_emphasis(ModalActionEmphasis::Prominent),
                ModalAction::new(
                    RemovalChoice::Cancel,
                    "Cancel",
                    ModalActionRole::Cancel,
                    "settings-remove-scheme-cancel",
                ),
            ],
        )
        .intent(AlertIntent::Critical)
        .present(window, cx, move |outcome, cx| {
            let confirmed = matches!(
                outcome,
                spaceterm_ui::AlertOutcome::Activated {
                    action_id: RemovalChoice::Remove,
                    ..
                }
            );
            if !confirmed {
                return;
            }
            let _ = owner.update(cx, |window, cx| {
                window.interchange_status =
                    Some(match window.editor.remove_custom_scheme(&id, cx) {
                        Ok(()) => SharedString::from(format!("Removed “{name}”.")),
                        Err(_) => SharedString::from("That scheme could not be removed."),
                    });
                cx.notify();
            });
        });
        if result.is_err() {
            self.interchange_status = Some(SharedString::from(
                "SpaceTerm could not ask you to confirm removing that scheme.",
            ));
            cx.notify();
        }
    }

    fn begin_import(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let _ = window;
        let selection = cx.prompt_for_paths(gpui::PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Import".into()),
        });
        self.interchange_status = None;
        cx.notify();
        cx.spawn(async move |owner, cx| {
            let Ok(Ok(Some(paths))) = selection.await else {
                return;
            };
            let Some(path) = paths.into_iter().next() else {
                return;
            };
            let read = cx
                .background_executor()
                .spawn(async move { read_interchange_document(&path) })
                .await;
            let _ = owner.update(cx, |window, cx| window.finish_import(read, cx));
        })
        .detach();
    }

    fn finish_import(&mut self, read: Result<Vec<u8>, SchemeReadError>, cx: &mut Context<Self>) {
        let bytes = match read {
            Ok(bytes) => bytes,
            Err(error) => {
                self.interchange_status = Some(SharedString::from(error.message()));
                cx.notify();
                return;
            }
        };
        // A SpaceTerm color package is tried first; a Zed theme family is the other accepted shape.
        let native = self
            .editor
            .import(SchemeImport::SpaceTerm(&bytes), &BTreeSet::new(), cx);
        let status = match native {
            Ok(receipt) => Some(installed_message(receipt.installed.len())),
            Err(_) => match super::editor::SettingsEditor::list_import_candidates(&bytes) {
                Ok(candidates) if !candidates.is_empty() => {
                    // Every candidate in the family is installed under its own identity, so one
                    // import makes the whole family selectable without choosing for the user.
                    let mut installed = 0;
                    let mut failed = false;
                    for candidate in &candidates {
                        match self.editor.import(
                            SchemeImport::Zed {
                                bytes: &bytes,
                                candidate_index: candidate.index,
                                kinds: &[ZedImportKind::Chrome, ZedImportKind::Terminal],
                            },
                            &BTreeSet::new(),
                            cx,
                        ) {
                            Ok(receipt) => installed += receipt.installed.len(),
                            Err(_) => failed = true,
                        }
                    }
                    if installed == 0 {
                        Some(SharedString::from(
                            "Those schemes are already installed, or they collide with schemes you have.",
                        ))
                    } else if failed {
                        Some(SharedString::from(format!(
                            "Installed {installed} schemes. Some were skipped because they collide with schemes you have."
                        )))
                    } else {
                        Some(installed_message(installed))
                    }
                }
                _ => Some(SharedString::from(
                    "That file is not a SpaceTerm color package or a Zed theme.",
                )),
            },
        };
        self.interchange_status = status;
        cx.notify();
    }

    fn begin_scheme_export(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let _ = window;
        let resolved = crate::ui::appearance_runtime::current(cx);
        let schemes = [
            (SchemeKind::Chrome, resolved.chrome.effective_scheme.clone()),
            (
                SchemeKind::Terminal,
                resolved.terminal.effective_scheme.clone(),
            ),
        ];
        match self.editor.export_schemes(&schemes) {
            Ok(contents) => self.write_export("SpaceTerm-color-schemes.json", contents, cx),
            Err(_) => {
                self.interchange_status =
                    Some(SharedString::from("Those schemes could not be exported."));
                cx.notify();
            }
        }
    }

    fn begin_document_export(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let _ = window;
        match self.editor.export_document() {
            Ok(contents) => self.write_export("SpaceTerm-settings.json", contents, cx),
            Err(_) => {
                self.interchange_status =
                    Some(SharedString::from("Your settings could not be exported."));
                cx.notify();
            }
        }
    }

    fn write_export(&mut self, name: &'static str, contents: String, cx: &mut Context<Self>) {
        let directory = std::env::current_dir().unwrap_or_else(|_| std::env::temp_dir());
        let receiver = cx.prompt_for_new_path(&directory, Some(name));
        self.interchange_status = None;
        cx.notify();
        cx.spawn(async move |owner, cx| {
            let Ok(Ok(Some(path))) = receiver.await else {
                return;
            };
            let written = cx
                .background_executor()
                .spawn(async move { std::fs::write(&path, contents) })
                .await;
            let _ = owner.update(cx, |window, cx| {
                window.interchange_status = Some(match written {
                    Ok(()) => SharedString::from("Export written."),
                    Err(_) => SharedString::from("That file could not be written."),
                });
                cx.notify();
            });
        })
        .detach();
    }
}

fn installed_message(installed: usize) -> SharedString {
    if installed == 1 {
        SharedString::from("Installed 1 scheme. Nothing was selected for you.")
    } else {
        SharedString::from(format!(
            "Installed {installed} schemes. Nothing was selected for you."
        ))
    }
}

/// Content-free wording for one resolution diagnostic.
fn diagnostic_message(diagnostic: AppearanceDiagnostic) -> &'static str {
    match diagnostic {
        AppearanceDiagnostic::SystemAppearanceUnavailable => {
            "SpaceTerm cannot read the system light or dark setting, so Auto is using the dark slot."
        }
        AppearanceDiagnostic::ChromeSchemeUnavailable { .. } => {
            "The application color scheme you selected is not installed. A built-in scheme is in use."
        }
        AppearanceDiagnostic::TerminalSchemeUnavailable { .. } => {
            "The terminal color scheme you selected is not installed. A built-in scheme is in use."
        }
        AppearanceDiagnostic::ChromeFontUnavailable => {
            "The interface font you selected is not available. The system font is in use."
        }
        AppearanceDiagnostic::TerminalFontUnavailable => {
            "The terminal font you selected is not available. A monospace fallback is in use."
        }
        AppearanceDiagnostic::TerminalFontNotMonospace => {
            "The terminal font you selected is not monospaced. A monospace fallback is in use."
        }
    }
}
