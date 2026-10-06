//! Prepared surface hierarchy owned by the Settings Window.

use std::sync::Arc;

use gpui::{App, Global};

use crate::appearance::{Appearance, Color, ResolvedChromeAppearance, SurfaceRole};

use super::{
    ChromeAppearance, ChromeStatePolicy, FloatingContrastFloors, prepare_state_control_host,
};

const CARD_TRANSMISSION_SHARE: f32 = 0.25;
const BUILT_IN_LIGHT_CARD_TRANSMISSION_SHARE: f32 = 0.10;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SettingsSurfaceRole {
    Sidebar,
    Canvas,
    Card,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct PreparedSettingsSurface {
    pub(crate) semantic: Color,
    pub(crate) paint: Color,
    pub(crate) background: Color,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct SettingsHostBackgrounds {
    window: Color,
    panel: Color,
    card: Color,
}

impl SettingsHostBackgrounds {
    pub(super) fn background(self, host: spaceterm_ui::ControlHost) -> Option<Color> {
        match host {
            spaceterm_ui::ControlHost::Window => Some(self.window),
            spaceterm_ui::ControlHost::Panel => Some(self.panel),
            spaceterm_ui::ControlHost::Card => Some(self.card),
            spaceterm_ui::ControlHost::TitleBar | spaceterm_ui::ControlHost::Floating => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct SettingsAppearance {
    pub(crate) chrome: Arc<ChromeAppearance>,
    sidebar: PreparedSettingsSurface,
    canvas: PreparedSettingsSurface,
    card: PreparedSettingsSurface,
}

#[derive(Clone)]
pub(crate) struct InstalledSettingsChrome {
    pub(crate) active: Arc<SettingsAppearance>,
    pub(crate) inactive: Arc<SettingsAppearance>,
}

impl Global for InstalledSettingsChrome {}

impl InstalledSettingsChrome {
    pub(crate) fn single(chrome: ChromeAppearance) -> Self {
        let appearance = Arc::new(SettingsAppearance::fallback(chrome));
        Self {
            active: Arc::clone(&appearance),
            inactive: appearance,
        }
    }
}

impl SettingsAppearance {
    pub(crate) fn fallback(chrome: ChromeAppearance) -> Self {
        let root = chrome.colors.background;
        let canvas_paint = chrome.materials.paint(SurfaceRole::Sheet, root, root);
        let canvas = PreparedSettingsSurface {
            semantic: root,
            paint: canvas_paint,
            background: chrome.control_host_background(spaceterm_ui::ControlHost::Window),
        };
        let sidebar = PreparedSettingsSurface {
            semantic: chrome.colors.panel_background,
            paint: chrome
                .materials
                .paint(SurfaceRole::Base, root, chrome.colors.panel_background),
            background: chrome.control_host_background(spaceterm_ui::ControlHost::Panel),
        };
        let card = PreparedSettingsSurface {
            semantic: chrome.colors.elevated_surface_background,
            paint: chrome.materials.paint(
                SurfaceRole::Surface,
                root,
                chrome.colors.elevated_surface_background,
            ),
            background: chrome.control_host_background(spaceterm_ui::ControlHost::Card),
        };
        Self {
            chrome: Arc::new(chrome),
            sidebar,
            canvas,
            card,
        }
    }

    pub(crate) fn surface(&self, role: SettingsSurfaceRole) -> PreparedSettingsSurface {
        match role {
            SettingsSurfaceRole::Sidebar => self.sidebar,
            SettingsSurfaceRole::Canvas => self.canvas,
            SettingsSurfaceRole::Card => self.card,
        }
    }

    pub(crate) fn separator(&self, role: SettingsSurfaceRole) -> Color {
        let host = match role {
            SettingsSurfaceRole::Sidebar => spaceterm_ui::ControlHost::Panel,
            SettingsSurfaceRole::Canvas => spaceterm_ui::ControlHost::Window,
            SettingsSurfaceRole::Card => spaceterm_ui::ControlHost::Card,
        };
        self.chrome.separator(host)
    }

    /// Accessibility can reinforce the otherwise fill-separated built-in sidebar and canvas.
    pub(crate) fn sidebar_edge(&self) -> Option<Color> {
        (self.chrome.capabilities.increase_contrast || self.chrome.capabilities.show_borders)
            .then(|| self.chrome.surface_edge(spaceterm_ui::ControlHost::Panel))
    }

    /// Groups separate by fill; accessibility adds boundaries.
    pub(crate) fn card_edge(&self) -> Color {
        if self.chrome.capabilities.increase_contrast || self.chrome.capabilities.show_borders {
            self.chrome.surface_edge(spaceterm_ui::ControlHost::Card)
        } else {
            Color::rgba(0)
        }
    }
}

pub(crate) fn prepare_variants(
    resolved: &ResolvedChromeAppearance,
    active: ChromeAppearance,
    inactive: ChromeAppearance,
) -> (SettingsAppearance, SettingsAppearance) {
    let mut active = prepare_variant(resolved, active);
    let mut inactive = prepare_variant(resolved, inactive);
    super::built_in_light::prepare_active_segmented_controls(
        Arc::make_mut(&mut active.chrome),
        resolved,
    );
    super::disabled_union::reconcile(
        Arc::make_mut(&mut active.chrome),
        Arc::make_mut(&mut inactive.chrome),
        &resolved.colors,
    );
    super::built_in_light::finalize_inactive_segmented_controls(Arc::make_mut(
        &mut inactive.chrome,
    ));
    let unfocused = super::collection_selection::prepare(&active.chrome, &inactive.chrome);
    Arc::make_mut(&mut active.chrome).unfocused_selection = unfocused;
    (active, inactive)
}

fn prepare_variant(
    resolved: &ResolvedChromeAppearance,
    mut chrome: ChromeAppearance,
) -> SettingsAppearance {
    let active = chrome.active;
    let capabilities = chrome.capabilities;
    let state = ChromeStatePolicy {
        active,
        capabilities,
    };
    let authored = state.surfaces(&resolved.colors);
    let mut settings_authored = authored.clone();
    let (sidebar, canvas, card) = prepare_surfaces(&chrome);
    settings_authored.panel_background = sidebar.semantic;
    settings_authored.elevated_surface_background = card.semantic;
    chrome.colors.panel_background = sidebar.semantic;
    chrome.colors.elevated_surface_background = card.semantic;
    chrome.settings_hosts = Some(SettingsHostBackgrounds {
        window: canvas.background,
        panel: sidebar.background,
        card: card.background,
    });

    let floors = FloatingContrastFloors {
        secondary: if active || capabilities.increase_contrast {
            FloatingContrastFloors::for_increase_contrast(capabilities.increase_contrast).secondary
        } else {
            3.0
        },
        interactive: active,
        ..FloatingContrastFloors::for_increase_contrast(capabilities.increase_contrast)
    };
    if chrome.appearance == Appearance::Light {
        // The Settings content column is the same light canvas as the Workspace content,
        // not the muted root beneath navigation. Resolve its controls on the painted host.
        let mut canvas_authored = settings_authored.clone();
        canvas_authored.background = canvas.semantic;
        let window = prepare_state_control_host(
            &canvas_authored,
            (canvas.semantic, canvas.background),
            spaceterm_ui::ControlHost::Window,
            chrome.materials,
            state,
            floors,
            true,
        );
        chrome.colors = super::with_final_host_content(chrome.colors, &window.colors);
        chrome.control_colors = window.colors;
        chrome.segmented_control_colors = window.segmented;
        chrome.window_control_fallbacks = window.fallback_families;
    }
    let mut panel_authored = settings_authored.clone();
    if chrome.appearance == Appearance::Dark {
        // Navigation states belong to the sidebar, not the darker definition root.
        // Rehost their existing contrast steps before preparing text and material paints.
        for fill in [
            &mut panel_authored.row_background,
            &mut panel_authored.row_hover_background,
            &mut panel_authored.row_selected_background,
            &mut panel_authored.row_selected_hover_background,
            &mut panel_authored.navigation_selected_background,
        ] {
            if let Some(rehosted) =
                super::host_relative_fill(*fill, authored.panel_background, sidebar.semantic)
            {
                *fill = rehosted;
            }
        }
    }
    chrome.panel_controls = prepare_state_control_host(
        &panel_authored,
        (sidebar.semantic, sidebar.background),
        spaceterm_ui::ControlHost::Panel,
        chrome.materials,
        state,
        floors,
        chrome.appearance == Appearance::Light,
    );
    chrome.card_controls = prepare_state_control_host(
        &settings_authored,
        (card.semantic, card.background),
        spaceterm_ui::ControlHost::Card,
        chrome.materials,
        state,
        floors,
        chrome.appearance == Appearance::Light,
    );
    chrome.semantic_text_pairs = super::super::chrome_semantic_pairs::prepare_semantic_text_pairs(
        &chrome.colors,
        &chrome.card_controls.reference,
        chrome.materials,
        canvas.background,
        card.background,
        capabilities.increase_contrast,
    );
    chrome.unfocused_selection = super::collection_selection::PreparedCollectionSelection::identity(
        &chrome.colors,
        &chrome.title_bar_controls.reference,
        &chrome.panel_controls.reference,
        &chrome.card_controls.reference,
        &chrome.floating_colors,
    );

    SettingsAppearance {
        chrome: Arc::new(chrome),
        sidebar,
        canvas,
        card,
    }
}

fn prepare_surfaces(
    chrome: &ChromeAppearance,
) -> (
    PreparedSettingsSurface,
    PreparedSettingsSurface,
    PreparedSettingsSurface,
) {
    let root = chrome.colors.background;
    let light_content_hierarchy = chrome.appearance == Appearance::Light;
    let sidebar_semantic = if chrome.appearance == Appearance::Light {
        chrome.colors.panel_background
    } else {
        root.mix(Color::WHITE, 0.10)
    };
    let elevated = chrome.colors.elevated_surface_background;
    // A Light navigation rests on the window root, so its canvas takes the rung between root and
    // group.
    let canvas_semantic = if light_content_hierarchy {
        root.mix(elevated, 0.5)
    } else {
        root
    };
    let card_semantic = elevated;
    let sidebar_materials = chrome.materials;
    let card_materials =
        chrome
            .materials
            .with_transmission_share(if chrome.appearance == Appearance::Light {
                BUILT_IN_LIGHT_CARD_TRANSMISSION_SHARE
            } else {
                CARD_TRANSMISSION_SHARE
            });
    let sidebar_paint = sidebar_materials.paint(SurfaceRole::Sheet, root, sidebar_semantic);
    let canvas_paint = if light_content_hierarchy {
        // A bright canvas holds its rung so the navigation column does not merge into the page.
        super::prominent_surface_with(chrome.materials, root, canvas_semantic)
    } else {
        // This column is a window backing, not a nested card. Keep the same requested
        // transmission as the Workspace sheet instead of halving it for Settings.
        chrome
            .materials
            .paint(SurfaceRole::Sheet, root, canvas_semantic)
    };
    let sidebar = PreparedSettingsSurface {
        semantic: sidebar_semantic,
        paint: sidebar_paint,
        background: sidebar_paint.source_over(root),
    };
    let canvas = PreparedSettingsSurface {
        semantic: canvas_semantic,
        paint: canvas_paint,
        background: canvas_paint.source_over(root),
    };
    let card_paint = card_materials.paint(SurfaceRole::Surface, canvas_semantic, card_semantic);
    let card = PreparedSettingsSurface {
        semantic: card_semantic,
        paint: card_paint,
        background: card_paint.source_over(canvas.background),
    };
    (sidebar, canvas, card)
}

pub(crate) fn selected(cx: &App) -> &Arc<SettingsAppearance> {
    let installed = cx.global::<InstalledSettingsChrome>();
    match spaceterm_ui::ControlWindowActivity::current() {
        spaceterm_ui::ControlWindowActivity::Active => &installed.active,
        spaceterm_ui::ControlWindowActivity::Inactive => &installed.inactive,
    }
}

pub(crate) fn shared(cx: &App) -> Arc<SettingsAppearance> {
    Arc::clone(selected(cx))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::appearance::Appearance;
    use crate::appearance::{
        AppearanceGeneration, AppearanceMode, AppearancePreferences, AvailableFonts,
        CompositionCapabilities, SystemAppearance, ThemeCatalog,
    };

    #[test]
    fn built_in_dark_groups_use_fill_separation_and_accessible_edges() {
        for transparency in [0.0, 0.35, 1.0] {
            for (increase_contrast, show_borders) in [(false, false), (true, false), (false, true)]
            {
                let mut preferences = AppearancePreferences {
                    mode: AppearanceMode::Dark,
                    ..AppearancePreferences::default()
                };
                preferences.window.transparency = transparency;
                let mut resolved = ThemeCatalog::default()
                    .resolve(
                        AppearanceGeneration::INITIAL,
                        &preferences,
                        SystemAppearance::available(Appearance::Dark)
                            .with_composition(CompositionCapabilities::new(true, true)),
                        &AvailableFonts::default(),
                    )
                    .expect("built-in Dark should resolve");
                let capabilities =
                    &mut Arc::make_mut(&mut resolved.chrome).composition.capabilities;
                capabilities.increase_contrast = increase_contrast;
                capabilities.show_borders = show_borders;
                let (active, inactive) = ChromeAppearance::prepare_variants(&resolved.chrome);
                let (active, inactive) = prepare_variants(&resolved.chrome, active, inactive);
                for settings in [active, inactive] {
                    let canvas = settings.surface(SettingsSurfaceRole::Canvas).background;
                    let card = settings.surface(SettingsSurfaceRole::Card).background;
                    assert!(card.r > canvas.r, "Dark groups must separate through fill");
                    if increase_contrast || show_borders {
                        assert!(settings.card_edge().source_over(card).contrast_ratio(card) >= 3.0);
                        let sidebar = settings.surface(SettingsSurfaceRole::Sidebar).background;
                        assert!(
                            settings
                                .sidebar_edge()
                                .unwrap()
                                .source_over(sidebar)
                                .contrast_ratio(sidebar)
                                >= 3.0
                        );
                    } else {
                        assert_eq!(
                            settings.card_edge().a,
                            0,
                            "ordinary Dark groups need no outline"
                        );
                        assert!(settings.sidebar_edge().is_none());
                        let rule = settings.separator(SettingsSurfaceRole::Card);
                        assert!(
                            (1.15..=1.35).contains(&rule.source_over(card).contrast_ratio(card))
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn built_in_light_card_keeps_its_content_step_at_full_transparency() {
        let mut preferences = AppearancePreferences {
            mode: AppearanceMode::Light,
            ..AppearancePreferences::default()
        };
        preferences.window.transparency = 1.0;
        let resolved = ThemeCatalog::default()
            .resolve(
                AppearanceGeneration::INITIAL,
                &preferences,
                SystemAppearance::available(Appearance::Light)
                    .with_composition(CompositionCapabilities::new(true, true)),
                &AvailableFonts::default(),
            )
            .expect("built-in Light should resolve");
        let (active, inactive) = ChromeAppearance::prepare_variants(&resolved.chrome);
        let (settings, _) = prepare_variants(&resolved.chrome, active, inactive);
        let canvas = settings.surface(SettingsSurfaceRole::Canvas).background;
        let card = settings.surface(SettingsSurfaceRole::Card);

        assert!(card.paint.a < u8::MAX, "the card must keep transmitting");
        assert!(
            card.background.r > canvas.r && card.background.contrast_ratio(canvas) >= 1.05,
            "the card must retain its content-tone step over the canvas"
        );
    }

    #[test]
    fn built_in_light_inactive_settings_segments_suppress_hover_after_reconciliation() {
        for transparency in [0.0, 0.35, 1.0] {
            let mut preferences = AppearancePreferences {
                mode: AppearanceMode::Light,
                ..AppearancePreferences::default()
            };
            preferences.window.transparency = transparency;
            let resolved = ThemeCatalog::default()
                .resolve(
                    AppearanceGeneration::INITIAL,
                    &preferences,
                    SystemAppearance::available(Appearance::Light)
                        .with_composition(CompositionCapabilities::new(true, true)),
                    &AvailableFonts::default(),
                )
                .expect("built-in Light should resolve");
            let (active, inactive) = ChromeAppearance::prepare_variants(&resolved.chrome);
            let (_, settings) = prepare_variants(&resolved.chrome, active, inactive);

            for (name, host, colors) in [
                (
                    "Panel",
                    spaceterm_ui::ControlHost::Panel,
                    &settings.chrome.panel_controls.segmented,
                ),
                (
                    "Card",
                    spaceterm_ui::ControlHost::Card,
                    &settings.chrome.card_controls.segmented,
                ),
            ] {
                let host = settings.chrome.control_host_background(host);
                let track = colors.element_background.source_over(host);
                let selected = colors.selection_background.source_over(track);
                let hovered = colors.selection_hover_background.source_over(track);
                assert!(
                    selected.r > track.r,
                    "inactive Light Settings {name} segment at {transparency} must stay raised over {track:?}"
                );
                assert_eq!(
                    hovered, selected,
                    "inactive Light Settings {name} segment at {transparency} must suppress hover"
                );
            }
        }
    }
}
