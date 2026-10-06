//! Application-owned, event-driven appearance resolution and publication.

#[cfg(test)]
#[path = "appearance_runtime_tests.rs"]
mod tests;

#[cfg(test)]
pub(crate) use tests::font_catalog_test_app;

use std::{borrow::Cow, rc::Rc, sync::Arc};

use gpui::{App, Global, Task, font, px};

use crate::platform::window_frame::WindowFrameGeometry;

use crate::appearance::{
    AppearanceChangeSet, AppearanceGeneration, AppearancePreferences, AvailableFont,
    AvailableFonts, CompositionCapabilities, FontClass, ResolvedAppearance, SystemAppearance,
    TerminalFontFamily, ThemeCatalog,
};
use crate::platform::appearance::{AppearancePlatform, SystemAppearanceSubscription};
use crate::settings::{Settings, SettingsError};

use super::appearance::{ChromeAppearance, InstalledChrome, settings};

#[derive(Clone)]
pub(crate) struct InstalledAppearance(pub(crate) Arc<ResolvedAppearance>);
impl Global for InstalledAppearance {}

#[cfg(feature = "developer-tools")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum AccessibilityPreviewFact {
    RequireOpaqueSurfaces,
    IncreaseContrast,
    ShowBorders,
    ReduceMotion,
    DifferentiateWithoutColor,
}

#[cfg(feature = "developer-tools")]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct AccessibilityPreviewOverride {
    require_opaque_surfaces: Option<bool>,
    increase_contrast: Option<bool>,
    show_borders: Option<bool>,
    reduce_motion: Option<bool>,
    differentiate_without_color: Option<bool>,
}

#[cfg(feature = "developer-tools")]
impl AccessibilityPreviewOverride {
    fn apply(self, mut capabilities: CompositionCapabilities) -> CompositionCapabilities {
        capabilities.require_opaque_surfaces = self
            .require_opaque_surfaces
            .unwrap_or(capabilities.require_opaque_surfaces);
        capabilities.increase_contrast = self
            .increase_contrast
            .unwrap_or(capabilities.increase_contrast);
        capabilities.show_borders = self.show_borders.unwrap_or(capabilities.show_borders);
        capabilities.reduce_motion = self.reduce_motion.unwrap_or(capabilities.reduce_motion);
        capabilities.differentiate_without_color = self
            .differentiate_without_color
            .unwrap_or(capabilities.differentiate_without_color);
        capabilities
    }

    fn set(&mut self, fact: AccessibilityPreviewFact, enabled: bool) {
        let value = Some(enabled);
        match fact {
            AccessibilityPreviewFact::RequireOpaqueSurfaces => self.require_opaque_surfaces = value,
            AccessibilityPreviewFact::IncreaseContrast => self.increase_contrast = value,
            AccessibilityPreviewFact::ShowBorders => self.show_borders = value,
            AccessibilityPreviewFact::ReduceMotion => self.reduce_motion = value,
            AccessibilityPreviewFact::DifferentiateWithoutColor => {
                self.differentiate_without_color = value;
            }
        }
    }
}

pub(crate) struct AppearanceRuntime {
    pub(crate) settings: Settings,
    platform: Rc<dyn AppearancePlatform>,
    fonts: AvailableFonts,
    pending_font_names: Option<Vec<String>>,
    control_motion: spaceterm_ui::ControlMotion,
    #[cfg(feature = "developer-tools")]
    accessibility_preview: AccessibilityPreviewOverride,
    _tasks: Vec<Task<()>>,
    _observation: Option<Box<dyn SystemAppearanceSubscription>>,
}
impl Global for AppearanceRuntime {}

/// Register all private host and terminal faces before capturing the font catalog.
pub(crate) fn register_fonts(cx: &App) -> gpui::Result<()> {
    let host = crate::host_fonts::HostFonts::get(cx);
    cx.text_system().add_fonts(
        crate::bundled_font::FACES
            .iter()
            .chain(host.bundled_ui_faces.iter())
            .map(|bytes| Cow::Borrowed(*bytes))
            .collect(),
    )
}

pub(crate) fn install(
    settings: Settings,
    platform: Rc<dyn AppearancePlatform>,
    cx: &mut App,
) -> Result<(), SettingsError> {
    let changed = settings.subscribe();
    let (fonts, pending_font_names) =
        capture_initial_fonts(cx, &settings.snapshot().candidate.appearance);
    let observation = platform.observe();
    let mut tasks = vec![cx.spawn(async move |cx| {
        while changed.recv().await.is_ok() {
            while changed.try_recv().is_ok() {}
            cx.update(|cx| {
                let _ = refresh(cx);
            });
        }
    })];
    let observation = observation.map(|observation| {
        tasks.push(cx.spawn(async move |cx| {
            while observation.changed.recv().await.is_ok() {
                while observation.changed.try_recv().is_ok() {}
                cx.update(|cx| {
                    let _ = refresh(cx);
                });
            }
        }));
        observation.subscription
    });
    cx.set_global(AppearanceRuntime {
        settings,
        platform,
        fonts,
        pending_font_names,
        control_motion: spaceterm_ui::ControlMotion::Standard,
        #[cfg(feature = "developer-tools")]
        accessibility_preview: AccessibilityPreviewOverride::default(),
        _tasks: tasks,
        _observation: observation,
    });
    refresh(cx)
}

pub(crate) fn refresh(cx: &mut App) -> Result<(), SettingsError> {
    let (platform, candidate, previous_control_motion) = {
        let runtime = cx.global::<AppearanceRuntime>();
        (
            Rc::clone(&runtime.platform),
            runtime.settings.snapshot().candidate,
            runtime.control_motion,
        )
    };
    ensure_selected_fonts(&candidate.appearance, cx);
    let fonts = cx.global::<AppearanceRuntime>().fonts.clone();
    let catalog = ThemeCatalog::from_terminal_themes(&candidate.terminal_themes)
        .map_err(|_| SettingsError::Invalid)?;
    let generation = cx
        .try_global::<InstalledAppearance>()
        .map_or(Some(AppearanceGeneration::INITIAL), |installed| {
            installed.0.generation.next()
        })
        .ok_or(SettingsError::RevisionExhausted)?;
    let accessibility = platform.accessibility_display_options();
    let native_composition = platform.native_window_composition(cx);
    let capabilities = CompositionCapabilities {
        native_window_opacity: native_composition.opacity,
        native_window_blur: native_composition.blur,
        require_opaque_surfaces: accessibility.require_opaque_surfaces,
        increase_contrast: accessibility.increase_contrast,
        show_borders: accessibility.show_borders,
        reduce_motion: platform.prefers_reduced_motion(),
        differentiate_without_color: accessibility.differentiate_without_color,
    };
    #[cfg(feature = "developer-tools")]
    let capabilities = cx
        .global::<AppearanceRuntime>()
        .accessibility_preview
        .apply(capabilities);
    let resolved = catalog
        .resolve(
            generation,
            &candidate.appearance,
            SystemAppearance::from(platform.system_appearance()).with_composition(capabilities),
            &fonts,
        )
        .map_err(|_| SettingsError::Invalid)?;
    let control_motion =
        resolved_control_motion(resolved.chrome.composition.capabilities.reduce_motion);
    let changes = cx
        .try_global::<InstalledAppearance>()
        .map(|previous| AppearanceChangeSet::between(&previous.0, &resolved));
    let control_motion_changed = previous_control_motion != control_motion;
    if !control_motion_changed
        && cx
            .try_global::<InstalledAppearance>()
            .is_some_and(|previous| {
                previous.0.chrome == resolved.chrome
                    && previous.0.terminal == resolved.terminal
                    && previous.0.diagnostics == resolved.diagnostics
            })
    {
        return Ok(());
    }
    let chrome_changed = changes.is_none_or(|changes| {
        changes.chrome_colors
            || changes.chrome_typography
            || changes.chrome_metrics
            || changes.window_composition
    });
    if chrome_changed || control_motion_changed {
        let (prepared, inactive) = ChromeAppearance::prepare_variants(&resolved.chrome);
        let (settings_prepared, settings_inactive) =
            settings::prepare_variants(&resolved.chrome, prepared.clone(), inactive.clone());
        let controls = Box::new(
            super::control_theme::catalog(&prepared, control_motion)
                .generation(spaceterm_ui::ControlThemeGeneration::new(generation.get())),
        );
        let inactive_controls = Box::new(
            super::control_theme::catalog(&inactive, control_motion)
                .generation(spaceterm_ui::ControlThemeGeneration::new(generation.get())),
        );
        let settings_controls = Box::new(
            super::control_theme::catalog(&settings_prepared.chrome, control_motion)
                .generation(spaceterm_ui::ControlThemeGeneration::new(generation.get())),
        );
        let settings_inactive_controls = Box::new(
            super::control_theme::catalog(&settings_inactive.chrome, control_motion)
                .generation(spaceterm_ui::ControlThemeGeneration::new(generation.get())),
        );
        if cx.has_global::<InstalledAppearance>() {
            spaceterm_ui::replace_control_theme_catalogs(
                cx,
                controls,
                inactive_controls,
                settings_controls,
                settings_inactive_controls,
            )
            .map_err(|_| SettingsError::Invalid)?;
        }

        if chrome_changed {
            cx.set_global(InstalledChrome {
                active: Arc::new(prepared),
                inactive: Arc::new(inactive),
            });
            cx.set_global(settings::InstalledSettingsChrome {
                active: Arc::new(settings_prepared),
                inactive: Arc::new(settings_inactive),
            });
        }
    }
    if changes.is_none_or(|changes| changes.native_appearance) {
        platform.apply_native_appearance(resolved.chrome.appearance);
    }
    cx.global_mut::<AppearanceRuntime>().control_motion = control_motion;
    cx.set_global(InstalledAppearance(Arc::new(resolved)));
    Ok(())
}

fn resolved_control_motion(reduced: bool) -> spaceterm_ui::ControlMotion {
    if reduced {
        spaceterm_ui::ControlMotion::Reduced
    } else {
        spaceterm_ui::ControlMotion::Standard
    }
}

pub(crate) fn control_motion(cx: &App) -> spaceterm_ui::ControlMotion {
    cx.try_global::<AppearanceRuntime>()
        .map_or(spaceterm_ui::ControlMotion::Standard, |runtime| {
            runtime.control_motion
        })
}

#[cfg(feature = "developer-tools")]
pub(crate) fn set_accessibility_preview(
    fact: AccessibilityPreviewFact,
    enabled: bool,
    cx: &mut App,
) -> Result<(), SettingsError> {
    let previous = {
        let runtime = cx.global_mut::<AppearanceRuntime>();
        let previous = runtime.accessibility_preview;
        runtime.accessibility_preview.set(fact, enabled);
        previous
    };
    if let Err(error) = refresh(cx) {
        cx.global_mut::<AppearanceRuntime>().accessibility_preview = previous;
        return Err(error);
    }
    Ok(())
}

#[cfg(feature = "developer-tools")]
pub(crate) fn reset_accessibility_preview(cx: &mut App) -> Result<(), SettingsError> {
    let previous = {
        let runtime = cx.global_mut::<AppearanceRuntime>();
        std::mem::take(&mut runtime.accessibility_preview)
    };
    if let Err(error) = refresh(cx) {
        cx.global_mut::<AppearanceRuntime>().accessibility_preview = previous;
        return Err(error);
    }
    Ok(())
}

fn available_font(text: &gpui::TextSystem, family: String) -> AvailableFont {
    let id = text.resolve_font(&font(family.clone()));
    let widths = ['i', 'M', '0', ' '].map(|character| text.advance(id, px(18.0), character));
    let monospace = widths.iter().all(|width| width.is_ok())
        && widths.windows(2).all(|pair| {
            (f32::from(pair[0].as_ref().unwrap().width)
                - f32::from(pair[1].as_ref().unwrap().width))
            .abs()
                < 0.01
        });
    AvailableFont {
        resolution_identity: format!("{id:?}"),
        family,
        class: if monospace {
            FontClass::Monospace
        } else {
            FontClass::Proportional
        },
    }
}

fn base_fonts(installed: Vec<AvailableFont>, cx: &App) -> AvailableFonts {
    let host = crate::host_fonts::HostFonts::get(cx);
    AvailableFonts {
        system_ui: AvailableFont {
            family: host.ui_family,
            class: FontClass::Proportional,
            resolution_identity: "system-ui".into(),
        },
        system_monospace: AvailableFont {
            family: host.system_monospace_family,
            class: FontClass::Monospace,
            resolution_identity: "system-monospace".into(),
        },
        installed,
        terminal_families: host
            .terminal_families
            .iter()
            .map(|family| (*family).into())
            .collect(),
        emoji_family: host.emoji_family,
    }
}

fn selected_font(
    family: &str,
    preferences: &AppearancePreferences,
    host: &crate::host_fonts::HostFonts,
) -> bool {
    match &preferences.terminal.typography.family {
        TerminalFontFamily::DefaultMonospace => {
            host.terminal_families.contains(&family) || host.system_monospace_family == family
        }
        TerminalFontFamily::Named { family: selected } => selected == family,
    }
}

/// Enumerate once before the first window, but classify only families that can affect its type.
fn capture_initial_fonts(
    cx: &App,
    preferences: &AppearancePreferences,
) -> (AvailableFonts, Option<Vec<String>>) {
    let text = cx.text_system();
    let names = text.all_font_names();
    let host = crate::host_fonts::HostFonts::get(cx);
    let installed = names
        .iter()
        .filter(|family| selected_font(family, preferences, &host))
        .cloned()
        .map(|family| available_font(text, family))
        .collect();
    (base_fonts(installed, cx), Some(names))
}

/// Classify a newly requested family before resolving an appearance change.
fn ensure_selected_fonts(preferences: &AppearancePreferences, cx: &mut App) {
    let host = crate::host_fonts::HostFonts::get(cx);
    let missing = {
        let runtime = cx.global::<AppearanceRuntime>();
        runtime.pending_font_names.as_ref().map(|names| {
            names
                .iter()
                .filter(|family| {
                    selected_font(family, preferences, &host)
                        && !runtime
                            .fonts
                            .installed
                            .iter()
                            .any(|font| font.family == family.as_str())
                })
                .cloned()
                .collect::<Vec<_>>()
        })
    };
    let Some(missing) = missing else {
        return;
    };
    let text = cx.text_system().clone();
    let added = missing
        .into_iter()
        .map(|family| available_font(&text, family));
    cx.global_mut::<AppearanceRuntime>()
        .fonts
        .installed
        .extend(added);
}

pub(crate) fn complete_font_catalog(cx: &mut App) {
    if !cx.has_global::<AppearanceRuntime>() {
        return;
    }
    let Some(_) = cx
        .global_mut::<AppearanceRuntime>()
        .pending_font_names
        .take()
    else {
        return;
    };
    let selected = cx.global::<AppearanceRuntime>().fonts.installed.clone();
    let text = cx.text_system().clone();
    let installed = text
        .all_font_names()
        .into_iter()
        .map(|family| {
            selected
                .iter()
                .find(|font| font.family == family)
                .cloned()
                .unwrap_or_else(|| available_font(&text, family))
        })
        .collect();
    cx.global_mut::<AppearanceRuntime>().fonts.installed = installed;
}

/// Classify all fonts only for explicit font reloads.
#[cfg(any(test, feature = "developer-tools"))]
fn capture_fonts(cx: &App) -> AvailableFonts {
    let text = cx.text_system();
    base_fonts(
        text.all_font_names()
            .into_iter()
            .map(|family| available_font(text, family))
            .collect(),
        cx,
    )
}

#[cfg(feature = "developer-tools")]
pub(crate) fn reload_fonts(cx: &mut App) -> Result<(), SettingsError> {
    let fonts = capture_fonts(cx);
    cx.global_mut::<AppearanceRuntime>().fonts = fonts;
    cx.global_mut::<AppearanceRuntime>().pending_font_names = None;
    refresh(cx)
}

/// The font availability captured at startup or at the last explicit font reload.
pub(crate) fn available_fonts(cx: &App) -> AvailableFonts {
    cx.try_global::<AppearanceRuntime>()
        .map(|runtime| runtime.fonts.clone())
        .unwrap_or_else(|| base_fonts(Vec::new(), cx))
}

pub(crate) fn current(cx: &App) -> Arc<ResolvedAppearance> {
    cx.try_global::<InstalledAppearance>()
        .map(|value| Arc::clone(&value.0))
        .unwrap_or_else(|| {
            Arc::new(
                ThemeCatalog::default()
                    .resolve(
                        AppearanceGeneration::INITIAL,
                        &Default::default(),
                        SystemAppearance::unavailable(),
                        &base_fonts(Vec::new(), cx),
                    )
                    .expect("built-in appearance is valid"),
            )
        })
}

/// Which client titlebar height anchors one window's native traffic lights.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum TrafficLightChrome {
    Workspace,
    SidebarWindow,
}

/// Owns one Operating-System Window's native traffic-light position across density changes. The
/// native position is fixed at window open, so Comfortable density would otherwise leave it high.
pub(crate) struct WindowTrafficLightOwner {
    role: TrafficLightChrome,
    applied: Option<gpui::Point<gpui::Pixels>>,
}

impl WindowTrafficLightOwner {
    pub(crate) fn workspace() -> Self {
        Self {
            role: TrafficLightChrome::Workspace,
            applied: None,
        }
    }

    pub(crate) fn sidebar_window() -> Self {
        Self {
            role: TrafficLightChrome::SidebarWindow,
            applied: None,
        }
    }

    pub(crate) fn desired_position(&self, cx: &App) -> Option<gpui::Point<gpui::Pixels>> {
        let appearance = super::appearance::chrome(cx);
        let geometry = cx
            .try_global::<WindowFrameGeometry>()
            .copied()
            .unwrap_or_default();
        match self.role {
            TrafficLightChrome::Workspace => {
                let height = super::workspace_frame::WorkspaceFrame::for_appearance(appearance, cx)
                    .top_chrome_height(appearance.top_height());
                geometry.workspace_traffic_light_position(height)
            }
            TrafficLightChrome::SidebarWindow => {
                geometry.sidebar_window_traffic_light_position(appearance.top_height())
            }
        }
    }

    pub(crate) fn apply(&mut self, window: &gpui::Window, cx: &App) {
        let Some(desired) = self.desired_position(cx) else {
            return;
        };
        if self.applied != Some(desired) {
            crate::platform::window_frame::place_traffic_lights(window, desired);
            self.applied = Some(desired);
        }
    }
}

/// Owns native backdrop effects for one Operating-System Window. The application native
/// Light/Dark setting remains app-scoped through AppearancePlatform.
#[derive(Default)]
pub(crate) struct WindowAppearanceOwner {
    effective: Option<crate::appearance::WindowBackgroundAppearance>,
    backdrop: Option<crate::platform::appearance::WindowBackdrop>,
}

impl WindowAppearanceOwner {
    pub(crate) fn apply(&mut self, window: &mut gpui::Window, cx: &App) {
        let chrome = current(cx).chrome.clone();
        let effective = chrome.composition.effective;
        if self.effective != Some(effective) {
            window.set_background_appearance(native_background(effective));
            self.effective = Some(effective);
        }
        let backdrop = requested_backdrop(
            effective,
            crate::appearance::ChromeTone::of(chrome.colors.background),
        );
        if self.backdrop == Some(backdrop) {
            return;
        }
        if let Some(runtime) = cx.try_global::<AppearanceRuntime>() {
            runtime.platform.apply_window_backdrop(window, backdrop);
        }
        self.backdrop = Some(backdrop);
    }
}

/// What must sit behind this window's content. The Chrome tone is read from the compiled window
/// root rather than the Light or Dark slot, so the material matches the paint.
fn requested_backdrop(
    effective: crate::appearance::WindowBackgroundAppearance,
    tone: crate::appearance::ChromeTone,
) -> crate::platform::appearance::WindowBackdrop {
    use crate::platform::appearance::WindowBackdrop;
    match effective {
        crate::appearance::WindowBackgroundAppearance::Blurred => WindowBackdrop::Frosted(tone),
        crate::appearance::WindowBackgroundAppearance::Opaque
        | crate::appearance::WindowBackgroundAppearance::Transparent => WindowBackdrop::Absent,
    }
}

pub(crate) fn window_background(cx: &App) -> gpui::WindowBackgroundAppearance {
    native_background(current(cx).chrome.composition.effective)
}

/// A blurred window asks the framework for a transparent one. SpaceTerm owns the blurred backdrop
/// through `AppearancePlatform` because GPUI's blurred background rewrites the native material's
/// private layers and leaves the desktop unblurred.
fn native_background(
    appearance: crate::appearance::WindowBackgroundAppearance,
) -> gpui::WindowBackgroundAppearance {
    match appearance {
        crate::appearance::WindowBackgroundAppearance::Opaque => {
            gpui::WindowBackgroundAppearance::Opaque
        }
        crate::appearance::WindowBackgroundAppearance::Transparent
        | crate::appearance::WindowBackgroundAppearance::Blurred => {
            gpui::WindowBackgroundAppearance::Transparent
        }
    }
}
