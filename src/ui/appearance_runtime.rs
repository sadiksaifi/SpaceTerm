//! Application-owned, event-driven appearance resolution and publication.

#[cfg(test)]
#[path = "appearance_runtime_tests.rs"]
mod tests;

use std::{rc::Rc, sync::Arc};

use gpui::{App, Global, Task, font, px};

use crate::appearance::{
    AppearanceChangeSet, AppearanceGeneration, AvailableFont, AvailableFonts, FontClass,
    ResolvedAppearance, SchemeCatalog, SystemAppearance,
};
use crate::platform::appearance::{AppearancePlatform, SystemAppearanceSubscription};
use crate::settings::{SettingsError, UserSettings};

use super::appearance::{ChromeAppearance, InstalledChrome};

#[derive(Clone)]
pub(crate) struct InstalledAppearance(pub(crate) Arc<ResolvedAppearance>);
impl Global for InstalledAppearance {}

pub(crate) struct AppearanceRuntime {
    pub(crate) settings: UserSettings,
    platform: Rc<dyn AppearancePlatform>,
    fonts: AvailableFonts,
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
        _tasks: tasks,
        _observation: observation,
    });
    refresh(cx)
}

pub(crate) fn refresh(cx: &mut App) -> Result<(), SettingsError> {
    let runtime = cx.global::<AppearanceRuntime>();
    let platform = Rc::clone(&runtime.platform);
    let candidate = runtime.settings.snapshot().candidate;
    let catalog = SchemeCatalog::from_custom_schemes(&candidate.custom_schemes)
        .map_err(|_| SettingsError::Invalid)?;
    let generation = cx
        .try_global::<InstalledAppearance>()
        .map_or(Some(AppearanceGeneration::INITIAL), |installed| {
            installed.0.generation.next()
        })
        .ok_or(SettingsError::RevisionExhausted)?;
    let resolved = catalog
        .resolve(
            generation,
            &candidate.preferences,
            SystemAppearance::from(runtime.platform.system_appearance()),
            &runtime.fonts,
        )
        .map_err(|_| SettingsError::Invalid)?;
    let changes = cx
        .try_global::<InstalledAppearance>()
        .map(|previous| AppearanceChangeSet::between(&previous.0, &resolved));
    if cx
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
        changes.chrome_colors || changes.chrome_typography || changes.chrome_metrics
    });
    if chrome_changed {
        let prepared = ChromeAppearance::prepare(&resolved.chrome);
        let controls = super::control_theme_catalog::catalog(&prepared)
            .generation(spaceterm_ui::ControlThemeGeneration::new(generation.get()));
        if cx.has_global::<InstalledAppearance>() {
            spaceterm_ui::replace_control_theme_catalog(cx, controls)
                .map_err(|_| SettingsError::Invalid)?;
        }
        platform.apply_native_appearance(resolved.chrome.appearance);
        cx.set_global(InstalledChrome(Arc::new(prepared)));
    }
    cx.set_global(InstalledAppearance(Arc::new(resolved)));
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
