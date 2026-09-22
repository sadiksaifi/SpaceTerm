//! Application-owned, event-driven appearance resolution and publication.

#[cfg(test)]
#[path = "appearance_runtime_tests.rs"]
mod tests;

use std::{rc::Rc, sync::Arc};

use gpui::{App, Global, Task, font, px};

use crate::platform::window_frame::WindowFrameGeometry;

use crate::appearance::{
    AppearanceChangeSet, AppearanceGeneration, AvailableFont, AvailableFonts,
    CompositionCapabilities, FontClass, ResolvedAppearance, SchemeCatalog, SystemAppearance,
};
use crate::platform::appearance::{AppearancePlatform, SystemAppearanceSubscription};
use crate::settings::{SettingsError, UserSettings};

use super::appearance::{ChromeAppearance, InstalledChrome, settings};

#[derive(Clone)]
pub(crate) struct InstalledAppearance(pub(crate) Arc<ResolvedAppearance>);
impl Global for InstalledAppearance {}

#[cfg(feature = "appearance-exerciser")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum AccessibilityPreviewFact {
    ReduceTransparency,
    IncreaseContrast,
    ShowBorders,
    ReduceMotion,
    DifferentiateWithoutColor,
}

#[cfg(feature = "appearance-exerciser")]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct AccessibilityPreviewOverride {
    reduce_transparency: Option<bool>,
    increase_contrast: Option<bool>,
    show_borders: Option<bool>,
    reduce_motion: Option<bool>,
    differentiate_without_color: Option<bool>,
}

#[cfg(feature = "appearance-exerciser")]
impl AccessibilityPreviewOverride {
    fn apply(self, mut capabilities: CompositionCapabilities) -> CompositionCapabilities {
        capabilities.reduce_transparency = self
            .reduce_transparency
            .unwrap_or(capabilities.reduce_transparency);
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
            AccessibilityPreviewFact::ReduceTransparency => self.reduce_transparency = value,
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
    pub(crate) settings: UserSettings,
    platform: Rc<dyn AppearancePlatform>,
    fonts: AvailableFonts,
    progress_motion: spaceterm_ui::ProgressMotion,
    #[cfg(feature = "appearance-exerciser")]
    accessibility_preview: AccessibilityPreviewOverride,
    _tasks: Vec<Task<()>>,
    _observation: Option<Box<dyn SystemAppearanceSubscription>>,
}
impl Global for AppearanceRuntime {}

pub(crate) fn install(
    settings: UserSettings,
    changed: async_channel::Receiver<()>,
    platform: Rc<dyn AppearancePlatform>,
    cx: &mut App,
) -> Result<(), SettingsError> {
    let fonts = capture_fonts(cx);
    let observation = platform.observe();
    let mut tasks = vec![cx.spawn(async move |cx| {
        while changed.recv().await.is_ok() {
            while changed.try_recv().is_ok() {}
            if cx
                .update(|cx| {
                    let _ = refresh(cx);
                })
                .is_err()
            {
                break;
            }
        }
    })];
    let observation = observation.map(|observation| {
        tasks.push(cx.spawn(async move |cx| {
            while observation.changed.recv().await.is_ok() {
                while observation.changed.try_recv().is_ok() {}
                if cx
                    .update(|cx| {
                        let _ = refresh(cx);
                    })
                    .is_err()
                {
                    break;
                }
            }
        }));
        observation.subscription
    });
    cx.set_global(AppearanceRuntime {
        settings,
        platform,
        fonts,
        progress_motion: spaceterm_ui::ProgressMotion::Standard,
        #[cfg(feature = "appearance-exerciser")]
        accessibility_preview: AccessibilityPreviewOverride::default(),
        _tasks: tasks,
        _observation: observation,
    });
    refresh(cx)
}

pub(crate) fn refresh(cx: &mut App) -> Result<(), SettingsError> {
    let (platform, candidate, fonts, previous_progress_motion) = {
        let runtime = cx.global::<AppearanceRuntime>();
        (
            Rc::clone(&runtime.platform),
            runtime.settings.snapshot().candidate,
            runtime.fonts.clone(),
            runtime.progress_motion,
        )
    };
    let catalog = SchemeCatalog::from_custom_schemes(&candidate.custom_schemes)
        .map_err(|_| SettingsError::Invalid)?;
    let generation = cx
        .try_global::<InstalledAppearance>()
        .map_or(Some(AppearanceGeneration::INITIAL), |installed| {
            installed.0.generation.next()
        })
        .ok_or(SettingsError::RevisionExhausted)?;
    let accessibility = platform.accessibility_display_options();
    let capabilities = CompositionCapabilities {
        native_window_transparency: platform.supports_native_window_transparency(),
        reduce_transparency: accessibility.reduce_transparency,
        increase_contrast: accessibility.increase_contrast,
        show_borders: accessibility.show_borders,
        reduce_motion: platform.prefers_reduced_motion(),
        differentiate_without_color: accessibility.differentiate_without_color,
    };
    #[cfg(feature = "appearance-exerciser")]
    let capabilities = cx
        .global::<AppearanceRuntime>()
        .accessibility_preview
        .apply(capabilities);
    let resolved = catalog
        .resolve(
            generation,
            &candidate.preferences,
            SystemAppearance::from(platform.system_appearance()).with_composition(capabilities),
            &fonts,
        )
        .map_err(|_| SettingsError::Invalid)?;
    let progress_motion =
        resolved_progress_motion(resolved.chrome.composition.capabilities.reduce_motion);
    let changes = cx
        .try_global::<InstalledAppearance>()
        .map(|previous| AppearanceChangeSet::between(&previous.0, &resolved));
    let progress_motion_changed = previous_progress_motion != progress_motion;
    if !progress_motion_changed
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
    if chrome_changed || progress_motion_changed {
        let (prepared, inactive) = ChromeAppearance::prepare_variants(&resolved.chrome);
        let (settings_prepared, settings_inactive) =
            settings::prepare_variants(&resolved.chrome, prepared.clone(), inactive.clone());
        let controls = Box::new(
            super::control_theme_catalog::catalog(&prepared, progress_motion)
                .generation(spaceterm_ui::ControlThemeGeneration::new(generation.get())),
        );
        let inactive_controls = Box::new(
            super::control_theme_catalog::catalog(&inactive, progress_motion)
                .generation(spaceterm_ui::ControlThemeGeneration::new(generation.get())),
        );
        let settings_controls = Box::new(
            super::control_theme_catalog::catalog(&settings_prepared.chrome, progress_motion)
                .generation(spaceterm_ui::ControlThemeGeneration::new(generation.get())),
        );
        let settings_inactive_controls = Box::new(
            super::control_theme_catalog::catalog(&settings_inactive.chrome, progress_motion)
                .generation(spaceterm_ui::ControlThemeGeneration::new(generation.get())),
        );
        if cx.has_global::<InstalledAppearance>() {
            spaceterm_ui::replace_scoped_control_theme_catalogs(
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
    cx.global_mut::<AppearanceRuntime>().progress_motion = progress_motion;
    cx.set_global(InstalledAppearance(Arc::new(resolved)));
    Ok(())
}

fn resolved_progress_motion(reduced: bool) -> spaceterm_ui::ProgressMotion {
    if reduced {
        spaceterm_ui::ProgressMotion::Reduced
    } else {
        spaceterm_ui::ProgressMotion::Standard
    }
}

pub(crate) fn progress_motion(cx: &App) -> spaceterm_ui::ProgressMotion {
    cx.try_global::<AppearanceRuntime>()
        .map_or(spaceterm_ui::ProgressMotion::Standard, |runtime| {
            runtime.progress_motion
        })
}

#[cfg(feature = "appearance-exerciser")]
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

#[cfg(feature = "appearance-exerciser")]
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

/// Called only at startup or an explicit font reload. No frame or timer enumerates fonts.
fn capture_fonts(cx: &App) -> AvailableFonts {
    let text = cx.text_system();
    let installed = text
        .all_font_names()
        .into_iter()
        .map(|family| {
            let id = text.resolve_font(&font(family.clone()));
            let widths =
                ['i', 'M', '0', ' '].map(|character| text.advance(id, px(18.0), character));
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
        })
        .collect();
    AvailableFonts {
        system_ui: AvailableFont {
            family: ".SystemUIFont".into(),
            class: FontClass::Proportional,
            resolution_identity: "system-ui".into(),
        },
        system_monospace: AvailableFont {
            family: "Menlo".into(),
            class: FontClass::Monospace,
            resolution_identity: "system-monospace".into(),
        },
        installed,
    }
}

#[cfg(feature = "appearance-exerciser")]
pub(crate) fn reload_fonts(cx: &mut App) -> Result<(), SettingsError> {
    let fonts = capture_fonts(cx);
    cx.global_mut::<AppearanceRuntime>().fonts = fonts;
    refresh(cx)
}

/// The font availability captured at startup or at the last explicit font reload.
///
/// Settings presents only families the resolver can actually use, so an unavailable choice cannot
/// be made from the interface in the first place.
pub(crate) fn available_fonts(cx: &App) -> AvailableFonts {
    cx.try_global::<AppearanceRuntime>()
        .map(|runtime| runtime.fonts.clone())
        .unwrap_or_default()
}

pub(crate) fn current(cx: &App) -> Arc<ResolvedAppearance> {
    cx.try_global::<InstalledAppearance>()
        .map(|value| Arc::clone(&value.0))
        .unwrap_or_else(|| {
            Arc::new(
                SchemeCatalog::default()
                    .resolve(
                        AppearanceGeneration::INITIAL,
                        &Default::default(),
                        SystemAppearance::unavailable(),
                        &AvailableFonts {
                            system_ui: AvailableFont {
                                family: ".SystemUIFont".into(),
                                class: FontClass::Proportional,
                                resolution_identity: "system-ui".into(),
                            },
                            system_monospace: AvailableFont {
                                family: "Menlo".into(),
                                class: FontClass::Monospace,
                                resolution_identity: "system-monospace".into(),
                            },
                            installed: Vec::new(),
                        },
                    )
                    .expect("built-in appearance is valid"),
            )
        })
}

/// Which client titlebar height anchors one window's native traffic lights.
///
/// Workspace chrome absorbs the frame's top space while Settings chrome does not, so each
/// window keeps its own anchor against the same host geometry facts.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum TrafficLightChrome {
    Workspace,
    Settings,
}

/// Owns one Operating-System Window's native traffic-light position across density changes.
///
/// The native position is fixed at window open while Comfortable density grows the titlebar,
/// so stored buttons would otherwise ride high above centered Tabs and headings. Re-applying
/// the geometry-anchored position keeps their center aligned with the taller chrome. Repeat
/// applies with an unchanged position cost no native work.
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

    pub(crate) fn settings() -> Self {
        Self {
            role: TrafficLightChrome::Settings,
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
            TrafficLightChrome::Settings => {
                geometry.settings_traffic_light_position(appearance.top_height())
            }
        }
    }

    pub(crate) fn apply(&mut self, window: &gpui::Window, cx: &App) {
        let Some(desired) = self.desired_position(cx) else {
            return;
        };
        if self.applied != Some(desired) {
            window.set_traffic_light_position(desired);
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

/// What must sit behind this window's content.
///
/// The Chrome tone travels with the request because the material behind the window is what the
/// reader sees the desktop through, and Chrome only shows the desktop through a material its own
/// paint does not match. The tone is read from the compiled window root rather than from the
/// Light or Dark slot, so a definition filed under Light that paints a near-black root asks for
/// the material its own paint can show. Deciding that here keeps the choice one piece of product
/// policy rather than an assumption inside the platform Adapter.
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

/// A blurred window asks the framework for a transparent one.
///
/// SpaceTerm owns the blurred backdrop itself through `AppearancePlatform`, because GPUI's own
/// blurred background rewrites the native material's private layers and leaves the desktop
/// showing through unblurred. Asking for transparency is exactly the part of the framework's
/// behavior SpaceTerm still wants: a non-opaque window whose renderer composites straight alpha.
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
