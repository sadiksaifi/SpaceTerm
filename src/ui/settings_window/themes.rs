//! The Themes section: the installed Terminal Theme library, what fell back, and file import.

use gpui::prelude::*;
use gpui::{AnyElement, App, Entity, SharedString, Window, div, px};
use spaceterm_ui::{
    Alert, AlertIntent, ModalAction, ModalActionEmphasis, ModalActionIntent, ModalActionRole,
    ModalId, SearchField, TextInput, TextInputEscapeBehavior, TextInputEvent,
    TextInputReturnBehavior, TextInputVariant, fuzzy_filter,
};

use crate::appearance::{
    Appearance, AppearanceDiagnostic, AppearanceMode, CatalogError, ImportError, ThemeId,
    ThemeSummary,
};
use crate::settings::{SettingsError, ThemeImport};
use crate::ui::appearance::ChromeAppearance;
use crate::ui::chrome_geometry::{HAIRLINE, RadiusRole};
use crate::ui::chrome_icons::IconRole;
use crate::ui::chrome_typography::{ChromeTextStyleExt as _, TextRole};

use super::SettingsWindow;
use super::controls::{action_button, badge, swatch_strip};
use crate::ui::appearance::gpui_color;
use super::import::{ImportError as ThemeReadError, read_theme_document};

/// The column a theme's removal takes at the end of its line in a list.
const TRAILING_WIDTH: f32 = 28.0;
/// A library shorter than this is scanned by eye, so it offers no search field.
const SEARCHABLE_LIBRARY: usize = 8;

pub(super) const INSTALLED_SEARCH_SELECTOR: &str = "settings-installed-themes-search";

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

/// A search field input for one list in the Themes section.
pub(super) fn new_list_search(
    selector: &'static str,
    accessibility_name: &'static str,
    window: &mut Window,
    cx: &mut Context<SettingsWindow>,
) -> Entity<TextInput> {
    cx.new(|cx| {
        TextInput::new(selector, accessibility_name, String::new(), window, cx)
            .placeholder("Search")
            .variant(TextInputVariant::Bare)
            .return_behavior(TextInputReturnBehavior::Propagate)
            .escape_behavior(TextInputEscapeBehavior::Propagate)
            .input_length_limit(Some(128))
            .emit_programmatic_changes(true)
            .debug_selector(selector)
    })
}

/// The choice a removal confirmation returns.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RemovalChoice {
    Remove,
    Cancel,
}

/// Owns the installed library's search.
pub(super) struct InstalledThemesSearch {
    input: Entity<TextInput>,
    query: SharedString,
}

impl InstalledThemesSearch {
    pub(super) fn new(window: &mut Window, cx: &mut Context<SettingsWindow>) -> Self {
        let input = new_list_search(
            INSTALLED_SEARCH_SELECTOR,
            "Search installed themes",
            window,
            cx,
        );
        cx.subscribe(&input, |settings, input, event: &TextInputEvent, cx| {
            if matches!(event, TextInputEvent::ValueChanged(_)) {
                settings.installed_search.query =
                    SharedString::from(input.read(cx).value().to_owned());
                cx.notify();
            }
        })
        .detach();
        Self {
            input,
            query: SharedString::default(),
        }
    }
}

impl SettingsWindow {
    /// Installed Terminal Themes, searchable once the library is long enough to need it.
    pub(super) fn render_installed_themes(
        &mut self,
        appearance: &ChromeAppearance,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let summaries = self.theme_summaries();
        let searchable = summaries.len() > SEARCHABLE_LIBRARY;
        let query = if searchable {
            self.installed_search.query.clone()
        } else {
            SharedString::default()
        };
        let matches = fuzzy_filter(&summaries, &query, |summary| {
            let mut target = spaceterm_ui::FuzzyTarget::new(&summary.name);
            if let Some(family) = &summary.family {
                target = target.field(family);
            }
            target.field(appearance_name(summary.appearance))
        });
        let rows = matches
            .iter()
            .map(|matched| self.render_theme_row(&summaries[matched.item_index()], appearance, cx))
            .collect::<Vec<_>>();
        let colors = appearance.host_colors(spaceterm_ui::ControlHost::Card);
        let selector = "settings-installed-themes";
        div()
            .debug_selector(move || selector.to_owned())
            .flex()
            .flex_col()
            .w_full()
            .gap(appearance.spacing(6.0))
            .when(searchable, |list| {
                list.child(
                    SearchField::new(
                        "settings-installed-themes-search-frame",
                        self.installed_search.input.clone(),
                    )
                    .debug_selectors(
                        "settings-installed-themes-search-frame",
                        "settings-installed-themes-search-clear",
                    ),
                )
            })
            .when(rows.is_empty(), |list| {
                list.child(
                    div()
                        .chrome_text(appearance.typography.style(TextRole::Secondary))
                        .text_color(gpui_color(colors.text_secondary))
                        .child(SharedString::from(format!(
                            "No installed themes match “{query}”."
                        ))),
                )
            })
            .child(div().flex().flex_col().w_full().children(rows))
            .children(self.interchange_status.clone().map(|status| {
                div()
                    .debug_selector(|| "settings-interchange-status".to_owned())
                    .chrome_text(appearance.typography.style(TextRole::Secondary))
                    .text_color(gpui_color(colors.text_secondary))
                    .whitespace_normal()
                    .child(status)
            }))
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

    /// One installed theme: its colors, its name, where it came from, and what can be done.
    ///
    /// A theme in use says so where its Use action would otherwise be, so every row keeps its
    /// columns whether it is in use or not.
    fn render_theme_row(
        &self,
        summary: &ThemeSummary,
        appearance: &ChromeAppearance,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let slots = &self.editor.document().preferences.terminal.themes;
        let selected = *slots.get(summary.appearance) == summary.id;
        let editable = self.editor.editable();
        let removable = !summary.builtin && editable;
        let colors = appearance.host_colors(spaceterm_ui::ControlHost::Card);
        let action_icon_size = appearance.icons.metrics(IconRole::Control).glyph_size;
        let id = summary.id.clone();
        let name = summary.name.clone();
        let row_selector = format!("settings-theme-row-{}", summary.id.as_str());
        let classification = format!(
            "{} · {}",
            appearance_name(summary.appearance),
            match (&summary.family, summary.builtin) {
                (_, true) => "Built-in",
                (Some(family), false) => family.as_str(),
                (None, false) => "Imported",
            }
        );
        let use_label = self.use_label(summary.appearance);
        let use_selector = format!("settings-theme-use-{}", summary.id.as_str());
        let slot = summary.appearance;
        let chosen = summary.id.clone();
        div()
            .debug_selector(move || row_selector.clone())
            .flex()
            .flex_row()
            .items_center()
            .w_full()
            .gap(appearance.spacing(10.0))
            .h(appearance.typography.style(TextRole::Body).line_height
                + appearance.typography.style(TextRole::Secondary).line_height
                + appearance.spacing(14.0))
            .child(swatch_strip(
                format!("settings-theme-swatches-{}", summary.id.as_str()),
                &summary.swatches,
                appearance,
            ))
            .child(
                // Each line takes one line and ends in an ellipsis, so a long name cannot grow
                // the row or push the actions out of their column.
                div()
                    .flex()
                    .flex_col()
                    .min_w_0()
                    .flex_1()
                    .child(
                        div()
                            .truncate()
                            .chrome_text(appearance.typography.style(TextRole::Body))
                            .text_color(gpui_color(colors.text))
                            .child(SharedString::from(summary.name.clone())),
                    )
                    .child(
                        div()
                            .truncate()
                            .chrome_text(appearance.typography.style(TextRole::Secondary))
                            .text_color(gpui_color(colors.text_muted))
                            .child(SharedString::from(classification)),
                    ),
            )
            .child(
                div()
                    .debug_selector({
                        let id = summary.id.clone();
                        move || format!("settings-theme-status-slot-{}", id.as_str())
                    })
                    .flex_none()
                    .map(|slot_element| {
                        if selected {
                            slot_element.child(badge("In use", appearance))
                        } else {
                            slot_element.child(
                                spaceterm_ui::Button::new(
                                    SharedString::from(use_selector.clone()),
                                    use_label,
                                )
                                .variant(spaceterm_ui::ButtonVariant::Outline)
                                .size(spaceterm_ui::ButtonSize::Small)
                                .disabled(!editable)
                                .tab_stop(true)
                                .debug_selector(use_selector.clone())
                                .on_activate(cx.listener(move |settings, _, _, cx| {
                                    settings.set_theme(slot, chosen.clone(), cx);
                                })),
                            )
                        }
                    }),
            )
            .child(
                // Every theme in a list ends with this column, so the actions stay in one column
                // whether a theme can be removed or not.
                div()
                    .w(appearance.spacing(TRAILING_WIDTH))
                    .flex_none()
                    .flex()
                    .flex_row()
                    .justify_end()
                    .when(!summary.builtin, |slot| {
                        slot.child(
                            spaceterm_ui::IconButton::new(
                                SharedString::from(format!(
                                    "settings-theme-remove-{}",
                                    summary.id.as_str()
                                )),
                                SharedString::from(format!("Remove {}", summary.name)),
                                move |foreground| {
                                    spaceterm_ui::Icon::new(
                                        spaceterm_ui::IconName::Trash2,
                                        action_icon_size,
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
                                "settings-theme-remove-{}",
                                summary.id.as_str()
                            ))
                            .on_activate(cx.listener(
                                move |window, _, gpui_window, cx| {
                                    window.confirm_theme_removal(
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

    /// "Use" selects a theme for the appearance on screen. A theme for the other appearance names
    /// its slot, because selecting it changes nothing visible until that appearance is in use.
    fn use_label(&self, theme: Appearance) -> &'static str {
        match self.editor.document().preferences.mode {
            AppearanceMode::Auto => "Use",
            _ if self.fixed_appearance() == theme => "Use",
            _ => match theme {
                Appearance::Light => "Use for Light",
                Appearance::Dark => "Use for Dark",
            },
        }
    }

    pub(super) fn render_theme_import(
        &mut self,
        appearance: &ChromeAppearance,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let _ = appearance;
        action_button(
            "settings-theme-import",
            "Import…",
            self.editor.editable(),
            owned(cx, |window, gpui_window, cx| {
                window.begin_import(gpui_window, cx);
            }),
        )
        .into_any_element()
    }

    pub(super) fn theme_summaries(&self) -> Vec<ThemeSummary> {
        self.editor.theme_summaries().unwrap_or_default()
    }

    fn confirm_theme_removal(
        &mut self,
        id: ThemeId,
        name: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let slots = &self.editor.document().preferences.terminal.themes;
        let in_use = slots.light == id || slots.dark == id;
        let detail = if in_use {
            "It is in use, so SpaceTerm will fall back to a built-in theme."
        } else {
            "You can install it again from its Zed extension or file."
        };
        let owner = cx.weak_entity();
        let result = Alert::new(
            ModalId::new("settings-remove-theme"),
            "Remove terminal theme",
            "Remove Terminal Theme",
            format!("Remove “{name}”? {detail}"),
            vec![
                ModalAction::new(
                    RemovalChoice::Remove,
                    "Remove",
                    ModalActionRole::Affirmative,
                    "settings-remove-theme-confirm",
                )
                .with_intent(ModalActionIntent::Destructive)
                .with_emphasis(ModalActionEmphasis::Prominent),
                ModalAction::new(
                    RemovalChoice::Cancel,
                    "Cancel",
                    ModalActionRole::Cancel,
                    "settings-remove-theme-cancel",
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
                    Some(match window.editor.remove_theme(&id, cx) {
                        Ok(()) => SharedString::from(format!("Removed “{name}”.")),
                        Err(_) => SharedString::from("That theme could not be removed."),
                    });
                cx.notify();
            });
        });
        if result.is_err() {
            self.interchange_status = Some(SharedString::from(
                "SpaceTerm could not ask you to confirm removing that theme.",
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
                .spawn(async move { read_theme_document(&path, opener.as_ref()) })
                .await;
            let _ = owner.update(cx, |window, cx| window.finish_import(read, cx));
        })
        .detach();
    }

    fn finish_import(&mut self, read: Result<Vec<u8>, ThemeReadError>, cx: &mut Context<Self>) {
        self.interchange_status = Some(match read {
            Ok(bytes) => import_family(&bytes, |source| self.editor.import(source, cx)),
            Err(error) => SharedString::from(error.message()),
        });
        cx.notify();
    }

    pub(super) fn begin_document_export(&mut self, window: &mut Window, cx: &mut Context<Self>) {
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

fn appearance_name(appearance: Appearance) -> &'static str {
    match appearance {
        Appearance::Light => "Light",
        Appearance::Dark => "Dark",
    }
}

fn import_family<'a>(
    bytes: &'a [u8],
    install: impl FnOnce(ThemeImport<'a>) -> Result<crate::settings::ImportReceipt, SettingsError>,
) -> SharedString {
    match install(ThemeImport::ZedFamily(bytes)) {
        Ok(receipt) => installed_message(receipt.installed.len()),
        Err(error) => import_failure_message(error).into(),
    }
}

pub(super) fn import_failure_message(error: SettingsError) -> &'static str {
    match error {
        SettingsError::Catalog(CatalogError::TooManyThemes) => {
            "There is no room for those themes. Remove some installed themes and try again."
        }
        SettingsError::Busy => "Settings are busy. Try again when saving finishes.",
        SettingsError::Stale | SettingsError::Catalog(CatalogError::RevisionConflict) => {
            "Settings changed before the install finished. Try again."
        }
        SettingsError::Import(ImportError::TooLarge) => "That theme file is too large.",
        SettingsError::Import(ImportError::InvalidThemeCount) => {
            "That source contains no themes SpaceTerm can use, or too many."
        }
        SettingsError::Import(_) => "That is not a Zed theme file SpaceTerm can read.",
        _ => "Those themes could not be installed.",
    }
}

fn installed_message(installed: usize) -> SharedString {
    if installed == 1 {
        SharedString::from("Installed 1 theme.")
    } else {
        SharedString::from(format!("Installed {installed} themes."))
    }
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

    #[gpui::test]
    fn themes_beyond_the_twelfth_row_can_be_scrolled_to_and_removed(
        cx: &mut gpui::TestAppContext,
    ) {
        use crate::appearance::{Appearance, SettingsDocument};
        use crate::platform::appearance::testing::RecordingAppearancePlatform;
        use crate::ui::appearance_runtime;
        use crate::ui::settings_window::test_support::MemoryStorage;

        let themes = (0..14)
            .map(|index| {
                serde_json::json!({
                    "id": format!("custom.terminal.{index:02}"), "name": format!("Sample {index}"),
                    "appearance": "dark", "colors": {},
                })
            })
            .collect::<Vec<_>>();
        let document = SettingsDocument {
            terminal_themes: serde_json::from_value(serde_json::json!(themes)).unwrap(),
            ..SettingsDocument::default()
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
                settings.reveal_section(super::super::SettingsSectionId::Themes, cx);
            });
        });
        cx.run_until_parked();

        let id = "custom.terminal.13";
        let selector = "settings-theme-remove-custom.terminal.13";

        let target = cx
            .debug_bounds(selector)
            .expect("late theme removal action");
        cx.update(|_, cx| {
            settings_window.update(cx, |settings, cx| {
                let viewport = settings.scroll.bounds();
                let offset = settings.scroll.offset().y + viewport.center().y - target.center().y;
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
            .debug_bounds("modal-action-settings-remove-theme-confirm")
            .expect("removal confirmation")
            .center();
        cx.simulate_mouse_move(confirmation, None, gpui::Modifiers::none());
        cx.simulate_click(confirmation, gpui::Modifiers::none());
        cx.run_until_parked();

        assert!(settings_window.read_with(cx, |settings, _| {
            settings
                .editor
                .document()
                .terminal_themes
                .iter()
                .all(|theme| theme.id.as_str() != id)
        }));
    }

    #[test]
    fn reimporting_a_zed_family_replaces_its_themes() {
        let (settings, _) = UserSettings::load(std::sync::Arc::new(EmptyStorage));
        let token = settings.begin_preview(0).unwrap();
        let bytes = br##"{"themes":[{"name":"Sample","appearance":"dark","style":{"terminal.foreground":"#abcdef"}}]}"##;
        let install = || {
            import_family(bytes, |source| {
                settings.import_preview(&token, settings.snapshot().catalog_revision, source)
            })
        };

        assert_eq!(install(), installed_message(1));
        assert_eq!(install(), installed_message(1));
        assert_eq!(settings.snapshot().candidate.terminal_themes.len(), 1);
    }

    #[test]
    fn a_malformed_zed_family_installs_nothing_and_a_corrected_retry_is_clean() {
        let (settings, _) = UserSettings::load(std::sync::Arc::new(EmptyStorage));
        let token = settings.begin_preview(0).unwrap();
        let invalid = br##"{"themes":[{"name":"First","appearance":"dark","style":{}},{"name":"Broken","appearance":"sepia","style":{}}]}"##;
        let corrected = br##"{"themes":[{"name":"First","appearance":"dark","style":{}},{"name":"Second","appearance":"light","style":{}}]}"##;
        let install = |bytes| {
            import_family(bytes, |source| {
                settings.import_preview(&token, settings.snapshot().catalog_revision, source)
            })
        };

        assert_eq!(
            install(invalid).as_ref(),
            "That is not a Zed theme file SpaceTerm can read."
        );
        assert!(settings.snapshot().candidate.terminal_themes.is_empty());

        assert_eq!(install(corrected), installed_message(2));
        assert_eq!(settings.snapshot().candidate.terminal_themes.len(), 2);
    }

    #[test]
    fn install_failures_have_content_free_messages() {
        for (error, expected) in [
            (
                SettingsError::Busy,
                "Settings are busy. Try again when saving finishes.",
            ),
            (
                SettingsError::Catalog(CatalogError::TooManyThemes),
                "There is no room for those themes. Remove some installed themes and try again.",
            ),
            (
                SettingsError::Import(ImportError::InvalidThemeCount),
                "That source contains no themes SpaceTerm can use, or too many.",
            ),
        ] {
            let mut attempts = 0;
            let message = import_family(b"{}", |source| {
                assert!(matches!(source, ThemeImport::ZedFamily(_)));
                attempts += 1;
                Err(error)
            });
            assert_eq!(attempts, 1);
            assert_eq!(message.as_ref(), expected);
        }
    }
}
