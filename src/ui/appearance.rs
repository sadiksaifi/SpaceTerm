//! Prepared chrome presentation shared by app-owned composites and reusable controls.

use std::sync::Arc;

use gpui::{App, Font, FontWeight, Global, Pixels, TextRun, Window, font, px, rgba};

use crate::appearance::{
    Appearance, ChromeColors, ChromeDensity, Color, ResolvedChromeAppearance,
    ResolvedFontDescriptor, SurfaceMaterials, SurfaceRole,
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
    pub(crate) appearance: Appearance,
    /// Opaque presentation: the contrast reference and the input to Workspace surface owners.
    pub(crate) colors: ChromeColors,
    /// The same colors with the window's material applied to background fills, for controls.
    pub(crate) control_colors: ChromeColors,
    /// Controls resolved once against the raised floating host and its admitted backdrops.
    pub(crate) floating_colors: ChromeColors,
    /// Standard field content resolved against its own opaque frame on a floating host.
    pub(crate) floating_field_colors: ChromeColors,
    pub(crate) materials: SurfaceMaterials,
    pub(crate) floating_materials: SurfaceMaterials,
    pub(crate) floating_blur: bool,
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
        let floating_materials = SurfaceMaterials::OPAQUE;
        let (floating_raised_material, floating_raised_wash) = resolved_floating_material(
            Appearance::Dark,
            floating_materials,
            colors.background,
            colors.elevated_surface_background,
        );
        let (floating_readout_material, floating_readout_wash) = resolved_floating_material(
            Appearance::Dark,
            floating_materials,
            colors.background,
            colors.preview_background,
        );
        let floating_colors = floating_host_colors(
            authored.floating_presentation(),
            floating_raised_material,
            floating_raised_wash,
            floating_readout_material,
            floating_readout_wash,
        );
        let floating_field_colors = resolve_floating_field_colors(floating_colors.clone());
        Self {
            appearance: Appearance::Dark,
            control_colors: colors.clone(),
            floating_colors,
            floating_field_colors,
            colors,
            materials: SurfaceMaterials::OPAQUE,
            floating_materials,
            floating_blur: false,
            caption: regular.clone(),
            regular,
            heading: emphasis.clone(),
            emphasis,
            text_scale: 1.0,
            spacing_scale: 1.0,
        }
    }
}

/// Keeps the shared alpha while moving only the floating tint far enough for one neutral
/// foreground to read over every admitted backdrop.
fn floating_material(appearance: Appearance, material: Color, wash: Color) -> Option<Color> {
    let preferred = match appearance {
        Appearance::Light => (Color::rgb(0x000000), Color::rgb(0xffffff)),
        Appearance::Dark => (Color::rgb(0xffffff), Color::rgb(0x000000)),
    };
    let alternate = (preferred.1, preferred.0);
    for (foreground, endpoint) in [preferred, alternate] {
        let endpoint = endpoint.with_alpha(material.a);
        let readable = |candidate: Color| {
            [Color::rgb(0x000000), Color::rgb(0xffffff)]
                .into_iter()
                .all(|underlay| {
                    let background = wash.source_over(candidate.source_over(underlay));
                    foreground.contrast_ratio(background) >= 4.5
                })
        };
        if readable(material) {
            return Some(material);
        }
        if !readable(endpoint) {
            continue;
        }
        let mut lower = 0.0;
        let mut upper = 1.0;
        let mut result = endpoint;
        for _ in 0..16 {
            let amount = (lower + upper) / 2.0;
            let candidate = material.mix(endpoint, amount).with_alpha(material.a);
            if readable(candidate) {
                result = candidate;
                upper = amount;
            } else {
                lower = amount;
            }
        }
        return Some(result);
    }
    None
}

fn opaque_floating_fallback(appearance: Appearance, target: Color) -> Color {
    let target = target.with_alpha(255);
    floating_material(appearance, target, Color::rgba(0)).unwrap_or_else(|| match appearance {
        Appearance::Light => Color::rgb(0xffffff),
        Appearance::Dark => Color::rgb(0x000000),
    })
}

fn resolved_floating_material(
    appearance: Appearance,
    materials: SurfaceMaterials,
    base: Color,
    target: Color,
) -> (Color, Color) {
    let material = materials.paint(SurfaceRole::Floating, base, target);
    if materials.is_opaque() {
        return (
            Color::rgba(0),
            floating_material(appearance, material, Color::rgba(0))
                .unwrap_or_else(|| opaque_floating_fallback(appearance, target)),
        );
    }
    let wash = materials.floating_wash(base, target);
    if let Some(tone) = floating_material(appearance, material, wash) {
        (tone, wash)
    } else {
        (Color::rgba(0), opaque_floating_fallback(appearance, target))
    }
}

fn floating_host_colors(
    mut colors: ChromeColors,
    raised_material: Color,
    raised_wash: Color,
    readout_material: Color,
    readout_wash: Color,
) -> ChromeColors {
    let raised = |color| readable_on_material(color, raised_material, raised_wash, 4.5);
    let raised_disabled = |color| readable_on_material(color, raised_material, raised_wash, 3.0);
    let readout = |color| readable_on_material(color, readout_material, readout_wash, 4.5);
    colors.text = raised(colors.text);
    colors.text_secondary = raised(colors.text_secondary);
    colors.text_muted = raised(colors.text_muted);
    colors.text_placeholder = raised(colors.text_placeholder);
    colors.text_disabled = raised_disabled(colors.text_disabled);
    colors.icon = raised(colors.icon);
    colors.icon_muted = raised(colors.icon_muted);
    colors.icon_disabled = raised_disabled(colors.icon_disabled);
    colors.input_text = raised(colors.input_text);
    colors.input_placeholder = raised(colors.input_placeholder);
    colors.preview_foreground = readout(colors.preview_foreground);
    macro_rules! row_content {
        ($($field:ident),+ $(,)?) => {
            $(colors.$field = raised(colors.$field);)+
        };
    }
    row_content!(
        row_foreground,
        row_secondary,
        row_icon,
        row_match,
        row_hover_foreground,
        row_hover_secondary,
        row_hover_icon,
        row_hover_match,
        row_selected_foreground,
        row_selected_secondary,
        row_selected_icon,
        row_selected_match,
        row_selected_hover_foreground,
        row_selected_hover_secondary,
        row_selected_hover_icon,
        row_selected_hover_match,
    );
    colors
}

fn readable_on_material(
    proposed: Color,
    material: Color,
    wash: Color,
    minimum_contrast: f64,
) -> Color {
    let backgrounds = [
        wash.source_over(material.source_over(Color::rgb(0x000000))),
        wash.source_over(material.source_over(Color::rgb(0xffffff))),
    ];
    readable_on_backgrounds(proposed, backgrounds, minimum_contrast)
}

pub(super) fn readable_on_backgrounds(
    proposed: Color,
    backgrounds: [Color; 2],
    minimum_contrast: f64,
) -> Color {
    let minimum = |color: Color| {
        backgrounds
            .into_iter()
            .map(|background| color.source_over(background).contrast_ratio(background))
            .fold(f64::INFINITY, f64::min)
    };
    if minimum(proposed) >= minimum_contrast {
        return proposed;
    }
    let dark = Color::rgb(0x000000);
    let light = Color::rgb(0xffffff);
    let target = if minimum(dark) >= minimum(light) {
        dark
    } else {
        light
    };
    let mut lower = 0.0;
    let mut upper = 1.0;
    let mut readable = target;
    for _ in 0..16 {
        let amount = (lower + upper) / 2.0;
        let candidate = proposed.mix(target, amount);
        if minimum(candidate) >= minimum_contrast {
            readable = candidate;
            upper = amount;
        } else {
            lower = amount;
        }
    }
    readable
}

fn resolve_floating_field_colors(mut colors: ChromeColors) -> ChromeColors {
    colors.input_text = readable_on_background(colors.input_text, colors.input_background, 4.5);
    colors.input_placeholder =
        readable_on_background(colors.input_placeholder, colors.input_background, 4.5);
    colors.input_caret = readable_on_background(colors.input_caret, colors.input_background, 3.0);
    colors.input_disabled_text = readable_on_background(
        colors.input_disabled_text,
        colors.input_disabled_background,
        4.5,
    );
    let selection = colors
        .input_selection_background
        .source_over(colors.input_background);
    colors.input_selection_foreground =
        readable_on_background(colors.input_selection_foreground, selection, 4.5);
    colors
}

pub(super) fn readable_on_background(
    proposed: Color,
    background: Color,
    minimum_contrast: f64,
) -> Color {
    let rendered = proposed.source_over(background);
    if rendered.contrast_ratio(background) >= minimum_contrast {
        return rendered;
    }
    let dark = Color::rgb(0x000000);
    let light = Color::rgb(0xffffff);
    let target = if dark.contrast_ratio(background) >= light.contrast_ratio(background) {
        dark
    } else {
        light
    };
    let mut lower = 0.0;
    let mut upper = 1.0;
    let mut readable = target;
    for _ in 0..16 {
        let amount = (lower + upper) / 2.0;
        let candidate = rendered.mix(target, amount);
        if candidate.contrast_ratio(background) >= minimum_contrast {
            readable = candidate;
            upper = amount;
        } else {
            lower = amount;
        }
    }
    readable
}

impl ChromeAppearance {
    /// Applies the window's material for one surface role to an authored background color.
    ///
    /// Only the owner of a painted background calls this, once. Authored translucency is scaled
    /// rather than replaced, so a theme that authored a translucent surface keeps its intent.
    pub(crate) fn surface(&self, role: SurfaceRole, color: Color) -> Color {
        self.materials.paint(role, self.colors.background, color)
    }

    /// Returns the combined tone and wash used to evaluate content over a floating surface.
    pub(crate) fn floating_surface(&self, color: Color) -> Color {
        let (tone, wash) = resolved_floating_material(
            self.appearance,
            self.floating_materials,
            self.colors.background,
            color,
        );
        wash.source_over(tone)
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

        let paint = |color: Color| {
            let (tone, wash) = resolved_floating_material(
                self.appearance,
                self.floating_materials,
                self.colors.background,
                color,
            );
            FloatingSurfacePaint::new(
                rgba(wash.rgba_hex()),
                rgba(self.colors.border.rgba_hex()),
                rgba(self.colors.border_variant.rgba_hex()),
            )
            .backdrop_tone(rgba(tone.rgba_hex()))
        };
        FloatingSurfaceTheme::new(
            FloatingSurfacePaints::new(
                paint(self.colors.elevated_surface_background),
                paint(self.colors.preview_background),
            ),
            rgba(self.colors.shadow.rgba_hex()).into(),
            rgba(self.colors.modal_scrim.rgba_hex()),
        )
        .backdrop_alpha_limit(self.materials.floating_backdrop_alpha_limit())
        .backdrop_blur(if self.floating_blur {
            px(20.0)
        } else {
            px(0.0)
        })
    }

    pub(crate) fn prepare(resolved: &ResolvedChromeAppearance) -> Self {
        let colors = resolved.colors.opaque_presentation();
        let (floating_raised_material, floating_raised_wash) = resolved_floating_material(
            resolved.appearance,
            resolved.composition.floating_materials,
            colors.background,
            colors.elevated_surface_background,
        );
        let (floating_readout_material, floating_readout_wash) = resolved_floating_material(
            resolved.appearance,
            resolved.composition.floating_materials,
            colors.background,
            colors.preview_background,
        );
        let floating_colors = floating_host_colors(
            resolved.colors.floating_presentation(),
            floating_raised_material,
            floating_raised_wash,
            floating_readout_material,
            floating_readout_wash,
        );
        let floating_field_colors = resolve_floating_field_colors(floating_colors.clone());
        Self {
            appearance: resolved.appearance,
            control_colors: colors.material_presentation(resolved.composition.materials),
            floating_colors,
            floating_field_colors,
            colors,
            materials: resolved.composition.materials,
            floating_materials: resolved.composition.floating_materials,
            floating_blur: resolved.composition.floating_blur,
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
