//! Prepared final-host pairs for app-owned semantic text surfaces.

use crate::appearance::{ChromeColors, Color, SurfaceMaterials, SurfaceRole};

/// One painted background and the two text registers that rest directly on its final composite.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct PreparedTextPair {
    pub(crate) background: Color,
    pub(crate) primary: Color,
    pub(crate) secondary: Color,
}

/// Semantic pairs retained by one immutable prepared Chrome presentation.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct SemanticTextPairs {
    pub(crate) badge: PreparedTextPair,
    pub(crate) warning_status: PreparedTextPair,
    pub(crate) error_status: PreparedTextPair,
}

/// Resolves semantic text only after its material and actual host are known.
pub(crate) fn prepare_semantic_text_pairs(
    window: &ChromeColors,
    card: &ChromeColors,
    materials: SurfaceMaterials,
    window_host: Color,
    card_host: Color,
    increase_contrast: bool,
) -> SemanticTextPairs {
    let badge_background = materials.paint(
        SurfaceRole::Surface,
        card.elevated_surface_background,
        card.badge_background,
    );
    let status = |foreground, background| {
        prepare_pair(
            materials.paint(SurfaceRole::Surface, window.background, background),
            window_host,
            foreground,
            foreground,
            7.0,
            4.5,
            increase_contrast,
        )
    };
    SemanticTextPairs {
        badge: prepare_pair(
            badge_background,
            card_host,
            card.badge_foreground,
            card.badge_foreground,
            4.5,
            4.5,
            increase_contrast,
        ),
        warning_status: status(window.warning, window.warning_background),
        error_status: status(window.error, window.error_background),
    }
}

fn prepare_pair(
    background: Color,
    host: Color,
    primary: Color,
    secondary: Color,
    primary_floor: f64,
    secondary_floor: f64,
    increase_contrast: bool,
) -> PreparedTextPair {
    if !increase_contrast {
        return PreparedTextPair {
            background,
            primary,
            secondary,
        };
    }

    let composite = background.source_over(host);
    let prepared_composite = super::chrome_state::contrast_host(composite, primary_floor);
    PreparedTextPair {
        // Retain the authored material whenever it can carry the requested text floor. An opaque
        // safe fallback is necessary only when no black-or-white ink can reach that floor on the
        // material composite.
        background: if prepared_composite == composite {
            background
        } else {
            prepared_composite
        },
        primary: super::appearance::readable_on_background(
            primary,
            prepared_composite,
            primary_floor,
        ),
        secondary: super::appearance::readable_on_background(
            secondary,
            prepared_composite,
            secondary_floor,
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::appearance::{
        Appearance, AppearanceGeneration, AppearanceMode, AppearancePreferences, AvailableFonts,
        CompositionCapabilities, SchemeCatalog, SchemeId, SystemAppearance, parse_color_document,
    };
    use crate::ui::appearance::ChromeAppearance;

    fn prepare(increase_contrast: bool) -> (crate::appearance::ChromeColors, ChromeAppearance) {
        let document = parse_color_document(
            br##"{"schema_version":1,"schemes":[{"kind":"chrome","id":"test.semantic-pairs","name":"Semantic pairs","appearance":"light","colors":{"background":"#ffffff","elevated_surface_background":"#202020","badge_background":"#777777b3","badge_foreground":"#777777","error":"#777777","error_background":"#777777b3","warning":"#777777","warning_background":"#777777b3"}}]}"##,
        )
        .expect("semantic-pair fixture must parse");
        let catalog = SchemeCatalog::from_custom_schemes(&document.schemes)
            .expect("semantic-pair fixture must compile");
        let mut preferences = AppearancePreferences {
            mode: AppearanceMode::Light,
            ..Default::default()
        };
        preferences.background.transparency = 1.0;
        preferences.chrome.schemes.light = SchemeId::new("test.semantic-pairs").unwrap();
        let mut capabilities = CompositionCapabilities::new(true, true);
        capabilities.increase_contrast = increase_contrast;
        let resolved = catalog
            .resolve(
                AppearanceGeneration::INITIAL,
                &preferences,
                SystemAppearance::available(Appearance::Light).with_composition(capabilities),
                &AvailableFonts::default(),
            )
            .expect("semantic-pair fixture must resolve");
        let authored = resolved.chrome.colors.clone();
        let prepared = ChromeAppearance::prepare(&resolved.chrome);
        assert_eq!(
            resolved.chrome.colors, authored,
            "preparation must not mutate source roles"
        );
        (authored, prepared)
    }

    #[test]
    fn increase_contrast_prepares_badge_and_status_pairs_on_their_final_hosts() {
        let (authored, appearance) = prepare(true);
        assert_eq!(authored.badge_foreground, Color::rgb(0x777777));
        assert_eq!(authored.error, Color::rgb(0x777777));
        assert_eq!(authored.warning, Color::rgb(0x777777));

        let badge = appearance.semantic_text_pairs.badge;
        let card_host = appearance.control_host_background(spaceterm_ui::ControlHost::Card);
        let badge_background = badge.background.source_over(card_host);
        assert!(
            badge
                .primary
                .source_over(badge_background)
                .contrast_ratio(badge_background)
                >= 4.5,
            "Badge text must reach 4.5:1 on its final Card composite",
        );

        let window_host = appearance.control_host_background(spaceterm_ui::ControlHost::Window);
        for banner in [
            appearance.semantic_text_pairs.error_status,
            appearance.semantic_text_pairs.warning_status,
        ] {
            let background = banner.background.source_over(window_host);
            assert!(
                banner
                    .primary
                    .source_over(background)
                    .contrast_ratio(background)
                    >= 7.0,
                "Status Body text must reach 7:1 on its final Window composite",
            );
            assert!(
                banner
                    .secondary
                    .source_over(background)
                    .contrast_ratio(background)
                    >= 4.5,
                "Status Secondary text must reach 4.5:1 on its final Window composite",
            );
        }
    }

    #[test]
    fn ordinary_contrast_preserves_authored_material_pairs() {
        let (_, appearance) = prepare(false);
        let card = appearance.host_colors(spaceterm_ui::ControlHost::Card);
        assert_eq!(
            appearance.semantic_text_pairs.badge,
            PreparedTextPair {
                background: appearance.materials.paint(
                    SurfaceRole::Surface,
                    card.elevated_surface_background,
                    card.badge_background,
                ),
                primary: card.badge_foreground,
                secondary: card.badge_foreground,
            }
        );
        assert_eq!(
            appearance.semantic_text_pairs.error_status,
            PreparedTextPair {
                background: appearance
                    .surface(SurfaceRole::Surface, appearance.colors.error_background,),
                primary: appearance.colors.error,
                secondary: appearance.colors.error,
            }
        );
    }
}
