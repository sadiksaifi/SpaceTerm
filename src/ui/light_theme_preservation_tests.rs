use crate::appearance::{
    Appearance, AppearanceGeneration, AppearanceMode, AvailableFonts, ChromeColorOverrides,
    ChromeScheme, Color, CompositionCapabilities, CustomScheme, SchemeCatalog, SchemeId,
    SchemeMetadata, SettingsDocument, SystemAppearance,
};

use super::appearance::{ChromeAppearance, settings};

#[gpui::test]
fn builtin_light_ordinary_control_edges_distinguish_hover_and_disabled(
    cx: &mut gpui::TestAppContext,
) {
    let mut preferences = crate::appearance::AppearancePreferences {
        mode: AppearanceMode::Light,
        ..Default::default()
    };
    preferences.background.transparency = 0.0;
    let resolved = SchemeCatalog::default()
        .resolve(
            AppearanceGeneration::INITIAL,
            &preferences,
            SystemAppearance::available(Appearance::Light),
            &AvailableFonts::default(),
        )
        .unwrap();
    let prepared = ChromeAppearance::prepare(&resolved.chrome);
    let catalog =
        super::control_theme_catalog::catalog(&prepared, spaceterm_ui::ProgressMotion::Standard);
    cx.update(|cx| spaceterm_ui::init(cx, catalog)).unwrap();
    let paints = cx.update(|cx| {
        cx.global::<spaceterm_ui::ButtonTheme>()
            .paints(spaceterm_ui::ButtonVariant::Secondary)
    });
    let edge_strength = |paint: spaceterm_ui::ButtonPaint| {
        let fill =
            Color::rgba(u32::from(paint.background())).source_over(prepared.colors.background);
        Color::rgba(u32::from(paint.border()))
            .source_over(fill)
            .contrast_ratio(fill)
    };
    assert!(
        edge_strength(paints.hovered()) > edge_strength(paints.normal()) + 0.15,
        "Light ordinary control hover should visibly strengthen its edge, not merely change the host underneath the same border"
    );
    assert!(
        edge_strength(paints.disabled()) < edge_strength(paints.normal()),
        "Light disabled controls should have quieter edges than enabled controls"
    );
}

#[gpui::test]
fn custom_light_keeps_authored_edges_and_surfaces_through_preparation(
    cx: &mut gpui::TestAppContext,
) {
    let root = Color::rgb(0xe7edf4);
    let sidebar = Color::rgb(0xc8d6e5);
    let card = Color::rgb(0xf4ead7);
    let pane_rim = Color::rgb(0x31516f);
    let row_rim = Color::rgb(0x925123);
    let shadow = Color::rgba(0x18344fdb);
    let scheme_id = SchemeId::new("test.custom-light-preservation").unwrap();
    let mut document = SettingsDocument::default();
    document.preferences.mode = AppearanceMode::Light;
    document.preferences.background.transparency = 0.0;
    document.preferences.background.blur = false;
    document.preferences.chrome.schemes.light = scheme_id.clone();
    document
        .custom_schemes
        .push(CustomScheme::Chrome(Box::new(ChromeScheme {
            window_background: None,
            id: scheme_id,
            name: "Custom Light Preservation".to_owned(),
            appearance: Appearance::Light,
            metadata: SchemeMetadata::default(),
            colors: ChromeColorOverrides {
                background: Some(root),
                panel_background: Some(sidebar),
                elevated_surface_background: Some(card),
                tab_separator: Some(pane_rim),
                row_selected_border: Some(row_rim),
                shadow: Some(shadow),
                ..ChromeColorOverrides::default()
            },
        })));
    document
        .validate()
        .expect("custom document should be valid");

    let resolved = SchemeCatalog::from_custom_schemes(&document.custom_schemes)
        .unwrap()
        .resolve(
            AppearanceGeneration::INITIAL,
            &document.preferences,
            SystemAppearance::available(Appearance::Light)
                .with_composition(CompositionCapabilities::new(true, true)),
            &AvailableFonts::default(),
        )
        .unwrap();
    let (active, inactive) = ChromeAppearance::prepare_variants(&resolved.chrome);
    let (settings_active, _) =
        settings::prepare_variants(&resolved.chrome, active.clone(), inactive);

    assert_eq!(active.pane_rim(), pane_rim);
    assert_eq!(active.panel_controls.reference.row_selected_border, row_rim);
    assert_eq!(
        active
            .floating_surfaces()
            .shell(spaceterm_ui::FloatingRole::Popover)
            .edge(),
        gpui::rgba(shadow.with_alpha(115).rgba_hex())
    );
    for (role, authored) in [
        (settings::SettingsSurfaceRole::Sidebar, sidebar),
        (settings::SettingsSurfaceRole::Canvas, root),
        (settings::SettingsSurfaceRole::Card, card),
    ] {
        let surface = settings_active.surface(role);
        assert_eq!(surface.semantic, authored);
        assert_eq!(surface.paint, authored);
        assert_eq!(surface.background, authored);
    }

    let catalog =
        super::control_theme_catalog::catalog(&active, spaceterm_ui::ProgressMotion::Standard);
    cx.update(|cx| spaceterm_ui::init(cx, catalog))
        .expect("custom control catalog should install");
    assert_eq!(
        cx.update(|cx| {
            cx.global::<spaceterm_ui::ButtonTheme>()
                .paints(spaceterm_ui::ButtonVariant::Secondary)
                .normal()
                .border()
        }),
        gpui::rgba(0x00000026)
    );
}
