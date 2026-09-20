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
    /// Segmented option fills compiled against their immediate track host.
    pub(crate) segmented_control_colors: ChromeColors,
    /// Control presentation for a Panel host.
    pub(crate) panel_controls: PreparedControlHost,
    /// Control presentation for a Card host.
    pub(crate) card_controls: PreparedControlHost,
    /// Opaque semantic and contrast reference for controls on the raised floating host.
    pub(crate) floating_colors: ChromeColors,
    /// Floating control fills expressed as overlays on the raised host.
    pub(crate) floating_control_colors: ChromeColors,
    /// Floating segmented option fills compiled against their material track.
    pub(crate) floating_segmented_colors: ChromeColors,
    /// Opaque field reference carrying content resolved for its material frame.
    pub(crate) floating_field_reference: ChromeColors,
    /// Standard field fills and content resolved against their material frame on a floating host.
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

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct PreparedControlHost {
    pub(crate) reference: ChromeColors,
    pub(crate) colors: ChromeColors,
    pub(crate) segmented: ChromeColors,
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
        let control_colors = colors.material_presentation(SurfaceMaterials::OPAQUE);
        let segmented_control_colors =
            compile_segmented_control_colors(&colors, &control_colors, SurfaceMaterials::OPAQUE);
        let panel_controls =
            prepare_control_host(&authored, colors.panel_background, SurfaceMaterials::OPAQUE);
        let card_controls = prepare_control_host(
            &authored,
            colors.elevated_surface_background,
            SurfaceMaterials::OPAQUE,
        );
        let floating_control_colors = resolve_floating_control_colors(
            &floating_colors,
            floating_colors.material_presentation(floating_materials),
            floating_raised_material,
            floating_raised_wash,
        );
        let floating_segmented_colors = resolve_floating_segmented_colors(
            &floating_colors,
            compile_segmented_control_colors(
                &floating_colors,
                &floating_control_colors,
                floating_materials,
            ),
            floating_raised_material,
            floating_raised_wash,
        );
        let (floating_field_reference, floating_field_colors) = resolve_floating_field_colors(
            floating_colors.clone(),
            floating_control_colors.clone(),
            floating_raised_material,
            floating_raised_wash,
        );
        Self {
            appearance: Appearance::Dark,
            control_colors,
            segmented_control_colors,
            panel_controls,
            card_controls,
            floating_control_colors,
            floating_segmented_colors,
            floating_colors,
            floating_field_reference,
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

pub(super) fn readable_on_backgrounds<const N: usize>(
    proposed: Color,
    backgrounds: [Color; N],
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

fn resolve_floating_field_colors(
    mut reference: ChromeColors,
    mut paint: ChromeColors,
    material: Color,
    wash: Color,
) -> (ChromeColors, ChromeColors) {
    let host_backgrounds = [Color::rgb(0x000000), Color::rgb(0xffffff)]
        .map(|underlay| wash.source_over(material.source_over(underlay)));
    let (input_background, [input_text, input_placeholder, input_caret]) = resolve_floating_frame(
        reference.input_background,
        paint.input_background,
        reference.elevated_surface_background,
        host_backgrounds,
        [
            (reference.input_text, 4.5),
            (reference.input_placeholder, 4.5),
            (reference.input_caret, 3.0),
        ],
    );
    paint.input_background = input_background;
    reference.input_text = input_text;
    reference.input_placeholder = input_placeholder;
    reference.input_caret = input_caret;
    paint.input_text = input_text;
    paint.input_placeholder = input_placeholder;
    paint.input_caret = input_caret;
    let (input_disabled_background, [input_disabled_text]) = resolve_floating_state(
        reference.input_disabled_background,
        paint.input_disabled_background,
        reference.elevated_surface_background,
        host_backgrounds,
        [reference.input_disabled_text],
        4.5,
    );
    paint.input_disabled_background = input_disabled_background;
    reference.input_disabled_text = input_disabled_text;
    paint.input_disabled_text = input_disabled_text;
    let field_backgrounds =
        host_backgrounds.map(|background| paint.input_background.source_over(background));
    let (selection_background, [selection_foreground]) = resolve_floating_state(
        reference.input_selection_background,
        paint.input_selection_background,
        reference.input_background,
        field_backgrounds,
        [reference.input_selection_foreground],
        4.5,
    );
    paint.input_selection_background = selection_background;
    reference.input_selection_foreground = selection_foreground;
    paint.input_selection_foreground = selection_foreground;
    (reference, paint)
}

fn resolve_floating_control_colors(
    reference: &ChromeColors,
    mut paint: ChromeColors,
    material: Color,
    wash: Color,
) -> ChromeColors {
    let host_backgrounds = [Color::rgb(0x000000), Color::rgb(0xffffff)]
        .map(|underlay| wash.source_over(material.source_over(underlay)));
    macro_rules! state {
        ($fill:ident, $minimum:expr, $($content:ident),+ $(,)?) => {{
            let (fill, [$($content),+]) = resolve_floating_state(
                reference.$fill,
                paint.$fill,
                reference.elevated_surface_background,
                host_backgrounds,
                [$(reference.$content),+],
                $minimum,
            );
            paint.$fill = fill;
            $(paint.$content = $content;)+
        }};
    }
    state!(element_background, 4.5, element_foreground, element_icon);
    state!(
        element_hover,
        4.5,
        element_hover_foreground,
        element_hover_icon
    );
    state!(
        element_active,
        4.5,
        element_active_foreground,
        element_active_icon
    );
    state!(
        element_disabled,
        3.0,
        element_disabled_foreground,
        element_disabled_icon
    );
    state!(
        ghost_element_background,
        4.5,
        ghost_element_foreground,
        ghost_element_icon
    );
    state!(
        ghost_element_hover,
        4.5,
        ghost_element_hover_foreground,
        ghost_element_hover_icon
    );
    state!(
        ghost_element_active,
        4.5,
        ghost_element_active_foreground,
        ghost_element_active_icon
    );
    state!(
        ghost_element_selected,
        4.5,
        ghost_element_selected_foreground
    );
    state!(
        ghost_element_disabled,
        3.0,
        ghost_element_disabled_foreground,
        ghost_element_disabled_icon
    );
    state!(primary_background, 4.5, primary_foreground, primary_icon);
    state!(
        primary_hover_background,
        4.5,
        primary_hover_foreground,
        primary_hover_icon
    );
    state!(
        primary_pressed_background,
        4.5,
        primary_pressed_foreground,
        primary_pressed_icon
    );
    state!(
        primary_disabled_background,
        3.0,
        primary_disabled_foreground,
        primary_disabled_icon
    );
    state!(
        destructive_background,
        4.5,
        destructive_foreground,
        destructive_icon
    );
    state!(
        destructive_hover_background,
        4.5,
        destructive_hover_foreground,
        destructive_hover_icon
    );
    state!(
        destructive_pressed_background,
        4.5,
        destructive_pressed_foreground,
        destructive_pressed_icon
    );
    state!(
        destructive_disabled_background,
        3.0,
        destructive_disabled_foreground,
        destructive_disabled_icon
    );
    state!(
        selection_background,
        4.5,
        selection_foreground,
        selection_icon
    );
    state!(
        selection_hover_background,
        4.5,
        selection_hover_foreground,
        selection_hover_icon
    );
    state!(
        selection_pressed_background,
        4.5,
        selection_pressed_foreground,
        selection_pressed_icon
    );
    state!(
        selection_disabled_background,
        3.0,
        selection_disabled_foreground,
        selection_disabled_icon
    );
    state!(toggle_off_hover_background, 3.0, toggle_off_hover_mark);
    state!(toggle_off_pressed_background, 3.0, toggle_off_pressed_mark);
    state!(
        toggle_off_disabled_background,
        3.0,
        toggle_off_disabled_mark
    );
    state!(toggle_on_background, 3.0, toggle_on_mark);
    state!(toggle_on_hover_background, 3.0, toggle_on_hover_mark);
    state!(toggle_on_pressed_background, 3.0, toggle_on_pressed_mark);
    state!(toggle_on_disabled_background, 3.0, toggle_on_disabled_mark);
    for (target, proposed, minimum) in [
        (&mut paint.link_text, reference.link_text, 4.5),
        (&mut paint.link_text_hover, reference.link_text_hover, 4.5),
        (
            &mut paint.link_text_pressed,
            reference.link_text_pressed,
            4.5,
        ),
        (
            &mut paint.link_text_disabled,
            reference.link_text_disabled,
            3.0,
        ),
        (&mut paint.toggle_off_label, reference.toggle_off_label, 4.5),
        (
            &mut paint.toggle_off_hover_label,
            reference.toggle_off_hover_label,
            4.5,
        ),
        (
            &mut paint.toggle_off_pressed_label,
            reference.toggle_off_pressed_label,
            4.5,
        ),
        (
            &mut paint.toggle_off_disabled_label,
            reference.toggle_off_disabled_label,
            3.0,
        ),
        (&mut paint.toggle_on_label, reference.toggle_on_label, 4.5),
        (
            &mut paint.toggle_on_hover_label,
            reference.toggle_on_hover_label,
            4.5,
        ),
        (
            &mut paint.toggle_on_pressed_label,
            reference.toggle_on_pressed_label,
            4.5,
        ),
        (
            &mut paint.toggle_on_disabled_label,
            reference.toggle_on_disabled_label,
            3.0,
        ),
    ] {
        *target = readable_on_backgrounds(proposed, host_backgrounds, minimum);
    }
    let (progress_background, text_accent) = resolve_floating_progress_accent(
        reference.toggle_off_background,
        paint.toggle_off_background,
        reference.elevated_surface_background,
        host_backgrounds,
        reference.text_accent,
    );
    paint.toggle_off_background = progress_background;
    paint.text_accent = text_accent;
    let progress_backgrounds =
        host_backgrounds.map(|background| progress_background.source_over(background));
    paint.toggle_off_mark =
        readable_on_backgrounds(reference.toggle_off_mark, progress_backgrounds, 3.0);
    paint.info = readable_on_backgrounds(reference.info, host_backgrounds, 3.0);
    paint.info_border = readable_on_backgrounds(reference.info_border, host_backgrounds, 3.0);
    paint.success = readable_on_backgrounds(reference.success, host_backgrounds, 3.0);
    paint.success_border = readable_on_backgrounds(reference.success_border, host_backgrounds, 3.0);
    paint.warning = readable_on_backgrounds(reference.warning, host_backgrounds, 3.0);
    paint.warning_border = readable_on_backgrounds(reference.warning_border, host_backgrounds, 3.0);
    paint.error = readable_on_backgrounds(reference.error, host_backgrounds, 3.0);
    paint.error_border = readable_on_backgrounds(reference.error_border, host_backgrounds, 3.0);
    paint
}

fn resolve_floating_state<const N: usize>(
    reference_fill: Color,
    paint_fill: Color,
    reference_surface: Color,
    host_backgrounds: [Color; 2],
    proposed: [Color; N],
    minimum_contrast: f64,
) -> (Color, [Color; N]) {
    resolve_floating_frame(
        reference_fill,
        paint_fill,
        reference_surface,
        host_backgrounds,
        proposed.map(|color| (color, minimum_contrast)),
    )
}

fn resolve_floating_frame<const N: usize>(
    reference_fill: Color,
    paint_fill: Color,
    reference_surface: Color,
    host_backgrounds: [Color; 2],
    proposed: [(Color, f64); N],
) -> (Color, [Color; N]) {
    let backgrounds = host_backgrounds.map(|background| paint_fill.source_over(background));
    if let Some(resolved) = resolve_content_on_backgrounds(proposed, backgrounds) {
        return (paint_fill, resolved);
    }
    let fallback = reference_fill.source_over(reference_surface);
    (
        fallback,
        proposed.map(|(color, minimum)| readable_on_backgrounds(color, [fallback; 2], minimum)),
    )
}

fn resolve_floating_progress_accent(
    reference_fill: Color,
    paint_fill: Color,
    reference_surface: Color,
    host_backgrounds: [Color; 2],
    proposed: Color,
) -> (Color, Color) {
    let progress_backgrounds =
        host_backgrounds.map(|background| paint_fill.source_over(background));
    let backgrounds = [
        host_backgrounds[0],
        host_backgrounds[1],
        progress_backgrounds[0],
        progress_backgrounds[1],
    ];
    if let Some([resolved]) = resolve_content_on_backgrounds([(proposed, 4.5)], backgrounds) {
        return (paint_fill, resolved);
    }

    let fallback = reference_fill.source_over(reference_surface);
    let fallback_backgrounds = [host_backgrounds[0], host_backgrounds[1], fallback, fallback];
    if let Some([resolved]) =
        resolve_content_on_backgrounds([(proposed, 4.5)], fallback_backgrounds)
    {
        return (fallback, resolved);
    }

    if let Some([resolved]) = resolve_content_on_backgrounds([(proposed, 4.5)], host_backgrounds) {
        let frame = if resolved
            .source_over(reference_surface)
            .contrast_ratio(reference_surface)
            >= 4.5
        {
            reference_surface
        } else {
            host_backgrounds[0]
        };
        return (frame, resolved);
    }

    let resolved = readable_on_backgrounds(proposed, [fallback; 2], 4.5);
    (fallback, resolved)
}

fn resolve_content_on_backgrounds<const N: usize, const B: usize>(
    proposed: [(Color, f64); N],
    backgrounds: [Color; B],
) -> Option<[Color; N]> {
    let resolved =
        proposed.map(|(color, minimum)| readable_on_backgrounds(color, backgrounds, minimum));
    resolved
        .iter()
        .zip(proposed)
        .all(|(color, (_, minimum))| {
            backgrounds.into_iter().all(|background| {
                color.source_over(background).contrast_ratio(background) >= minimum
            })
        })
        .then_some(resolved)
}

fn compile_segmented_control_colors(
    reference: &ChromeColors,
    paint: &ChromeColors,
    materials: SurfaceMaterials,
) -> ChromeColors {
    if materials.is_opaque() {
        return paint.clone();
    }
    let mut segmented = paint.clone();
    segmented.border = materials.edge(reference.element_background, reference.border);
    segmented.border_variant =
        materials.edge(reference.ghost_element_active, reference.border_variant);
    let option =
        |target| materials.paint(SurfaceRole::Surface, reference.element_background, target);
    segmented.ghost_element_hover = option(reference.ghost_element_hover);
    segmented.ghost_element_active = option(reference.ghost_element_active);
    segmented.selection_background = option(reference.selection_background);
    segmented.selection_hover_background = option(reference.selection_hover_background);
    segmented.selection_pressed_background = option(reference.selection_pressed_background);
    segmented.selection_disabled_background = option(reference.selection_disabled_background);
    segmented
}

fn prepare_control_host(
    authored: &ChromeColors,
    host: Color,
    materials: SurfaceMaterials,
) -> PreparedControlHost {
    let reference = authored.host_presentation(host);
    let colors = reference.material_presentation(materials);
    let segmented = compile_segmented_control_colors(&reference, &colors, materials);
    PreparedControlHost {
        reference,
        colors,
        segmented,
    }
}

fn resolve_floating_segmented_colors(
    reference: &ChromeColors,
    mut paint: ChromeColors,
    material: Color,
    wash: Color,
) -> ChromeColors {
    let host_backgrounds = [Color::rgb(0x000000), Color::rgb(0xffffff)]
        .map(|underlay| wash.source_over(material.source_over(underlay)));
    let (track, [text_secondary, text_disabled]) = resolve_floating_frame(
        reference.element_background,
        paint.element_background,
        reference.elevated_surface_background,
        host_backgrounds,
        [
            (reference.text_secondary, 4.5),
            (reference.text_disabled, 3.0),
        ],
    );
    paint.element_background = track;
    paint.text_secondary = text_secondary;
    paint.text_disabled = text_disabled;
    let track_backgrounds =
        host_backgrounds.map(|background| paint.element_background.source_over(background));
    macro_rules! state {
        ($fill:ident, $minimum:expr, $($content:ident),+ $(,)?) => {{
            let (fill, [$($content),+]) = resolve_floating_state(
                reference.$fill,
                paint.$fill,
                reference.element_background,
                track_backgrounds,
                [$(reference.$content),+],
                $minimum,
            );
            paint.$fill = fill;
            $(paint.$content = $content;)+
        }};
    }
    state!(ghost_element_hover, 4.5, ghost_element_hover_foreground);
    state!(ghost_element_active, 4.5, ghost_element_active_foreground);
    state!(selection_background, 4.5, selection_foreground);
    state!(selection_hover_background, 4.5, selection_hover_foreground);
    state!(
        selection_pressed_background,
        4.5,
        selection_pressed_foreground
    );
    state!(
        selection_disabled_background,
        3.0,
        selection_disabled_foreground
    );
    paint
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

    /// Pane boundaries use the structural separator independently of decorative row rims.
    pub(crate) fn pane_rim(&self) -> Color {
        self.materials
            .edge(self.colors.panel_background, self.colors.tab_separator)
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
            let edge = self.floating_materials.edge(color, self.colors.border);
            let divider = self
                .floating_materials
                .edge(color, self.colors.border_variant);
            FloatingSurfacePaint::new(
                rgba(wash.rgba_hex()),
                rgba(edge.rgba_hex()),
                rgba(divider.rgba_hex()),
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
        let control_colors = colors.material_presentation(resolved.composition.materials);
        let segmented_control_colors = compile_segmented_control_colors(
            &colors,
            &control_colors,
            resolved.composition.materials,
        );
        let panel_controls = prepare_control_host(
            &resolved.colors,
            colors.panel_background,
            resolved.composition.materials,
        );
        let card_controls = prepare_control_host(
            &resolved.colors,
            colors.elevated_surface_background,
            resolved.composition.materials,
        );
        let floating_control_colors = resolve_floating_control_colors(
            &floating_colors,
            floating_colors.material_presentation(resolved.composition.floating_materials),
            floating_raised_material,
            floating_raised_wash,
        );
        let floating_segmented_colors = resolve_floating_segmented_colors(
            &floating_colors,
            compile_segmented_control_colors(
                &floating_colors,
                &floating_control_colors,
                resolved.composition.floating_materials,
            ),
            floating_raised_material,
            floating_raised_wash,
        );
        let (floating_field_reference, floating_field_colors) = resolve_floating_field_colors(
            floating_colors.clone(),
            floating_control_colors.clone(),
            floating_raised_material,
            floating_raised_wash,
        );
        Self {
            appearance: resolved.appearance,
            control_colors,
            segmented_control_colors,
            panel_controls,
            card_controls,
            floating_control_colors,
            floating_segmented_colors,
            floating_colors,
            floating_field_reference,
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
    use super::{
        ChromeAppearance, resolve_floating_control_colors, resolve_floating_field_colors,
        resolve_floating_segmented_colors,
    };

    #[test]
    fn floating_fields_use_the_opaque_frame_when_shifted_endpoints_have_no_readable_foreground() {
        use crate::appearance::{ChromeColors, Color};

        let reference = ChromeColors {
            elevated_surface_background: Color::rgb(0x202020),
            input_background: Color::rgb(0x767676),
            input_text: Color::rgb(0xffffff),
            input_placeholder: Color::rgb(0xffffff),
            ..ChromeColors::default()
        };
        let paint = ChromeColors {
            input_background: Color::rgba(0x737373e7),
            ..reference.clone()
        };
        let (_, resolved) =
            resolve_floating_field_colors(reference, paint, Color::rgba(0), Color::rgba(0));

        for underlay in [Color::rgb(0x000000), Color::rgb(0xffffff)] {
            let background = resolved.input_background.source_over(underlay);
            assert!(
                resolved
                    .input_text
                    .source_over(background)
                    .contrast_ratio(background)
                    >= 4.5,
                "field text must read over {background:?}",
            );
        }
    }

    #[test]
    fn floating_segment_labels_use_the_opaque_track_when_shifted_endpoints_are_unreadable() {
        use crate::appearance::{ChromeColors, Color};

        let reference = ChromeColors {
            elevated_surface_background: Color::rgb(0x202020),
            element_background: Color::rgb(0x767676),
            text_secondary: Color::rgb(0xffffff),
            ..ChromeColors::default()
        };
        let paint = ChromeColors {
            element_background: Color::rgba(0x737373e7),
            ..reference.clone()
        };
        let resolved =
            resolve_floating_segmented_colors(&reference, paint, Color::rgba(0), Color::rgba(0));

        for underlay in [Color::rgb(0x000000), Color::rgb(0xffffff)] {
            let background = resolved.element_background.source_over(underlay);
            assert!(
                resolved
                    .text_secondary
                    .source_over(background)
                    .contrast_ratio(background)
                    >= 4.5,
                "segment label must read over {background:?}",
            );
        }
    }

    #[test]
    fn floating_progress_accent_uses_the_opaque_track_when_shifted_endpoints_are_unreadable() {
        use crate::appearance::{ChromeColors, Color};

        let reference = ChromeColors {
            elevated_surface_background: Color::rgb(0x606060),
            toggle_off_background: Color::rgb(0x767676),
            toggle_off_mark: Color::rgb(0xffffff),
            text_accent: Color::rgb(0xffffff),
            ..ChromeColors::default()
        };
        let paint = ChromeColors {
            toggle_off_background: Color::rgba(0xffffff1a),
            ..reference.clone()
        };
        let material = Color::rgba(0x666666ef);
        let resolved = resolve_floating_control_colors(&reference, paint, material, Color::rgba(0));

        for underlay in [Color::rgb(0x000000), Color::rgb(0xffffff)] {
            let host = material.source_over(underlay);
            let track = resolved.toggle_off_background.source_over(host);
            for background in [host, track] {
                assert!(
                    resolved
                        .text_accent
                        .source_over(background)
                        .contrast_ratio(background)
                        >= 4.5,
                    "progress accent must read over {background:?}",
                );
            }
        }
    }

    #[test]
    fn floating_warning_border_keeps_its_role_and_resolves_against_the_host() {
        use crate::appearance::{ChromeColors, Color};

        let reference = ChromeColors {
            warning: Color::rgb(0xd02020),
            warning_border: Color::rgb(0xf0f0f0),
            ..ChromeColors::default()
        };
        let resolved = resolve_floating_control_colors(
            &reference,
            reference.clone(),
            Color::rgba(0),
            Color::rgb(0xffffff),
        );

        assert_ne!(resolved.warning_border, resolved.warning);
        assert!(resolved.warning_border.contrast_ratio(Color::rgb(0xffffff)) >= 3.0,);
    }

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
