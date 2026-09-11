//! Development-only exerciser for the production Appearance and User Settings Interfaces.

#[cfg(all(test, feature = "appearance-exerciser"))]
#[path = "appearance_exerciser_tests.rs"]
mod tests;

use std::{collections::BTreeSet, time::Duration};

use gpui::prelude::*;
use gpui::{
    App, Bounds, Context, Entity, Global, Render, TitlebarOptions, Window, WindowBounds,
    WindowHandle, WindowOptions, actions, div, px, rgba, size,
};
use spaceterm_ui::{
    Alert, AlertIntent, Button, ButtonSize, ButtonVariant, Checkbox, CheckboxState, ComboBox,
    ComboBoxItem, ContextMenu, Dialog, DialogCloseDecision, DialogInitialFocus, MenuEntry,
    ModalAction, ModalActionRole, ModalId, ProgressCancelDecision, ProgressCancellation,
    ProgressDialog, ProgressState, Switch, TextInput, TextInputContentMode, TextInputVariant,
    ToggleSize, Tooltip,
};

use crate::appearance::{
    Appearance, ChromeDensity, ChromeFontFamily, ResetTarget, SchemeId, SchemeKind,
    SchemeSelection, TerminalFontFamily, ZedImportKind, export_settings, parse_settings,
};
use crate::settings::{PreviewToken, SchemeImport};

use super::WorkspaceManager;
use super::appearance_runtime::{self, AppearanceRuntime};

const ENABLE_VARIABLE: &str = "SPACETERM_APPEARANCE_EXERCISER";

actions!(
    spaceterm,
    [ShowAppearanceExerciser, ToggleAppearancePreview]
);

struct ExerciserWindows {
    appearance: WindowHandle<AppearanceExerciser>,
    workspace: WindowHandle<WorkspaceManager>,
}
impl Global for ExerciserWindows {}

pub(crate) fn open(workspace: WindowHandle<WorkspaceManager>, cx: &mut App) -> gpui::Result<()> {
    if std::env::var(ENABLE_VARIABLE).as_deref() != Ok("1") {
        return Ok(());
    }
    if !cx.has_global::<AppearanceRuntime>() {
        return Err(anyhow::anyhow!("appearance runtime is unavailable"));
    }
    let bounds = Bounds::centered(None, size(px(920.0), px(420.0)), cx);
    let appearance = cx.open_window(
        WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            window_min_size: Some(size(px(680.0), px(320.0))),
            titlebar: Some(TitlebarOptions {
                title: Some("Appearance Exerciser".into()),
                appears_transparent: false,
                traffic_light_position: None,
            }),
            ..WindowOptions::default()
        },
        |window, cx| cx.new(|cx| AppearanceExerciser::new(window, cx)),
    )?;
    cx.set_global(ExerciserWindows {
        appearance,
        workspace,
    });
    cx.on_action(show_appearance_exerciser);
    cx.on_action(toggle_appearance_preview);
    Ok(())
}

fn show_appearance_exerciser(_: &ShowAppearanceExerciser, cx: &mut App) {
    let window = cx.global::<ExerciserWindows>().appearance;
    cx.defer(move |cx| {
        let _ = window.update(cx, |_, window, _| window.activate_window());
    });
}

fn toggle_appearance_preview(_: &ToggleAppearancePreview, cx: &mut App) {
    let window = cx.global::<ExerciserWindows>().appearance;
    cx.defer(move |cx| {
        let _ = window.update(cx, |exerciser, _, cx| exerciser.toggle_chrome(cx));
    });
}

struct AppearanceExerciser {
    editor: Entity<TextInput>,
    preview: Option<PreviewToken>,
    status: String,
    field_reset_index: usize,
    reset_index: usize,
    checkbox_state: CheckboxState,
    switch_on: bool,
}

impl AppearanceExerciser {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        cx.observe_global::<appearance_runtime::InstalledAppearance>(|_, cx| cx.notify())
            .detach();
        let settings = Self::settings(cx);
        let initial = settings
            .export_document()
            .unwrap_or_else(|_| String::from("{}"));
        let editor = cx.new(|cx| {
            TextInput::new("appearance-json", "Appearance JSON", initial, window, cx)
                .variant(TextInputVariant::Standard)
                .input_length_limit(Some(64 * 1024))
        });
        Self {
            editor,
            preview: None,
            status: String::from("Ready. Storage is isolated."),
            field_reset_index: 0,
            reset_index: 0,
            checkbox_state: CheckboxState::Mixed,
            switch_on: false,
        }
    }

    fn settings(cx: &App) -> crate::settings::UserSettings {
        cx.global::<AppearanceRuntime>().settings.clone()
    }

    fn editor_value(&self, cx: &App) -> String {
        self.editor.read(cx).value().to_owned()
    }

    fn set_editor(&self, value: String, cx: &mut Context<Self>) {
        self.editor.update(cx, |editor, cx| {
            editor.set_value(value, cx);
        });
    }

    fn ensure_preview(&mut self, cx: &App) -> Result<(), &'static str> {
        if self.preview.is_none() {
            let settings = Self::settings(cx);
            let revision = settings.snapshot().committed.revision;
            self.preview = Some(
                settings
                    .begin_preview(revision)
                    .map_err(|_| "Preview is busy or stale")?,
            );
        }
        Ok(())
    }

    fn apply_editor(&mut self, cx: &mut Context<Self>) {
        let result = (|| {
            let document = parse_settings(self.editor_value(cx).as_bytes())
                .map_err(|_| "Invalid settings JSON")?;
            self.ensure_preview(cx)?;
            Self::settings(cx)
                .update_preview(self.preview.as_ref().unwrap(), document)
                .map_err(|_| "Preview update rejected")
        })();
        self.status = result
            .map(|()| "Preview applied")
            .unwrap_or_else(|error| error)
            .to_owned();
        cx.notify();
    }

    fn toggle_chrome(&mut self, cx: &mut Context<Self>) {
        let result = (|| {
            self.ensure_preview(cx)?;
            let settings = Self::settings(cx);
            let mut candidate = (*settings.snapshot().candidate).clone();
            let chrome_light = matches!(
                candidate.preferences.chrome.scheme,
                SchemeSelection::Fixed {
                    appearance: Appearance::Light,
                    ..
                }
            );
            candidate.preferences.chrome.scheme = SchemeSelection::Fixed {
                id: SchemeId::new(if chrome_light {
                    "builtin.vague-pro.chrome.dark"
                } else {
                    "builtin.spaceterm.chrome.light"
                })
                .unwrap(),
                appearance: if chrome_light {
                    Appearance::Dark
                } else {
                    Appearance::Light
                },
            };
            settings
                .update_preview(self.preview.as_ref().unwrap(), candidate)
                .map_err(|_| "Scheme preview rejected")
        })();
        self.status = result
            .map(|()| "Chrome scheme toggled; terminal appearance retained")
            .unwrap_or_else(|error| error)
            .to_owned();
        self.export_settings_to_editor(cx);
        cx.notify();
    }

    fn toggle_terminal(&mut self, cx: &mut Context<Self>) {
        let result = (|| {
            self.ensure_preview(cx)?;
            let settings = Self::settings(cx);
            let mut candidate = (*settings.snapshot().candidate).clone();
            let terminal_light = matches!(
                candidate.preferences.terminal.scheme,
                SchemeSelection::Fixed {
                    appearance: Appearance::Light,
                    ..
                }
            );
            candidate.preferences.terminal.scheme = SchemeSelection::Fixed {
                id: SchemeId::new(if terminal_light {
                    "builtin.vague-pro.terminal.dark"
                } else {
                    "builtin.spaceterm.terminal.light"
                })
                .unwrap(),
                appearance: if terminal_light {
                    Appearance::Dark
                } else {
                    Appearance::Light
                },
            };
            settings
                .update_preview(self.preview.as_ref().unwrap(), candidate)
                .map_err(|_| "Terminal scheme preview rejected")
        })();
        self.status = result
            .map(|()| "Terminal scheme toggled; chrome appearance retained")
            .unwrap_or_else(|error| error)
            .to_owned();
        self.export_settings_to_editor(cx);
        cx.notify();
    }

    fn toggle_typography(&mut self, cx: &mut Context<Self>) {
        let result = (|| {
            self.ensure_preview(cx)?;
            let settings = Self::settings(cx);
            let mut candidate = (*settings.snapshot().candidate).clone();
            let alternate = candidate.preferences.chrome.typography.base_size == 13.0;
            candidate.preferences.chrome.typography.family = if alternate {
                ChromeFontFamily::Named {
                    family: String::from("Helvetica Neue"),
                }
            } else {
                ChromeFontFamily::SystemUi
            };
            candidate.preferences.chrome.typography.base_size = if alternate { 24.0 } else { 13.0 };
            candidate.preferences.chrome.density = if alternate {
                ChromeDensity::Comfortable
            } else {
                ChromeDensity::Compact
            };
            candidate.preferences.terminal.typography.family = if alternate {
                TerminalFontFamily::Named {
                    family: String::from("Menlo"),
                }
            } else {
                TerminalFontFamily::DefaultMonospace
            };
            candidate.preferences.terminal.typography.base_size =
                if alternate { 22.0 } else { 18.0 };
            candidate.preferences.terminal.typography.line_height =
                if alternate { 1.35 } else { 20.0 / 18.0 };
            settings
                .update_preview(self.preview.as_ref().unwrap(), candidate)
                .map_err(|_| "Typography preview rejected")
        })();
        self.status = result
            .map(|()| "Independent chrome/terminal fonts, sizes and density toggled")
            .unwrap_or_else(|error| error)
            .to_owned();
        self.export_settings_to_editor(cx);
        cx.notify();
    }

    fn follow_system(&mut self, cx: &mut Context<Self>) {
        let result = (|| {
            self.ensure_preview(cx)?;
            let settings = Self::settings(cx);
            let mut candidate = (*settings.snapshot().candidate).clone();
            candidate.preferences.chrome.scheme = SchemeSelection::System {
                light: SchemeId::new("builtin.spaceterm.chrome.light").unwrap(),
                dark: SchemeId::new("builtin.vague-pro.chrome.dark").unwrap(),
            };
            candidate.preferences.terminal.scheme = SchemeSelection::System {
                light: SchemeId::new("builtin.spaceterm.terminal.light").unwrap(),
                dark: SchemeId::new("builtin.vague-pro.terminal.dark").unwrap(),
            };
            settings
                .update_preview(self.preview.as_ref().unwrap(), candidate)
                .map_err(|_| "System policy preview rejected")
        })();
        self.status = result
            .map(|()| "Chrome and terminal now follow independent system slots")
            .unwrap_or_else(|error| error)
            .to_owned();
        self.export_settings_to_editor(cx);
        cx.notify();
    }

    fn reset_next_group(&mut self, cx: &mut Context<Self>) {
        const GROUPS: [ResetTarget; 6] = [
            ResetTarget::ChromeColors,
            ResetTarget::ChromeTypography,
            ResetTarget::ChromeDensity,
            ResetTarget::TerminalColors,
            ResetTarget::TerminalTypography,
            ResetTarget::TerminalRendering,
        ];
        let target = GROUPS[self.reset_index % GROUPS.len()].clone();
        let label = format!("Reset group {target:?}");
        self.reset_index = self.reset_index.wrapping_add(1);
        let result = (|| {
            self.ensure_preview(cx)?;
            let settings = Self::settings(cx);
            let token = self.preview.as_ref().unwrap();
            settings
                .reset_preview(token, target)
                .map_err(|_| "Group reset rejected")
        })();
        self.status = result
            .map(|()| label)
            .unwrap_or_else(|error| error.to_owned());
        self.export_settings_to_editor(cx);
        cx.notify();
    }

    fn reset_next_field(&mut self, cx: &mut Context<Self>) {
        let resolved = appearance_runtime::current(cx);
        let target = match self.field_reset_index % 16 {
            0 => ResetTarget::ChromeSchemeSelection,
            1 => ResetTarget::ChromeFontFamily,
            2 => ResetTarget::ChromeBaseSize,
            3 => ResetTarget::ChromeRegularWeight,
            4 => ResetTarget::ChromeEmphasisWeight,
            5 => ResetTarget::ChromeHeadingWeight,
            6 => ResetTarget::TerminalSchemeSelection,
            7 => ResetTarget::TerminalFontFamily,
            8 => ResetTarget::TerminalBaseSize,
            9 => ResetTarget::TerminalRegularWeight,
            10 => ResetTarget::TerminalBoldWeight,
            11 => ResetTarget::TerminalLineHeight,
            12 => ResetTarget::TerminalItalic,
            13 => ResetTarget::TerminalBoldAsBright,
            14 => ResetTarget::chrome_color_override(
                resolved.chrome.effective_scheme.clone(),
                "background",
            )
            .unwrap(),
            _ => ResetTarget::terminal_color_override(
                resolved.terminal.effective_scheme.clone(),
                "foreground",
            )
            .unwrap(),
        };
        self.field_reset_index = self.field_reset_index.wrapping_add(1);
        let label = format!("Reset field {target:?}");
        let result = (|| {
            self.ensure_preview(cx)?;
            Self::settings(cx)
                .reset_preview(self.preview.as_ref().unwrap(), target)
                .map_err(|_| "Field reset rejected")
        })();
        self.status = result
            .map(|()| label)
            .unwrap_or_else(|error| error.to_owned());
        self.export_settings_to_editor(cx);
        cx.notify();
    }

    fn reset_all(&mut self, cx: &mut Context<Self>) {
        let result = (|| {
            self.ensure_preview(cx)?;
            let settings = Self::settings(cx);
            settings
                .reset_preview(self.preview.as_ref().unwrap(), ResetTarget::AllAppearance)
                .map_err(|_| "Reset preview rejected")
        })();
        self.status = result
            .map(|()| "All appearance preferences reset; custom schemes retained")
            .unwrap_or_else(|error| error)
            .to_owned();
        self.export_settings_to_editor(cx);
        cx.notify();
    }

    fn cancel(&mut self, cx: &mut Context<Self>) {
        let result = self
            .preview
            .as_ref()
            .map_or(Ok(()), |token| Self::settings(cx).cancel_preview(token));
        if result.is_ok() {
            self.preview.take();
        }
        self.status = if result.is_ok() {
            "Preview cancelled"
        } else {
            "Cancel rejected"
        }
        .to_owned();
        self.export_settings_to_editor(cx);
        cx.notify();
    }

    fn commit(&mut self, cx: &mut Context<Self>) {
        let settings = Self::settings(cx);
        let preview_commit = self.preview.is_some();
        let job = if let Some(token) = self.preview.as_ref() {
            settings.commit_preview(token)
        } else {
            let document = match parse_settings(self.editor_value(cx).as_bytes()) {
                Ok(document) => document,
                Err(_) => {
                    self.status = String::from("Commit JSON is invalid");
                    cx.notify();
                    return;
                }
            };
            let revision = settings.snapshot().committed.revision;
            settings.update_committed(revision, document)
        };
        let Ok(job) = job else {
            self.status = String::from("Commit could not start");
            cx.notify();
            return;
        };
        self.status = String::from("Commit in progress");
        let settings_after_commit = settings.clone();
        let weak = cx.weak_entity();
        cx.spawn(async move |_, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { job.run() })
                .await;
            let _ = weak.update(cx, |this, cx| {
                this.status = match result {
                    Ok(outcome) if outcome.reload_required => {
                        if preview_commit {
                            this.preview.take();
                        }
                        String::from("Committed; reload required before another save")
                    }
                    Ok(_) => {
                        if preview_commit {
                            this.preview.take();
                        }
                        String::from("Committed and synchronized")
                    }
                    Err(_) => {
                        if !preview_commit
                            && let Some(candidate) = settings_after_commit
                                .snapshot()
                                .recoverable_candidate
                                .as_deref()
                            && let Ok(output) = export_settings(candidate)
                        {
                            this.set_editor(output, cx);
                        }
                        String::from("Commit failed; candidate remains recoverable")
                    }
                };
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn reload(&mut self, cx: &mut Context<Self>) {
        if self.preview.is_some() {
            self.status = String::from("Cancel preview before reload");
        } else {
            self.status = if Self::settings(cx).reload().is_ok() {
                "Reloaded isolated settings"
            } else {
                "Reload failed; last committed settings retained"
            }
            .to_owned();
            self.export_settings_to_editor(cx);
        }
        cx.notify();
    }

    fn import_editor(&mut self, cx: &mut Context<Self>) {
        let result = (|| {
            let bytes = self.editor_value(cx);
            self.ensure_preview(cx)?;
            let settings = Self::settings(cx);
            let catalog_revision = settings.snapshot().catalog_revision;
            settings
                .import_preview(
                    self.preview.as_ref().unwrap(),
                    catalog_revision,
                    SchemeImport::SpaceTerm(bytes.as_bytes()),
                    &BTreeSet::new(),
                )
                .map_err(|_| "Import collides or is invalid")
        })();
        self.status = result
            .map(|receipt| {
                format!(
                    "Imported {} schemes at catalog revision {}; no scheme was selected",
                    receipt.installed.len(),
                    receipt.catalog_revision
                )
            })
            .unwrap_or_else(str::to_owned);
        self.export_settings_to_editor(cx);
        cx.notify();
    }

    fn import_zed_editor(&mut self, cx: &mut Context<Self>) {
        let result = (|| {
            let bytes = self.editor_value(cx);
            let candidates =
                crate::settings::UserSettings::list_import_candidates(bytes.as_bytes())
                    .map_err(|_| "Invalid Zed theme family")?;
            let candidate = candidates.first().ok_or("Zed family has no candidate 0")?;
            let candidate_index = candidate.index;
            let candidate_name = candidate.name.clone();
            let candidate_appearance = candidate.appearance;
            self.ensure_preview(cx)?;
            let settings = Self::settings(cx);
            let catalog_revision = settings.snapshot().catalog_revision;
            settings
                .import_preview(
                    self.preview.as_ref().unwrap(),
                    catalog_revision,
                    SchemeImport::Zed {
                        bytes: bytes.as_bytes(),
                        candidate_index,
                        kinds: &[ZedImportKind::Chrome, ZedImportKind::Terminal],
                    },
                    &BTreeSet::new(),
                )
                .map_err(|_| "Zed candidate rejected")?;
            Ok(format!(
                "Imported candidate 0 '{}' ({:?}) into preview; no scheme was selected",
                candidate_name, candidate_appearance
            ))
        })();
        self.status = result.unwrap_or_else(|error: &'static str| error.to_owned());
        self.export_settings_to_editor(cx);
        cx.notify();
    }

    fn reload_fonts(&mut self, cx: &mut Context<Self>) {
        self.status = if appearance_runtime::reload_fonts(cx).is_ok() {
            "Font availability reloaded"
        } else {
            "Font reload rejected"
        }
        .to_owned();
        cx.notify();
    }

    fn show_terminal_window(&mut self, cx: &mut Context<Self>) {
        let window = cx.global::<ExerciserWindows>().workspace;
        self.status = if window
            .update(cx, |_, window, _| window.activate_window())
            .is_ok()
        {
            "Activated terminal window"
        } else {
            "Terminal window is no longer available"
        }
        .to_owned();
        cx.notify();
    }

    fn show_fixture_window(&mut self, cx: &mut Context<Self>) {
        let bounds = Bounds::centered(None, size(px(640.0), px(440.0)), cx);
        self.status = if cx
            .open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    window_min_size: Some(size(px(480.0), px(320.0))),
                    titlebar: Some(TitlebarOptions {
                        title: Some("Appearance Acceptance Fixtures".into()),
                        appears_transparent: false,
                        traffic_light_position: None,
                    }),
                    ..WindowOptions::default()
                },
                |window, cx| cx.new(|cx| AppearanceFixtures::new(window, cx)),
            )
            .is_ok()
        {
            "Opened modal and obscured-control fixtures"
        } else {
            "Fixture window could not open"
        }
        .to_owned();
        cx.notify();
    }

    fn export_settings_to_editor(&self, cx: &mut Context<Self>) {
        if let Ok(output) = Self::settings(cx).export_document() {
            self.set_editor(output, cx);
        }
    }

    fn export_effective_schemes(&mut self, cx: &mut Context<Self>) {
        let settings = Self::settings(cx);
        let current = appearance_runtime::current(cx);
        match settings.export_schemes(&[
            (SchemeKind::Chrome, current.chrome.effective_scheme.clone()),
            (
                SchemeKind::Terminal,
                current.terminal.effective_scheme.clone(),
            ),
        ]) {
            Ok(output) => {
                self.set_editor(output, cx);
                self.status = String::from("Exported complete effective color schemes");
            }
            Err(_) => self.status = String::from("Scheme export failed"),
        }
        cx.notify();
    }

    fn diagnostics(&self, cx: &App) -> String {
        let current = appearance_runtime::current(cx);
        let settings = Self::settings(cx).snapshot();
        format!(
            "generation={} revision={} phase={:?} storage={:?} chrome requested={} effective={} terminal requested={} effective={} diagnostics={:?}",
            current.generation.get(),
            settings.committed.revision,
            settings.phase,
            settings.status,
            current.chrome.requested_scheme,
            current.chrome.effective_scheme,
            current.terminal.requested_scheme,
            current.terminal.effective_scheme,
            current.diagnostics
        )
    }
}

impl Render for AppearanceExerciser {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let appearance = super::appearance::chrome(cx).clone();
        let background = rgba(appearance.colors.background.rgba_hex());
        let foreground = rgba(appearance.colors.text.rgba_hex());
        let muted = rgba(appearance.colors.text_muted.rgba_hex());
        let editor_background = rgba(appearance.colors.input_background.rgba_hex());
        let editor_border = rgba(appearance.colors.input_border.rgba_hex());
        let weak = cx.weak_entity();
        let preview_shortcut =
            crate::desktop_profile::DesktopPresentation::get(cx).shortcut(&ToggleAppearancePreview);
        let action =
            |id: &'static str, label: &'static str, handler: fn(&mut Self, &mut Context<Self>)| {
                let weak = weak.clone();
                Button::new(id, label)
                    .size(ButtonSize::Small)
                    .variant(ButtonVariant::Secondary)
                    .on_activate(move |_, _, cx| {
                        let _ = weak.update(cx, handler);
                    })
            };
        let checkbox_weak = weak.clone();
        let checkbox = Checkbox::new(
            "appearance-checkbox",
            "Restore panes when SpaceTerm opens",
            self.checkbox_state,
        )
        .size(ToggleSize::Regular)
        .on_change(move |change, _, cx| {
            let _ = checkbox_weak.update(cx, |this, cx| {
                this.checkbox_state = change.requested();
                cx.notify();
            });
        });
        let switch_weak = weak.clone();
        let notification_switch = Switch::new(
            "appearance-switch",
            "Attention notifications",
            self.switch_on,
        )
        .size(ToggleSize::Regular)
        .on_change(move |change, _, cx| {
            let _ = switch_weak.update(cx, |this, cx| {
                this.switch_on = change.requested();
                cx.notify();
            });
        });
        let content = div().id("appearance-exerciser-scroll").size_full().flex().flex_col().overflow_y_scroll()
            .gap(appearance.spacing(10.0))
            .p(appearance.spacing(14.0))
            .bg(background)
            .text_color(foreground)
            .text_size(appearance.text_size(13.0))
            .font(appearance.regular.clone())
            .child(div().font(appearance.heading.clone()).text_size(appearance.text_size(16.0)).child("SpaceTerm Appearance Exerciser"))
            .child(div().debug_selector({
                let generation = appearance_runtime::current(cx).generation.get();
                move || format!("appearance-diagnostics-generation-{generation}")
            }).text_size(appearance.text_size(11.0)).text_color(muted).whitespace_normal().child(self.diagnostics(cx)))
            .child(div().h(appearance.height(32.0, 13.0)).bg(editor_background).border_1().border_color(editor_border).child(self.editor.clone()))
            .child(div().flex().flex_wrap().gap(px(8.0))
                .child(action("appearance-apply", "Apply JSON Preview", Self::apply_editor))
                .child(
                    action("appearance-toggle-chrome", "Toggle Chrome", Self::toggle_chrome)
                        .tooltip(
                            Tooltip::new(
                                "appearance-toggle-chrome-tooltip",
                                "Toggle Chrome Preview Without Activating This Window",
                            )
                            .keyboard_equivalent(preview_shortcut),
                        ),
                )
                .child(action("appearance-toggle-terminal", "Toggle Terminal", Self::toggle_terminal))
                .child(action("appearance-toggle-type", "Toggle Fonts/Density", Self::toggle_typography))
                .child(action("appearance-system", "Follow System", Self::follow_system))
                .child(action("appearance-reset-field", "Reset Next Field", Self::reset_next_field))
                .child(action("appearance-reset-group", "Reset Next Group", Self::reset_next_group))
                .child(action("appearance-reset", "Reset All", Self::reset_all))
                .child(action("appearance-cancel", "Cancel", Self::cancel))
                .child(action("appearance-commit", "Commit", Self::commit))
                .child(action("appearance-reload", "Reload", Self::reload))
                .child(action("appearance-import", "Import Color Package", Self::import_editor))
                .child(action("appearance-import-zed", "Import Zed Candidate 0", Self::import_zed_editor))
                .child(action("appearance-export", "Export Effective Schemes", Self::export_effective_schemes))
                .child(action("appearance-reload-fonts", "Reload Fonts", Self::reload_fonts))
                .child(action("appearance-show-fixtures", "Show Acceptance Fixtures", Self::show_fixture_window))
                .child(action("appearance-show-terminal", "Show Terminal Window", Self::show_terminal_window)))
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap(appearance.spacing(18.0))
                    .child(checkbox)
                    .child(notification_switch),
            )
            .child(div().text_size(appearance.text_size(12.0)).whitespace_normal().child(self.status.clone()))
            .child(div().text_size(appearance.text_size(11.0)).text_color(muted).whitespace_normal().child(
                "The JSON editor is a bounded single-line development field. Export, edit or paste a native settings/color package, then use the matching action. Preview never writes; Commit uses the isolated retained Config root."
            ));
        spaceterm_ui::ModalLayer::new(spaceterm_ui::TooltipLayer::new(content))
    }
}

struct AppearanceDialogBody;

impl Render for AppearanceDialogBody {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let appearance = super::appearance::chrome(cx);
        div()
            .text_size(appearance.text_size(13.0))
            .child("This body verifies elevated surfaces, text, borders and focus presentation.")
    }
}

struct AppearanceFixtures {
    input: Entity<TextInput>,
}

impl AppearanceFixtures {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        Self {
            input: cx.new(|cx| {
                TextInput::new(
                    "appearance-obscured-input",
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

    fn show_alert(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let _ = Alert::new(
            ModalId::new("appearance-alert-fixture"),
            "Appearance alert fixture",
            "Appearance Alert",
            "Verify scrim, elevated surface, semantic warning colors and action states.",
            vec![ModalAction::new(
                (),
                "Close",
                ModalActionRole::Cancel,
                "appearance-alert-close",
            )],
        )
        .intent(AlertIntent::Warning)
        .present(window, cx, |_, _| {});
    }

    fn show_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let body = cx.new(|_| AppearanceDialogBody);
        let _ = Dialog::new(
            ModalId::new("appearance-dialog-fixture"),
            "Appearance dialog fixture",
            "Appearance Dialog",
            vec![ModalAction::new(
                (),
                "Close",
                ModalActionRole::Cancel,
                "appearance-dialog-close",
            )],
            DialogInitialFocus::Action(()),
        )
        .description("Verify typography, spacing, focus and action contrast.")
        .body(body)
        .present(window, cx, |_, _, _| DialogCloseDecision::Allow, |_, _| {});
    }

    fn show_progress(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let _ = ProgressDialog::<()>::new(
            ModalId::new("appearance-progress-fixture"),
            "Appearance progress fixture",
            "Appearance Progress",
            "Checking semantic progress presentation",
            ProgressState::Indeterminate,
            ProgressCancellation::programmatic_only(Duration::from_secs(30)),
        )
        .detail("The fixture expires automatically after thirty seconds.")
        .present(
            window,
            cx,
            |_, _, _| ProgressCancelDecision::Allow,
            |_, _| {},
        );
    }
}

impl Render for AppearanceFixtures {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let appearance = super::appearance::chrome(cx).clone();
        let weak = cx.weak_entity();
        let modal_action =
            |id: &'static str,
             label: &'static str,
             handler: fn(&mut Self, &mut Window, &mut Context<Self>)| {
                let weak = weak.clone();
                Button::new(id, label)
                    .variant(ButtonVariant::Secondary)
                    .on_activate(move |_, window, cx| {
                        let _ = weak.update(cx, |this, cx| handler(this, window, cx));
                    })
            };
        let combo = ComboBox::new(
            "appearance-obscured-combo",
            "Obscured combo box fixture",
            None,
            "Choose a semantic state",
            vec![
                ComboBoxItem::new(1_u8, "Normal"),
                ComboBoxItem::new(2_u8, "Selected"),
                ComboBoxItem::new(3_u8, "Warning"),
            ],
        )
        .on_accept(|_, _, _| {});
        let menu = ContextMenu::new(
            "appearance-obscured-menu",
            "Obscured menu fixture",
            Button::new("appearance-menu-trigger", "Open Menu").variant(ButtonVariant::Secondary),
            vec![
                MenuEntry::action("Normal item", 1_u8),
                MenuEntry::action("Selected item", 2_u8),
            ],
        )
        .on_activate(|_, _, _| {});
        let content = div()
            .id("appearance-fixtures-scroll")
            .size_full()
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .gap(appearance.spacing(12.0))
            .p(appearance.spacing(16.0))
            .bg(rgba(appearance.colors.background.rgba_hex()))
            .text_color(rgba(appearance.colors.text.rgba_hex()))
            .text_size(appearance.text_size(13.0))
            .font(appearance.regular.clone())
            .child(
                div()
                    .font(appearance.heading.clone())
                    .text_size(appearance.text_size(16.0))
                    .child("Obscured Controls and Modal Fixtures"),
            )
            .child(self.input.clone())
            .child(combo)
            .child(menu)
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .gap(appearance.spacing(8.0))
                    .child(modal_action(
                        "appearance-show-alert",
                        "Show Alert",
                        Self::show_alert,
                    ))
                    .child(modal_action(
                        "appearance-show-dialog",
                        "Show Dialog",
                        Self::show_dialog,
                    ))
                    .child(modal_action(
                        "appearance-show-progress",
                        "Show Progress",
                        Self::show_progress,
                    )),
            )
            .child(
                div()
                    .text_size(appearance.text_size(11.0))
                    .text_color(rgba(appearance.colors.text_muted.rgba_hex()))
                    .whitespace_normal()
                    .child("Open each modal to verify that the input, combo box and menu remain safely obscured and cannot receive interaction through the scrim."),
            );
        spaceterm_ui::ModalLayer::new(spaceterm_ui::TooltipLayer::new(content))
    }
}
