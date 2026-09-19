//! Prepared chrome presentation shared by app-owned composites and reusable controls.

use std::sync::Arc;

use gpui::{App, Font, FontWeight, Global, Pixels, TextRun, Window, font, px, rgba};

use crate::appearance::{
    ChromeColors, ChromeDensity, Color, ResolvedChromeAppearance, ResolvedFontDescriptor,
    SurfaceMaterials, SurfaceRole,
};

pub(crate) fn prepared_font(descriptor: &ResolvedFontDescriptor) -> Font {
    let mut result = font(descriptor.primary_family.clone());
    result.weight = FontWeight(f32::from(descriptor.weight));
    result.style = match descriptor.style {
        crate::appearance::FontStyle::Normal => gpui::FontStyle::Normal,
        crate::appearance::FontStyle::Italic => gpui::FontStyle::Italic,
    };
    result.fallbacks = Some(gpui::FontFallbacks::from_fonts(
        descriptor.fallback_families.clone(),
    ));
    result.features = gpui::FontFeatures(Arc::new(
        descriptor
            .features
            .iter()
            .map(|feature| {
                if let Some(tag) = feature.strip_prefix('-') {
                    (tag.to_owned(), 0)
                } else {
                    (feature.trim_start_matches('+').to_owned(), 1)
                }
            })
            .collect(),
    ));
    result
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TerminalFonts {
    pub(crate) resolution_identity: String,
    pub(crate) regular: Font,
    pub(crate) bold: Font,
    pub(crate) italic: Font,
    pub(crate) bold_italic: Font,
}

impl TerminalFonts {
    pub(crate) fn prepare(typography: &crate::appearance::ResolvedTerminalTypography) -> Self {
        Self {
            resolution_identity: typography.regular.resolution_identity.clone(),
            regular: prepared_font(&typography.regular),
            bold: prepared_font(&typography.bold),
            italic: prepared_font(&typography.italic),
            bold_italic: prepared_font(&typography.bold_italic),
        }
    }

    pub(crate) fn cell(&self, bold: bool, italic: bool) -> &Font {
        match (bold, italic) {
            (false, false) => &self.regular,
            (true, false) => &self.bold,
            (false, true) => &self.italic,
            (true, true) => &self.bold_italic,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ChromeAppearance {
    /// Opaque presentation: the contrast reference and the input to Workspace surface owners.
    pub(crate) colors: ChromeColors,
    /// The same colors with the window's material applied to background fills, for controls.
    pub(crate) control_colors: ChromeColors,
    /// Controls resolved from authored RGBA against the raised floating host.
    pub(crate) floating_colors: ChromeColors,
    pub(crate) materials: SurfaceMaterials,
    pub(crate) regular: Font,
    pub(crate) emphasis: Font,
    pub(crate) caption: Font,
    pub(crate) heading: Font,
    pub(crate) text_scale: f32,
    pub(crate) spacing_scale: f32,
}

impl Default for ChromeAppearance {
    fn default() -> Self {
        let regular = font(".SystemUIFont");
        let mut emphasis = regular.clone();
        emphasis.weight = FontWeight::SEMIBOLD;
        let authored = ChromeColors::default();
        let colors = authored.opaque_presentation();
        Self {
            control_colors: colors.clone(),
            floating_colors: authored.floating_presentation(),
            colors,
            materials: SurfaceMaterials::OPAQUE,
            caption: regular.clone(),
            regular,
            heading: emphasis.clone(),
            emphasis,
            text_scale: 1.0,
            spacing_scale: 1.0,
        }
    }
}

impl ChromeAppearance {
    /// Applies the window's material for one surface role to an authored background color.
    ///
    /// Only the owner of a painted background calls this, once. Authored translucency is scaled
    /// rather than replaced, so a theme that authored a translucent surface keeps its intent.
    pub(crate) fn surface(&self, role: SurfaceRole, color: Color) -> Color {
        self.materials.paint(role, self.colors.background, color)
    }

    /// The backdrop a Pane paints beneath its Terminal.
    ///
    /// Presentation only: protocol colors and explicit cell backgrounds keep their own values. A
    /// translucent window lifts the default backdrop toward the scheme's elevated surface so it
    /// separates from the base; an opaque window paints it as is. Dark also retains a backing
    /// beneath that tint so desktop colors do not wash out Terminal text.
    pub(crate) fn pane_surface(&self, terminal_background: Color) -> Color {
        self.materials.pane_surface(
            self.colors.background,
            self.colors.elevated_surface_background,
            terminal_background,
        )
    }

    /// The hairline around every Pane: the same neutral edge a selected Workspace row or Active Tab
    /// chip carries, at every transparency.
    pub(crate) fn pane_rim(&self) -> Color {
        self.colors.row_selected_border
    }

    /// Resolves shared floating paints; the control catalog applies density once on installation.
    pub(crate) fn floating_surfaces(&self) -> spaceterm_ui::FloatingSurfaceTheme {
        use spaceterm_ui::{FloatingSurfacePaint, FloatingSurfacePaints, FloatingSurfaceTheme};

        let paint = |color| {
            FloatingSurfacePaint::new(
                rgba(self.surface(SurfaceRole::Floating, color).rgba_hex()),
                rgba(self.colors.border.rgba_hex()),
                rgba(self.colors.border_variant.rgba_hex()),
            )
        };
        FloatingSurfaceTheme::new(
            FloatingSurfacePaints::new(
                paint(self.colors.elevated_surface_background),
                paint(self.colors.preview_background),
            ),
            rgba(self.colors.shadow.rgba_hex()).into(),
            rgba(self.colors.modal_scrim.rgba_hex()),
        )
    }

    pub(crate) fn prepare(resolved: &ResolvedChromeAppearance) -> Self {
        let colors = resolved.colors.opaque_presentation();
        Self {
            control_colors: colors.material_presentation(resolved.composition.materials),
            floating_colors: resolved.colors.floating_presentation(),
            colors,
            materials: resolved.composition.materials,
            regular: prepared_font(&resolved.typography.body),
            emphasis: prepared_font(&resolved.typography.navigation),
            caption: prepared_font(&resolved.typography.caption),
            heading: prepared_font(&resolved.typography.heading),
            text_scale: resolved.typography.body.size / 13.0,
            spacing_scale: Self::density_spacing_scale(resolved.density),
        }
    }

    /// The factor every density-scaled Chrome length is multiplied by.
    pub(crate) fn density_spacing_scale(density: ChromeDensity) -> f32 {
        match density {
            ChromeDensity::Compact => 1.0,
            ChromeDensity::Comfortable => 1.25,
        }
    }
    pub(crate) fn text_size(&self, baseline: f32) -> Pixels {
        px(baseline * self.text_scale)
    }

    /// The body font with tabular figures, for a readout whose value changes in place.
    ///
    /// Proportional digits give a numeric readout a different width for every value it shows, so
    /// stepping through one shifts the text under the pointer. The feature is added to whatever
    /// the resolved font already asks for rather than replacing it, and a family without tabular
    /// figures simply ignores it.
    pub(crate) fn tabular(&self) -> Font {
        let mut font = self.regular.clone();
        let mut features = font.features.tag_value_list().to_vec();
        if !features.iter().any(|(tag, _)| tag == "tnum") {
            features.push(("tnum".to_owned(), 1));
        }
        font.features = gpui::FontFeatures(Arc::new(features));
        font
    }
    pub(crate) fn spacing(&self, baseline: f32) -> Pixels {
        px(baseline * self.spacing_scale)
    }

    /// Keep the original line box and add only the extra text and density space.
    pub(crate) fn height(&self, baseline: f32, text: f32) -> Pixels {
        let line = text * 1.4;
        px(
            (line * self.text_scale + (baseline - line).max(0.0) * self.spacing_scale)
                .max(baseline),
        )
    }

    pub(crate) fn top_height(&self) -> Pixels {
        self.height(36.0, 12.0)
    }

    pub(crate) fn caption_height(&self) -> Pixels {
        self.height(32.0, 12.65)
    }

    pub(crate) fn measure(&self, value: &str, baseline: f32, window: &Window) -> Pixels {
        self.measure_font(value, baseline, &self.regular, window)
    }

    pub(crate) fn measure_emphasis(&self, value: &str, baseline: f32, window: &Window) -> Pixels {
        self.measure_font(value, baseline, &self.emphasis, window)
    }

    fn measure_font(&self, value: &str, baseline: f32, font: &Font, window: &Window) -> Pixels {
        window
            .text_system()
            .shape_line(
                value.to_owned().into(),
                self.text_size(baseline),
                &[TextRun {
                    len: value.len(),
                    font: font.clone(),
                    color: rgba(self.colors.text.rgba_hex()).into(),
                    background_color: None,
                    underline: None,
                    strikethrough: None,
                }],
                None,
            )
            .width
    }
}

#[cfg(test)]
mod typography_tests {
    use super::ChromeAppearance;

    #[test]
    fn prepared_field_surfaces_match_compiler_backing_without_flattening_authored_intent() {
        use crate::appearance::*;
        let document = parse_color_document(br##"{"schema_version":1,"schemes":[{"kind":"chrome","id":"test.translucent","name":"Translucent","appearance":"light","colors":{"background":"#ffffff80","panel_background":"#00000040","input_background":"#00000000"}}]}"##).unwrap();
        let catalog = SchemeCatalog::from_custom_schemes(&document.schemes).unwrap();
        let mut preferences = AppearancePreferences {
            mode: AppearanceMode::Light,
            ..Default::default()
        };
        preferences.chrome.schemes.light = SchemeId::new("test.translucent").unwrap();
        let resolved = catalog
            .resolve(
                AppearanceGeneration::INITIAL,
                &preferences,
                SystemAppearance::unavailable(),
                &AvailableFonts::default(),
            )
            .unwrap();
        let prepared = ChromeAppearance::prepare(&resolved.chrome);
        assert_eq!(resolved.chrome.colors.background.a, 128);
        assert_eq!(resolved.chrome.colors.input_background.a, 0);
        assert_eq!(prepared.colors.background, Color::rgb(0xffffff));
        assert_eq!(
            prepared.colors.input_background,
            prepared.colors.panel_background
        );
        assert_eq!(prepared.colors.input_background.a, 255);
        assert!(
            prepared
                .colors
                .input_text
                .source_over(prepared.colors.input_background)
                .contrast_ratio(prepared.colors.input_background)
                >= 4.5
        );
    }

    /// A readout asking for tabular figures keeps whatever the resolved font already asked for.
    #[test]
    fn tabular_figures_join_the_resolved_features_rather_than_replacing_them() {
        let mut appearance = ChromeAppearance::default();
        appearance.regular.features =
            gpui::FontFeatures(std::sync::Arc::new(vec![("calt".to_owned(), 0)]));

        let features = appearance.tabular().features.tag_value_list().to_vec();

        assert!(features.contains(&("calt".to_owned(), 0)));
        assert!(features.contains(&("tnum".to_owned(), 1)));
        assert_eq!(
            appearance.tabular().features.tag_value_list().len(),
            features.len(),
            "asking twice should not stack the feature"
        );
    }
}

#[derive(Clone)]
pub(crate) struct InstalledChrome(pub(crate) Arc<ChromeAppearance>);
impl Global for InstalledChrome {}

pub(crate) fn chrome(cx: &App) -> &ChromeAppearance {
    &cx.global::<InstalledChrome>().0
}

pub(crate) fn initialize(cx: &mut App) {
    if !cx.has_global::<InstalledChrome>() {
        cx.set_global(InstalledChrome(Arc::new(ChromeAppearance::default())));
    }
}
