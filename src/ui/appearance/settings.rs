//! Prepared surface hierarchy owned by the Settings Window.

use std::sync::Arc;

use gpui::{App, Global};

use crate::appearance::{
    Appearance, Color, ColorProvenance, ResolvedChromeAppearance, SurfaceRole,
};

use super::{
    ChromeAppearance, ChromeStatePolicy, FloatingContrastFloors, prepare_state_control_host,
};

const CANVAS_TRANSMISSION_SHARE: f32 = 0.5;
const CARD_TRANSMISSION_SHARE: f32 = 0.25;
const BUILT_IN_LIGHT_CARD_TRANSMISSION_SHARE: f32 = 0.10;
const SIDEBAR_CHANNEL_STEP: f64 = 5.0;

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

    /// Separates the built-in Light sidebar from the canvas, which shares its base color.
    pub(crate) fn sidebar_edge(&self) -> Option<Color> {
        self.chrome
            .built_in_light
            .then(|| self.chrome.surface_edge(spaceterm_ui::ControlHost::Panel))
    }

    /// Uses a quiet Light card edge and preserves the existing Dark/custom grouping boundary.
    pub(crate) fn card_edge(&self) -> Color {
        let host = self.card.background;
        if self.chrome.built_in_light {
            return self.chrome.surface_edge(spaceterm_ui::ControlHost::Card);
        }
        let prepared = self.separator(SettingsSurfaceRole::Card);
        if self.chrome.capabilities.increase_contrast {
            return prepared;
        }
        let ink = prepared.with_alpha(255);
        let within_ceiling = |candidate: Color| {
            candidate.source_over(host).contrast_ratio(host) <= super::separator::CONTRAST_CEILING
        };
        if within_ceiling(ink) {
            return ink;
        }
        let mut lower = u16::from(prepared.a);
        let mut upper = u16::from(u8::MAX);
        while lower + 1 < upper {
            let middle = (lower + upper) / 2;
            if within_ceiling(ink.with_alpha(middle as u8)) {
                lower = middle;
            } else {
                upper = middle;
            }
        }
        ink.with_alpha(lower as u8)
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
    let explicit_segmented_track = matches!(
        resolved.provenance.get("segmented_track_background"),
        Some(ColorProvenance::Authored | ColorProvenance::Overridden)
    );
    chrome.panel_controls = prepare_state_control_host(
        &settings_authored,
        (sidebar.semantic, sidebar.background),
        spaceterm_ui::ControlHost::Panel,
        chrome.materials,
        state,
        floors,
        explicit_segmented_track,
        chrome.built_in_light,
    );
    chrome.card_controls = prepare_state_control_host(
        &settings_authored,
        (card.semantic, card.background),
        spaceterm_ui::ControlHost::Card,
        chrome.materials,
        state,
        floors,
        explicit_segmented_track,
        chrome.built_in_light,
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
    // Built-in Light groups content with raised cards rather than another sidebar color.
    let sidebar_semantic = if chrome.built_in_light {
        chrome.colors.panel_background
    } else {
        settings_sidebar_rung(root, chrome.colors.panel_background, chrome.appearance)
    };
    let canvas_semantic = root;
    let card_semantic = chrome.colors.elevated_surface_background;
    let sidebar_materials = chrome.materials;
    let canvas_materials = chrome
        .materials
        .with_transmission_share(CANVAS_TRANSMISSION_SHARE);
    let card_materials = chrome
        .materials
        .with_transmission_share(if chrome.built_in_light {
            BUILT_IN_LIGHT_CARD_TRANSMISSION_SHARE
        } else {
            CARD_TRANSMISSION_SHARE
        });
    let sidebar_paint = sidebar_materials.paint(SurfaceRole::Sheet, root, sidebar_semantic);
    let canvas_paint = canvas_materials.paint(SurfaceRole::Sheet, root, canvas_semantic);
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

fn sidebar_rung(root: Color, appearance: Appearance) -> Color {
    let endpoint = match appearance {
        Appearance::Light => Color::rgb(0x000000),
        Appearance::Dark => Color::rgb(0xffffff),
    };
    let distance = [root.r, root.g, root.b]
        .into_iter()
        .zip([endpoint.r, endpoint.g, endpoint.b])
        .map(|(channel, endpoint)| channel.abs_diff(endpoint))
        .max()
        .unwrap_or_default();
    if distance == 0 {
        root
    } else {
        root.mix(
            endpoint,
            (SIDEBAR_CHANNEL_STEP / f64::from(distance)).min(1.0),
        )
    }
}

fn settings_sidebar_rung(root: Color, authored_panel: Color, appearance: Appearance) -> Color {
    if authored_panel == root {
        sidebar_rung(root, appearance)
    } else {
        authored_panel
    }
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
    use crate::appearance::{
        AppearanceGeneration, AppearanceMode, AppearancePreferences, AvailableFonts,
        CompositionCapabilities, SchemeCatalog, SystemAppearance,
    };

    #[test]
    fn sidebar_rung_moves_five_channels_in_the_appearance_direction() {
        assert_eq!(
            sidebar_rung(Color::rgb(0xe5e5e5), Appearance::Light),
            Color::rgb(0xe0e0e0)
        );
        assert_eq!(
            sidebar_rung(Color::rgb(0x151515), Appearance::Dark),
            Color::rgb(0x1a1a1a)
        );
    }

    #[test]
    fn distinct_custom_sidebar_color_is_not_replaced_by_the_builtin_rung() {
        let root = Color::rgb(0xe5e5e5);
        let authored_panel = Color::rgb(0x7a91b3);

        assert_eq!(
            settings_sidebar_rung(root, authored_panel, Appearance::Light),
            authored_panel
        );
    }

    #[test]
    fn built_in_light_card_keeps_its_content_step_at_full_transparency() {
        let mut preferences = AppearancePreferences {
            mode: AppearanceMode::Light,
            ..AppearancePreferences::default()
        };
        preferences.background.transparency = 1.0;
        let resolved = SchemeCatalog::default()
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
            card.background.contrast_ratio(canvas) >= 1.1,
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
            preferences.background.transparency = transparency;
            let resolved = SchemeCatalog::default()
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
