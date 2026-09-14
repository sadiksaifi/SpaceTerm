//! The Color Schemes section: what is installed, where schemes come from, and what fell back.

use std::collections::BTreeSet;

use gpui::prelude::*;
use gpui::{AnyElement, App, SharedString, Window, div, px};
use spaceterm_ui::{
    Alert, AlertIntent, ModalAction, ModalActionEmphasis, ModalActionIntent, ModalActionRole,
    ModalId,
};

use crate::appearance::{
    AppearanceDiagnostic, CatalogError, ImportError, SchemeId, SchemeKind, SchemeSummary,
    ZedImportKind,
};
use crate::settings::{ImportReceipt, SchemeImport, SettingsError};
use crate::ui::appearance::ChromeAppearance;

use super::SettingsWindow;
use super::controls::{CARD_RADIUS, action_button, badge, gpui_color, swatch_strip, text};
use super::import::{ImportError as SchemeReadError, read_interchange_document};

/// The column a scheme's removal takes at the end of its line in a list.
const TRAILING_WIDTH: f32 = 28.0;

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
    /// One surface's installed schemes.
    ///
    /// The group's own title names the surface, so the list adds no heading of its own: it is a run
    /// of rows on the group's card, like every other row on every other page.
    pub(super) fn render_installed_schemes(
        &mut self,
        kind: SchemeKind,
        appearance: &ChromeAppearance,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let summaries = self.scheme_summaries(kind, cx);
        let rows = summaries
            .iter()
            .map(|summary| self.render_scheme_row(summary, appearance, cx))
            .collect::<Vec<_>>();
        let selector = match kind {
            SchemeKind::Chrome => "settings-installed-schemes-chrome",
            SchemeKind::Terminal => "settings-installed-schemes-terminal",
        };
        div()
            .debug_selector(move || selector.to_owned())
            .flex()
            .flex_col()
            .w_full()
            .children(rows)
            .into_any_element()
    }

    /// The warning the library carries when something selected could not be resolved.
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
                .rounded(appearance.spacing(CARD_RADIUS))
                .bg(gpui_color(appearance.colors.warning_background))
                .border_1()
                .border_color(gpui_color(appearance.colors.warning_border))
                // The same glyph the window's own banner carries, at the same size: they are the
                // same kind of warning, one about the page and one about the window.
                .child(div().flex_none().mt(px(1.0)).child(spaceterm_ui::Icon::new(
                    spaceterm_ui::IconName::TriangleAlert,
                    appearance.text_size(13.0),
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
                                .text_size(appearance.text_size(text::SMALL))
                                .text_color(gpui_color(appearance.colors.warning))
                                .whitespace_normal()
                                .child(diagnostic_message(diagnostic))
                        })),
                )
                .into_any_element(),
        )
    }

    /// One installed scheme: its colors, its name, what it is, and what can be done with it.
    ///
    /// The classifications read as one dim line rather than a row of outlined pills, so only the
    /// status worth noticing, the scheme actually in use, carries a fill.
    fn render_scheme_row(
        &self,
        summary: &SchemeSummary,
        appearance: &ChromeAppearance,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let selected = self.selected_scheme_ids(cx).contains(&summary.id);
        let removable = !summary.builtin && self.editor.editable();
        let id = summary.id.clone();
        let name = summary.name.clone();
        let row_selector = format!("settings-scheme-row-{}", summary.id.as_str());
        let classification = format!(
            "{} · {}",
            match summary.appearance {
                crate::appearance::Appearance::Light => "Light",
                crate::appearance::Appearance::Dark => "Dark",
            },
            if summary.builtin {
                "Built-in"
            } else {
                "Custom"
            }
        );
        div()
            .debug_selector(move || row_selector.clone())
            .flex()
            .flex_row()
            .items_center()
            .w_full()
            .gap(appearance.spacing(10.0))
            .h(appearance.height(34.0, 12.0))
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
                    .text_size(appearance.text_size(text::BODY))
                    .text_color(gpui_color(appearance.colors.text))
                    .child(SharedString::from(summary.name.clone())),
            )
            .children(selected.then(|| badge("In use", appearance)))
            .child(
                div()
                    .flex_none()
                    .text_size(appearance.text_size(text::SMALL))
                    .text_color(gpui_color(appearance.colors.text_muted))
                    .child(SharedString::from(classification)),
            )
            .child(
                // Every scheme in a list ends with this column, so the classifications stay in
                // one column whether a scheme can be removed or not.
                div()
                    .w(appearance.text_size(TRAILING_WIDTH))
                    .flex_none()
                    .flex()
                    .flex_row()
                    .justify_end()
                    .when(!summary.builtin, |slot| {
                        slot.child(
                            spaceterm_ui::IconButton::new(
                                SharedString::from(format!(
                                    "settings-scheme-remove-{}",
                                    summary.id.as_str()
                                )),
                                SharedString::from(format!("Remove {}", summary.name)),
                                |foreground| {
                                    spaceterm_ui::Icon::new(
                                        spaceterm_ui::IconName::Trash2,
                                        px(12.0),
                                        foreground,
                                    )
                                    .into_any_element()
                                },
                            )
                            .variant(spaceterm_ui::ButtonVariant::Ghost)
                            .size(spaceterm_ui::ButtonSize::Small)
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
                        "Export Effective Schemes…",
                        true,
                        owned(cx, |window, gpui_window, cx| {
                            window.begin_scheme_export(gpui_window, cx);
                        }),
                    ))
                    .child(action_button(
                        "settings-definition-export",
                        "Export Scheme Definitions…",
                        true,
                        owned(cx, |window, gpui_window, cx| {
                            window.begin_definition_export(gpui_window, cx);
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
                    .text_size(appearance.text_size(text::SMALL))
                    .text_color(gpui_color(appearance.colors.text_secondary))
                    .whitespace_normal()
                    .child(status)
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
        for slots in [&preferences.chrome.schemes, &preferences.terminal.schemes] {
            selected.insert(slots.light.clone());
            selected.insert(slots.dark.clone());
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
        let Some(opener) = cx
            .try_global::<crate::app::SelectedFileAccess>()
            .map(|access| std::sync::Arc::clone(&access.0))
        else {
            self.interchange_status = Some("File import is unavailable.".into());
            cx.notify();
            return;
        };
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
                .spawn(async move { read_interchange_document(&path, opener.as_ref()) })
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
        self.interchange_status = Some(import_document(&bytes, |source| {
            self.editor.import(source, &BTreeSet::new(), cx)
        }));
        cx.notify();
    }

    fn begin_scheme_export(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let _ = window;
        let resolved = crate::ui::appearance_runtime::current(cx);
        match self.editor.export_appearance(&resolved) {
            Ok(contents) => self.write_export("SpaceTerm-color-schemes.json", contents, cx),
            Err(_) => {
                self.interchange_status =
                    Some(SharedString::from("Those schemes could not be exported."));
                cx.notify();
            }
        }
    }

    fn begin_definition_export(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let _ = window;
        let resolved = crate::ui::appearance_runtime::current(cx);
        let schemes = [
            (SchemeKind::Chrome, resolved.chrome.effective_scheme.clone()),
            (
                SchemeKind::Terminal,
                resolved.terminal.effective_scheme.clone(),
            ),
        ];
        match self.editor.export_definitions(&schemes) {
            Ok(contents) => self.write_export("SpaceTerm-scheme-definitions.json", contents, cx),
            Err(_) => {
                self.interchange_status = Some(SharedString::from(
                    "Those definitions could not be exported.",
                ));
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

fn import_document<'a>(
    bytes: &'a [u8],
    mut install: impl FnMut(SchemeImport<'a>) -> Result<ImportReceipt, SettingsError>,
) -> SharedString {
    match install(SchemeImport::SpaceTerm(bytes)) {
        Ok(receipt) => return installed_message(receipt.installed.len()),
        // A Zed theme family fails to deserialize as a SpaceTerm package. Installation failures
        // say nothing about its format and must retain their own classification.
        Err(SettingsError::Import(ImportError::InvalidJson)) => {}
        Err(error) => return import_failure_message(error).into(),
    }
    let candidates = match super::editor::SettingsEditor::list_import_candidates(bytes) {
        Ok(candidates) if !candidates.is_empty() => candidates,
        _ => return "That file is not a SpaceTerm color package or a Zed theme.".into(),
    };
    // Each candidate has its own identity. Import the family without selecting a scheme.
    let mut installed = 0;
    let mut failure = None;
    for candidate in candidates {
        match install(SchemeImport::Zed {
            bytes,
            candidate_index: candidate.index,
            kinds: &[ZedImportKind::Chrome, ZedImportKind::Terminal],
        }) {
            Ok(receipt) => installed += receipt.installed.len(),
            Err(error) => {
                failure.get_or_insert(error);
            }
        }
    }
    match failure {
        Some(error) if installed == 0 => import_failure_message(error).into(),
        Some(error) => format!(
            "Installed {installed} schemes. Some could not be imported. {}",
            import_failure_message(error)
        )
        .into(),
        None => installed_message(installed),
    }
}

fn import_failure_message(error: SettingsError) -> &'static str {
    match error {
        SettingsError::Catalog(CatalogError::DuplicateId) => {
            "Those schemes are already installed, or they collide with schemes you have."
        }
        SettingsError::Catalog(CatalogError::TooManySchemes) => {
            "There is no room for those schemes. Remove an installed custom scheme and try again."
        }
        SettingsError::Busy => "Settings are busy. Try importing again when saving finishes.",
        SettingsError::Stale | SettingsError::Catalog(CatalogError::RevisionConflict) => {
            "Settings changed before the import finished. Try importing again."
        }
        SettingsError::Import(ImportError::UnsupportedVersion) => {
            "That color package uses an unsupported version."
        }
        SettingsError::Import(_) => "That color package contains invalid schemes.",
        _ => "Those schemes could not be imported.",
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::secure_filesystem::{PrivateFileSnapshot, SecureEntryIdentity};
    use crate::settings::UserSettings;
    use crate::settings::storage::{SettingsStorage, StorageCommit, StorageError};

    struct EmptyStorage;

    impl SettingsStorage for EmptyStorage {
        fn read(&self) -> Result<Option<PrivateFileSnapshot>, StorageError> {
            Ok(None)
        }

        fn write(
            &self,
            _: &[u8],
            _: Option<&SecureEntryIdentity>,
        ) -> Result<StorageCommit, StorageError> {
            panic!("importing a preview must not write settings");
        }
    }

    const PACKAGE: &[u8] = br##"{"schema_version":1,"schemes":[{"kind":"chrome","id":"custom.sample","name":"Sample","appearance":"light","colors":{"text":"#112233"}}]}"##;

    #[gpui::test]
    fn schemes_beyond_the_twelfth_row_can_be_scrolled_to_and_removed(
        cx: &mut gpui::TestAppContext,
    ) {
        use crate::appearance::{Appearance, AppearanceDocument};
        use crate::platform::appearance::testing::RecordingAppearancePlatform;
        use crate::ui::appearance_runtime;
        use crate::ui::settings_window::test_support::MemoryStorage;

        let schemes = ["chrome", "terminal"]
            .into_iter()
            .flat_map(|kind| {
                (0..14).map(move |index| {
                    serde_json::json!({
                        "kind": kind,
                        "id": format!("custom.{kind}.{index:02}"),
                        "name": format!("Sample {index}"),
                        "appearance": "dark",
                        "colors": {},
                    })
                })
            })
            .collect::<Vec<_>>();
        let document = AppearanceDocument {
            custom_schemes: serde_json::from_value(serde_json::json!(schemes)).unwrap(),
            ..AppearanceDocument::default()
        };
        let storage = MemoryStorage::with_document(&document);
        let (settings, changed) = UserSettings::load(storage);
        let platform = RecordingAppearancePlatform::default();
        platform.set_system_appearance(Some(Appearance::Dark));
        cx.update(|cx| {
            appearance_runtime::install(settings, changed, std::rc::Rc::new(platform), cx)
                .expect("appearance runtime");
            crate::ui::init(cx).expect("UI initialization");
        });
        let (settings_window, cx) = cx.add_window_view(SettingsWindow::new);
        cx.update(|window, cx| {
            window.activate_window();
            settings_window.update(cx, |settings, cx| {
                settings.reveal_section(super::super::SettingsSectionId::ColorSchemes, cx);
            });
        });
        cx.run_until_parked();

        for (id, selector) in [
            (
                "custom.chrome.13",
                "settings-scheme-remove-custom.chrome.13",
            ),
            (
                "custom.terminal.13",
                "settings-scheme-remove-custom.terminal.13",
            ),
        ] {
            let target = cx
                .debug_bounds(selector)
                .expect("late scheme removal action");
            cx.update(|_, cx| {
                settings_window.update(cx, |settings, cx| {
                    let viewport = settings.scroll.bounds();
                    let offset =
                        settings.scroll.offset().y + viewport.center().y - target.center().y;
                    settings.scroll.set_offset(gpui::point(px(0.0), offset));
                    cx.notify();
                });
            });
            cx.run_until_parked();
            let position = cx.debug_bounds(selector).unwrap().center();
            assert!(settings_window.read_with(cx, |settings, _| {
                settings.scroll.bounds().contains(&position)
            }));
            cx.simulate_mouse_move(position, None, gpui::Modifiers::none());
            cx.simulate_click(position, gpui::Modifiers::none());
            cx.run_until_parked();
            let confirmation = cx
                .debug_bounds("modal-action-settings-remove-scheme-confirm")
                .expect("removal confirmation")
                .center();
            cx.simulate_mouse_move(confirmation, None, gpui::Modifiers::none());
            cx.simulate_click(confirmation, gpui::Modifiers::none());
            cx.run_until_parked();

            assert!(settings_window.read_with(cx, |settings, _| {
                settings
                    .editor
                    .document()
                    .custom_schemes
                    .iter()
                    .all(|scheme| scheme.id().as_str() != id)
            }));
        }
    }

    #[test]
    fn importing_an_installed_native_package_reports_a_collision() {
        let (settings, _) = UserSettings::load(std::sync::Arc::new(EmptyStorage));
        let token = settings.begin_preview(0).unwrap();
        let mut install = |source| {
            settings.import_preview(
                &token,
                settings.snapshot().catalog_revision,
                source,
                &BTreeSet::new(),
            )
        };
        import_document(PACKAGE, &mut install);

        assert_eq!(
            import_document(PACKAGE, &mut install).as_ref(),
            "Those schemes are already installed, or they collide with schemes you have."
        );
        assert_eq!(settings.snapshot().candidate.custom_schemes.len(), 1);
    }

    #[test]
    fn a_zed_family_is_imported_when_native_deserialization_fails() {
        let (settings, _) = UserSettings::load(std::sync::Arc::new(EmptyStorage));
        let token = settings.begin_preview(0).unwrap();
        let bytes = br##"{"themes":[{"name":"Sample","appearance":"dark","style":{"terminal.foreground":"#abcdef"}}]}"##;

        let message = import_document(bytes, |source| {
            settings.import_preview(
                &token,
                settings.snapshot().catalog_revision,
                source,
                &BTreeSet::new(),
            )
        });

        assert_eq!(message, installed_message(2));
        assert_eq!(settings.snapshot().candidate.custom_schemes.len(), 2);
    }

    #[test]
    fn native_import_failures_do_not_attempt_zed_installation() {
        for (error, expected) in [
            (
                SettingsError::Busy,
                "Settings are busy. Try importing again when saving finishes.",
            ),
            (
                SettingsError::Catalog(CatalogError::TooManySchemes),
                "There is no room for those schemes. Remove an installed custom scheme and try again.",
            ),
            (
                SettingsError::Import(ImportError::UnsupportedVersion),
                "That color package uses an unsupported version.",
            ),
        ] {
            let mut attempts = 0;
            let message = import_document(PACKAGE, |source| {
                assert!(matches!(source, SchemeImport::SpaceTerm(_)));
                attempts += 1;
                Err(error)
            });
            assert_eq!(attempts, 1);
            assert_eq!(message.as_ref(), expected);
        }
    }
}
