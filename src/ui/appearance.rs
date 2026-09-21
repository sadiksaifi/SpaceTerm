//! Prepared chrome presentation shared by app-owned composites and reusable controls.

mod built_in_dark;
pub(crate) mod built_in_light;
mod collection_selection;
mod disabled_union;
mod separator;
pub(crate) mod settings;

use std::sync::Arc;

use super::{
    chrome_icons::ChromeIcons, chrome_state::ChromeStatePolicy, chrome_typography::ChromeTypography,
};

use gpui::{App, Font, FontWeight, Global, Pixels, Window, font, px, rgba};

use crate::appearance::{
    Appearance, ChromeColors, ChromeDensity, Color, ColorProvenance, CompositionCapabilities,
    ResolvedChromeAppearance, ResolvedFontDescriptor, SurfaceMaterials, SurfaceRole,
};

pub(super) const SUBDUED_SELECTION_CONTRAST: f64 = 1.20;

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
    pub(crate) active: bool,
    pub(crate) capabilities: CompositionCapabilities,
    pub(crate) typography: ChromeTypography,
    pub(crate) icons: ChromeIcons,
    pub(crate) semantic_text_pairs: super::chrome_semantic_pairs::SemanticTextPairs,
    /// Prepared active-window selection pairs for collections without keyboard focus.
    pub(crate) unfocused_selection: collection_selection::PreparedCollectionSelection,
    /// Opaque presentation: the contrast reference and the input to Workspace surface owners.
    pub(crate) colors: ChromeColors,
    /// The same colors with the window's material applied to background fills, for controls.
    pub(crate) control_colors: ChromeColors,
    /// Segmented option fills compiled against their immediate track host.
    pub(crate) segmented_control_colors: ChromeColors,
    /// Window-hosted families whose authored constraints required a safe fallback.
    pub(crate) window_control_fallbacks: Vec<FloatingControlFamily>,
    /// Control presentation for a Panel host.
    pub(crate) panel_controls: PreparedControlHost,
    /// Controls on the independently authored active or inactive title bar.
    pub(crate) title_bar_controls: PreparedControlHost,
    /// Control presentation for a Card host.
    pub(crate) card_controls: PreparedControlHost,
    /// Opaque semantic and contrast reference for controls on the raised floating host.
    pub(crate) floating_colors: ChromeColors,
    /// Disabled selection derives from the active state in both window variants.
    pub(crate) floating_disabled_selected_background: Color,
    /// Floating control fills expressed as overlays on the raised host.
    pub(crate) floating_control_colors: ChromeColors,
    /// Floating segmented option fills compiled against their material track.
    pub(crate) floating_segmented_colors: ChromeColors,
    /// Opaque field reference carrying content resolved for its material frame.
    pub(crate) floating_field_reference: ChromeColors,
    /// Standard field fills and content resolved against their material frame on a floating host.
    pub(crate) floating_field_colors: ChromeColors,
    /// Content-free families whose authored constraints required a safe fallback.
    ///
    /// Most families use opaque state fills when no translucent solution exists. An
    /// unrepresentable host-relative step can instead retain the legacy safe presentation, which
    /// may remain translucent.
    pub(crate) floating_fallbacks: Vec<FloatingControlFamily>,
    /// Content-free disabled-state failures collected across every prepared host.
    pub(crate) disabled_diagnostics: Vec<DisabledControlDiagnostic>,
    pub(crate) materials: SurfaceMaterials,
    pub(crate) floating_materials: SurfaceMaterials,
    pub(crate) floating_blur: bool,
    pub(crate) text_scale: f32,
    pub(crate) spacing_scale: f32,
    pub(crate) settings_hosts: Option<settings::SettingsHostBackgrounds>,
    /// The built-in Light definition owns a boundary and surface policy of its own.
    pub(crate) built_in_light: bool,
    /// The built-in Dark definition separates internal rules from independent surface edges.
    pub(crate) built_in_dark: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct PreparedControlHost {
    /// Semantic host reference whose host-level content registers are resolved on final material.
    /// Surface and control-state roles remain the opaque semantic reference.
    pub(crate) reference: ChromeColors,
    pub(crate) colors: ChromeColors,
    pub(crate) segmented: ChromeColors,
    pub(crate) fallback_families: Vec<FloatingControlFamily>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum DisabledControlDiagnostic {
    ContrastFloor { family: FloatingControlFamily },
    Separation { family: FloatingControlFamily },
    SharedPaint { family: FloatingControlFamily },
    SelectedStep { family: FloatingControlFamily },
}

fn with_final_host_content(mut reference: ChromeColors, resolved: &ChromeColors) -> ChromeColors {
    macro_rules! project {
        ($($field:ident),+ $(,)?) => { $(reference.$field = resolved.$field;)+ };
    }
    project!(
        text,
        text_accent,
        text_secondary,
        text_muted,
        text_placeholder,
        text_disabled,
        icon,
        icon_muted,
        icon_disabled,
        link_text,
        link_text_hover,
        link_text_pressed,
        link_text_disabled,
        focus_ring,
        sidebar_focus
    );
    reference
}

impl Default for ChromeAppearance {
    fn default() -> Self {
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
            FloatingContrastFloors::STANDARD,
        );
        let control_colors = colors.material_presentation(SurfaceMaterials::OPAQUE);
        let segmented_compilation = compile_segmented_control_colors(
            &authored,
            &colors,
            &control_colors,
            SurfaceMaterials::OPAQUE,
            false,
        );
        let window_control_fallbacks = segmented_compilation
            .used_host_fallback
            .then_some(FloatingControlFamily::Segmented)
            .into_iter()
            .collect();
        let segmented_control_colors = segmented_compilation.colors;
        let panel_controls =
            prepare_control_host(&authored, colors.panel_background, SurfaceMaterials::OPAQUE);
        let title_bar_controls = prepare_control_host(
            &authored,
            colors.title_bar_background,
            SurfaceMaterials::OPAQUE,
        );
        let card_controls = prepare_control_host(
            &authored,
            colors.elevated_surface_background,
            SurfaceMaterials::OPAQUE,
        );
        let FloatingColorResolution {
            colors: floating_control_colors,
            mut fallback_families,
        } = resolve_floating_control_colors_detailed(
            &authored,
            FloatingContrastFloors::STANDARD,
            &floating_colors,
            floating_colors.material_presentation(floating_materials),
            floating_raised_material,
            floating_raised_wash,
        );
        let FloatingColorResolution {
            colors: floating_segmented_colors,
            fallback_families: segmented_fallbacks,
        } = resolve_floating_segmented_colors_detailed(
            &authored,
            FloatingContrastFloors::STANDARD,
            &floating_colors,
            compile_segmented_control_colors(
                &authored,
                &floating_colors,
                &floating_control_colors,
                floating_materials,
                false,
            )
            .colors,
            floating_raised_material,
            floating_raised_wash,
            false,
        );
        let FloatingFieldResolution {
            reference: floating_field_reference,
            colors: floating_field_colors,
            fallback_families: field_fallbacks,
        } = resolve_floating_field_colors_detailed(
            &authored,
            FloatingContrastFloors::STANDARD,
            floating_colors.clone(),
            floating_control_colors.clone(),
            floating_raised_material,
            floating_raised_wash,
        );
        let colors = with_final_host_content(colors, &control_colors);
        let semantic_text_pairs = super::chrome_semantic_pairs::prepare_semantic_text_pairs(
            &colors,
            &card_controls.reference,
            SurfaceMaterials::OPAQUE,
            colors.background,
            colors.elevated_surface_background,
            false,
        );
        let unfocused_selection = collection_selection::PreparedCollectionSelection::identity(
            &colors,
            &title_bar_controls.reference,
            &panel_controls.reference,
            &card_controls.reference,
            &floating_colors,
        );
        Self {
            appearance: Appearance::Dark,
            active: true,
            capabilities: CompositionCapabilities::default(),
            typography: ChromeTypography::default(),
            icons: ChromeIcons::default(),
            semantic_text_pairs,
            unfocused_selection,
            control_colors,
            segmented_control_colors,
            window_control_fallbacks,
            panel_controls,
            title_bar_controls,
            card_controls,
            floating_control_colors,
            floating_segmented_colors,
            floating_disabled_selected_background: floating_colors
                .row_selected_background
                .mix(floating_colors.elevated_surface_background, 0.60),
            floating_colors,
            floating_field_reference,
            floating_field_colors,
            colors,
            floating_fallbacks: {
                for family in segmented_fallbacks.into_iter().chain(field_fallbacks) {
                    if !fallback_families.contains(&family) {
                        fallback_families.push(family);
                    }
                }
                fallback_families
            },
            disabled_diagnostics: Vec::new(),
            materials: SurfaceMaterials::OPAQUE,
            floating_materials,
            floating_blur: false,
            text_scale: 1.0,
            spacing_scale: 1.0,
            settings_hosts: None,
            built_in_light: false,
            built_in_dark: false,
        }
    }
}

/// Keeps the shared alpha while moving only the floating tint far enough for one neutral
/// foreground to read over every admitted backdrop.
fn floating_material(appearance: Appearance, material: Color, wash: Color) -> Option<Color> {
    floating_material_with_floor(appearance, material, wash, 4.5)
}

fn floating_material_with_floor(
    appearance: Appearance,
    material: Color,
    wash: Color,
    minimum: f64,
) -> Option<Color> {
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
                    foreground.contrast_ratio(background) >= minimum
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
    floors: FloatingContrastFloors,
) -> ChromeColors {
    let raised = |color| readable_on_material(color, raised_material, raised_wash, floors.primary);
    let secondary =
        |color| readable_on_material(color, raised_material, raised_wash, floors.secondary);
    let raised_disabled =
        |color| readable_on_material(color, raised_material, raised_wash, floors.disabled);
    let readout =
        |color| readable_on_material(color, readout_material, readout_wash, floors.secondary);
    colors.text = raised(colors.text);
    colors.text_secondary = secondary(colors.text_secondary);
    colors.text_muted = secondary(colors.text_muted);
    colors.text_placeholder = secondary(colors.text_placeholder);
    colors.text_disabled = raised_disabled(colors.text_disabled);
    colors.icon = raised(colors.icon);
    colors.icon_muted = secondary(colors.icon_muted);
    colors.icon_disabled = raised_disabled(colors.icon_disabled);
    colors.input_text = raised(colors.input_text);
    colors.input_placeholder = secondary(colors.input_placeholder);
    colors.preview_foreground = readout(colors.preview_foreground);
    macro_rules! row_content {
        ($($field:ident),+ $(,)?) => {
            $(colors.$field = raised(colors.$field);)+
        };
    }
    row_content!(
        row_foreground,
        row_icon,
        row_match,
        row_hover_foreground,
        row_hover_icon,
        row_hover_match,
        row_selected_foreground,
        row_selected_icon,
        row_selected_match,
        row_selected_hover_foreground,
        row_selected_hover_icon,
        row_selected_hover_match,
        navigation_selected_foreground,
        navigation_selected_icon,
    );
    for role in [
        &mut colors.row_secondary,
        &mut colors.row_hover_secondary,
        &mut colors.row_selected_secondary,
        &mut colors.row_selected_hover_secondary,
        &mut colors.navigation_selected_secondary,
    ] {
        *role = secondary(*role);
    }
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

fn content_meets_backgrounds<const N: usize>(
    content: Color,
    backgrounds: [Color; N],
    minimum: f64,
) -> bool {
    backgrounds
        .into_iter()
        .all(|background| content.source_over(background).contrast_ratio(background) >= minimum)
}

fn content_is_lighter_than_background(content: Color, background: Color) -> bool {
    relative_luminance(content.source_over(background)) > relative_luminance(background)
}

fn preferred_readable_endpoint<const N: usize>(backgrounds: [Color; N]) -> Color {
    let minimum = |color: Color| {
        backgrounds
            .into_iter()
            .map(|background| color.contrast_ratio(background))
            .fold(f64::INFINITY, f64::min)
    };
    let dark = Color::rgb(0x000000);
    let light = Color::rgb(0xffffff);
    if minimum(dark) >= minimum(light) {
        dark
    } else {
        light
    }
}

fn readable_toward_endpoint<const N: usize>(
    proposed: Color,
    endpoint: Color,
    backgrounds: [Color; N],
    minimum: f64,
) -> Color {
    let endpoint_is_lighter = backgrounds
        .into_iter()
        .all(|background| relative_luminance(endpoint) >= relative_luminance(background));
    let proposed_is_on_endpoint_side = backgrounds.into_iter().all(|background| {
        (relative_luminance(proposed.source_over(background)) >= relative_luminance(background))
            == endpoint_is_lighter
    });
    if proposed_is_on_endpoint_side && content_meets_backgrounds(proposed, backgrounds, minimum) {
        return proposed;
    }
    proposed
        .readable_preserving_chroma_toward(&backgrounds, minimum, endpoint_is_lighter)
        .unwrap_or(endpoint)
}

#[cfg(test)]
fn resolve_floating_field_colors(
    authored: &ChromeColors,
    floors: FloatingContrastFloors,
    reference: ChromeColors,
    paint: ChromeColors,
    material: Color,
    wash: Color,
) -> (ChromeColors, ChromeColors) {
    let resolved =
        resolve_floating_field_colors_detailed(authored, floors, reference, paint, material, wash);
    let _pending_diagnostics = resolved.fallback_families;
    (resolved.reference, resolved.colors)
}

fn resolve_floating_field_colors_detailed(
    authored: &ChromeColors,
    floors: FloatingContrastFloors,
    mut reference: ChromeColors,
    mut paint: ChromeColors,
    material: Color,
    wash: Color,
) -> FloatingFieldResolution {
    let host_backgrounds = [Color::rgb(0x000000), Color::rgb(0xffffff)]
        .map(|underlay| wash.source_over(material.source_over(underlay)));
    if let Some(focus_floor) = floors.focus {
        let focus_ring =
            resolve_host_endpoint_color(paint.focus_ring, host_backgrounds, focus_floor);
        reference.focus_ring = focus_ring;
        paint.focus_ring = focus_ring;
    }
    let input_focused_border = resolve_host_endpoint_color(
        paint.input_focused_border,
        host_backgrounds,
        floors.boundary,
    );
    let input_invalid_border = resolve_host_endpoint_color(
        paint.input_invalid_border,
        host_backgrounds,
        floors.boundary,
    );
    reference.input_focused_border = input_focused_border;
    reference.input_invalid_border = input_invalid_border;
    paint.input_focused_border = input_focused_border;
    paint.input_invalid_border = input_invalid_border;
    let input_states = rehost_floating_states(
        [
            floating_state(
                reference.input_background,
                paint.input_background,
                reference.elevated_surface_background,
                [
                    floating_constraint(reference.input_text, floors.primary),
                    floating_constraint(reference.input_placeholder, floors.secondary),
                    floating_constraint(reference.input_caret, floors.boundary),
                    floating_host_constraint(paint.input_border, floors.boundary),
                ],
            ),
            floating_state(
                reference.input_disabled_background,
                paint.input_disabled_background,
                reference.elevated_surface_background,
                [
                    floating_constraint(reference.input_disabled_text, floors.disabled),
                    floating_host_constraint(paint.input_disabled_border, floors.boundary),
                    None,
                    None,
                ],
            ),
        ],
        [
            authored.input_background,
            authored.input_disabled_background,
        ],
        authored.background,
        reference.elevated_surface_background,
    );
    let input_resolution =
        resolve_floating_family_for_presentation(input_states, host_backgrounds, &[]);
    let mut fallback_families = Vec::new();
    if let Ok((states, used_opaque_fallback)) = input_resolution {
        if used_opaque_fallback {
            fallback_families.push(FloatingControlFamily::Input);
        }
        paint.input_background = states[0].fill;
        reference.input_text = states[0].content[0];
        reference.input_placeholder = states[0].content[1];
        reference.input_caret = states[0].content[2];
        paint.input_text = states[0].content[0];
        paint.input_placeholder = states[0].content[1];
        paint.input_caret = states[0].content[2];
        paint.input_border = states[0].content[3];
        paint.input_disabled_background = states[1].fill;
        reference.input_disabled_text = states[1].content[0];
        paint.input_disabled_text = states[1].content[0];
        paint.input_disabled_border = states[1].content[1];
    } else {
        fallback_families.push(FloatingControlFamily::Input);
        let (input_background, [input_text, input_placeholder, input_caret], input_border) =
            resolve_floating_frame_with_boundary(
                reference.input_background,
                paint.input_background,
                reference.elevated_surface_background,
                host_backgrounds,
                [
                    (reference.input_text, floors.primary),
                    (reference.input_placeholder, floors.secondary),
                    (reference.input_caret, floors.boundary),
                ],
                paint.input_border,
                floors.boundary,
            );
        paint.input_background = input_background;
        reference.input_text = input_text;
        reference.input_placeholder = input_placeholder;
        reference.input_caret = input_caret;
        paint.input_text = input_text;
        paint.input_placeholder = input_placeholder;
        paint.input_caret = input_caret;
        paint.input_border = input_border;
        let (input_disabled_background, [input_disabled_text], input_disabled_border) =
            resolve_floating_frame_with_boundary(
                reference.input_disabled_background,
                paint.input_disabled_background,
                reference.elevated_surface_background,
                host_backgrounds,
                [(reference.input_disabled_text, floors.disabled)],
                paint.input_disabled_border,
                floors.boundary,
            );
        paint.input_disabled_background = input_disabled_background;
        reference.input_disabled_text = input_disabled_text;
        paint.input_disabled_text = input_disabled_text;
        paint.input_disabled_border = input_disabled_border;
    }
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
    FloatingFieldResolution {
        reference,
        colors: paint,
        fallback_families,
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum FloatingControlFamily {
    Element,
    GhostElement,
    Toggle,
    Segmented,
    Input,
}

#[derive(Clone, Copy)]
struct FloatingContrastFloors {
    primary: f64,
    secondary: f64,
    mark: f64,
    disabled: f64,
    boundary: f64,
    focus: Option<f64>,
    interactive: bool,
}

impl FloatingContrastFloors {
    const STANDARD: Self = Self {
        primary: 4.5,
        secondary: 4.5,
        mark: 3.0,
        disabled: 3.0,
        boundary: 3.0,
        focus: None,
        interactive: true,
    };

    const INCREASED: Self = Self {
        primary: 7.0,
        secondary: 4.5,
        mark: 4.5,
        disabled: 4.5,
        boundary: 3.0,
        focus: Some(4.5),
        interactive: true,
    };

    const fn for_increase_contrast(increase_contrast: bool) -> Self {
        if increase_contrast {
            Self::INCREASED
        } else {
            Self::STANDARD
        }
    }
}

#[derive(Clone, Debug)]
struct FloatingColorResolution {
    colors: ChromeColors,
    fallback_families: Vec<FloatingControlFamily>,
}

#[derive(Clone, Debug)]
struct FloatingFieldResolution {
    reference: ChromeColors,
    colors: ChromeColors,
    fallback_families: Vec<FloatingControlFamily>,
}

#[derive(Clone, Copy)]
struct FloatingConstraint {
    proposed: Color,
    minimum: f64,
    background: FloatingConstraintBackground,
}

#[derive(Clone, Copy)]
enum FloatingConstraintBackground {
    Fill,
    Host,
}

#[derive(Clone, Copy)]
struct FloatingStateSpec {
    target_fill: Color,
    initial_alpha: u8,
    unpainted: bool,
    reference_surface: Color,
    constraints: [Option<FloatingConstraint>; 4],
}

#[derive(Clone, Copy)]
struct FloatingStateResolution {
    fill: Color,
    content: [Color; 4],
}

fn floating_constraint(proposed: Color, minimum: f64) -> Option<FloatingConstraint> {
    (proposed.a != 0).then_some(FloatingConstraint {
        proposed,
        minimum,
        background: FloatingConstraintBackground::Fill,
    })
}

fn floating_host_constraint(proposed: Color, minimum: f64) -> Option<FloatingConstraint> {
    (proposed.a != 0).then_some(FloatingConstraint {
        proposed,
        minimum,
        background: FloatingConstraintBackground::Host,
    })
}

fn resolve_host_endpoint_color(
    proposed: Color,
    host_backgrounds: [Color; 2],
    minimum: f64,
) -> Color {
    if proposed.a == 0 {
        proposed
    } else {
        readable_on_backgrounds(proposed, host_backgrounds, minimum)
    }
}

fn floating_state(
    reference_fill: Color,
    paint_fill: Color,
    reference_surface: Color,
    constraints: [Option<FloatingConstraint>; 4],
) -> FloatingStateSpec {
    FloatingStateSpec {
        target_fill: paint_fill.source_over(reference_surface),
        initial_alpha: paint_fill.a,
        unpainted: reference_fill.a == 0 && paint_fill.a == 0,
        reference_surface,
        constraints,
    }
}

fn linear_channel(channel: u8) -> f64 {
    let value = f64::from(channel) / 255.0;
    if value <= 0.04045 {
        value / 12.92
    } else {
        ((value + 0.055) / 1.055).powf(2.4)
    }
}

fn srgb_channel(value: f64) -> u8 {
    let value = if value <= 0.003_130_8 {
        12.92 * value
    } else {
        1.055 * value.powf(1.0 / 2.4) - 0.055
    };
    (value * 255.0).round().clamp(0.0, 255.0) as u8
}

fn relative_luminance(color: Color) -> f64 {
    0.2126 * linear_channel(color.r)
        + 0.7152 * linear_channel(color.g)
        + 0.0722 * linear_channel(color.b)
}

/// Reapplies an authored fill's luminance step from the definition root to an immediate host.
///
/// The returned color is an opaque semantic target. Transparent fills remain unpainted at the
/// caller. A target outside the displayable luminance interval is reported instead of clamped,
/// because clamping would claim to preserve a relationship that the color gamut cannot represent.
fn host_relative_fill(authored: Color, root: Color, host: Color) -> Option<Color> {
    let root = root.with_alpha(255);
    let host = host.with_alpha(255);
    let authored = authored.source_over(root);
    let authored_luminance = relative_luminance(authored);
    let ratio = (authored_luminance + 0.05) / (relative_luminance(root) + 0.05);
    let target_luminance = (relative_luminance(host) + 0.05) * ratio - 0.05;
    if !(0.0..=1.0).contains(&target_luminance) {
        return None;
    }

    let authored_linear = [
        linear_channel(authored.r),
        linear_channel(authored.g),
        linear_channel(authored.b),
    ];
    let mut target_linear = if authored_luminance == 0.0 {
        [target_luminance; 3]
    } else {
        let scale = target_luminance / authored_luminance;
        authored_linear.map(|channel| channel * scale)
    };
    if target_linear.into_iter().any(|channel| channel > 1.0) {
        target_linear = target_linear.map(|channel| channel.min(1.0));
        let clamped_luminance =
            0.2126 * target_linear[0] + 0.7152 * target_linear[1] + 0.0722 * target_linear[2];
        if clamped_luminance > target_luminance + f64::EPSILON {
            return None;
        }
        let remaining = 1.0 - clamped_luminance;
        let white_mix = if remaining == 0.0 {
            0.0
        } else {
            (target_luminance - clamped_luminance) / remaining
        };
        target_linear = target_linear.map(|channel| channel + (1.0 - channel) * white_mix);
    }

    Some(Color::from_rgb_components(
        srgb_channel(target_linear[0]),
        srgb_channel(target_linear[1]),
        srgb_channel(target_linear[2]),
    ))
}

fn rehost_floating_states<const N: usize>(
    mut states: [FloatingStateSpec; N],
    authored_fills: [Color; N],
    authoring_root: Color,
    host: Color,
) -> Result<[FloatingStateSpec; N], FloatingFamilyFailure> {
    for (state, authored) in states.iter_mut().zip(authored_fills) {
        if state.unpainted {
            continue;
        }
        state.target_fill = host_relative_fill(authored, authoring_root, host)
            .ok_or(FloatingFamilyFailure::UnrepresentableHostStep)?;
        state.reference_surface = host;
        state.initial_alpha = (0..=255)
            .find(|&alpha| equivalent_overlay(state.target_fill, host, alpha).is_some())
            .ok_or(FloatingFamilyFailure::UnrepresentableHostStep)?;
    }
    Ok(states)
}

/// Returns an overlay with `alpha` that reconstructs `target` over the opaque semantic `base`.
fn equivalent_overlay(target: Color, base: Color, alpha: u8) -> Option<Color> {
    if alpha == 0 {
        return (target == base).then_some(Color::rgba(0));
    }
    let alpha_fraction = f64::from(alpha) / 255.0;
    let target_channels = [target.r, target.g, target.b];
    let base_channels = [base.r, base.g, base.b];
    let mut ink = [0_u8; 3];
    for (index, (&target_channel, &base_channel)) in
        target_channels.iter().zip(&base_channels).enumerate()
    {
        let channel = (f64::from(target_channel)
            - (1.0 - alpha_fraction) * f64::from(base_channel))
            / alpha_fraction;
        if !(-0.5..=255.5).contains(&channel) {
            return None;
        }
        ink[index] = channel.round().clamp(0.0, 255.0) as u8;
    }
    let overlay = Color::from_rgb_components(ink[0], ink[1], ink[2]).with_alpha(alpha);
    let reconstructed = overlay.source_over(base);
    [reconstructed.r, reconstructed.g, reconstructed.b]
        .into_iter()
        .zip(target_channels)
        .all(|(actual, expected)| actual.abs_diff(expected) <= 1)
        .then_some(overlay)
}

fn resolve_floating_state_at_alpha(
    state: FloatingStateSpec,
    alpha: u8,
    host_backgrounds: [Color; 2],
) -> Option<FloatingStateResolution> {
    resolve_floating_state_at_alpha_with_content_fallback(state, alpha, host_backgrounds, false)
}

fn resolve_floating_state_at_alpha_with_content_fallback(
    state: FloatingStateSpec,
    alpha: u8,
    host_backgrounds: [Color; 2],
    allow_content_fallback: bool,
) -> Option<FloatingStateResolution> {
    let fill = if state.unpainted {
        Color::rgba(0)
    } else {
        equivalent_overlay(state.target_fill, state.reference_surface, alpha)?
    };
    let fill_backgrounds = host_backgrounds.map(|background| fill.source_over(background));
    let fill_constraints_need_adjustment = !state.unpainted
        && state.constraints.into_iter().flatten().any(|constraint| {
            matches!(constraint.background, FloatingConstraintBackground::Fill)
                && !content_meets_backgrounds(
                    constraint.proposed,
                    fill_backgrounds,
                    constraint.minimum,
                )
        });
    let authored_fill_endpoint = state
        .constraints
        .into_iter()
        .flatten()
        .find(|constraint| matches!(constraint.background, FloatingConstraintBackground::Fill))
        .map(|constraint| {
            if content_is_lighter_than_background(constraint.proposed, state.target_fill) {
                Color::rgb(0xffffff)
            } else {
                Color::rgb(0x000000)
            }
        });
    let mut content = [Color::rgba(0); 4];
    for (index, constraint) in state.constraints.into_iter().enumerate() {
        let Some(constraint) = constraint else {
            continue;
        };
        let backgrounds = match constraint.background {
            FloatingConstraintBackground::Fill => fill_backgrounds,
            FloatingConstraintBackground::Host => host_backgrounds,
        };
        let resolved = if matches!(constraint.background, FloatingConstraintBackground::Fill)
            && !state.unpainted
        {
            if fill_constraints_need_adjustment {
                let same_polarity = readable_toward_endpoint(
                    constraint.proposed,
                    authored_fill_endpoint?,
                    backgrounds,
                    constraint.minimum,
                );
                if content_meets_backgrounds(same_polarity, backgrounds, constraint.minimum) {
                    same_polarity
                } else if allow_content_fallback {
                    readable_toward_endpoint(
                        constraint.proposed,
                        preferred_readable_endpoint(backgrounds),
                        backgrounds,
                        constraint.minimum,
                    )
                } else {
                    return None;
                }
            } else if content_meets_backgrounds(
                constraint.proposed,
                backgrounds,
                constraint.minimum,
            ) {
                constraint.proposed
            } else {
                return None;
            }
        } else {
            readable_on_backgrounds(constraint.proposed, backgrounds, constraint.minimum)
        };
        if !content_meets_backgrounds(resolved, backgrounds, constraint.minimum) {
            return None;
        }
        content[index] = resolved;
    }
    Some(FloatingStateResolution { fill, content })
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum FloatingFamilyFailure {
    ReferenceOrder,
    NoTranslucentSolution,
    UnrepresentableHostStep,
}

fn interaction_relationships<const N: usize>(
    states: &[FloatingStateSpec; N],
    order: [usize; 3],
) -> Option<[std::cmp::Ordering; 3]> {
    let luminance = order.map(|index| relative_luminance(states[index].target_fill));
    let compare = |left: usize, right: usize| luminance[left].partial_cmp(&luminance[right]);
    let relationships = [compare(0, 1)?, compare(0, 2)?, compare(1, 2)?];
    relationships
        .into_iter()
        .all(|relationship| relationship != std::cmp::Ordering::Equal)
        .then_some(relationships)
}

fn interaction_order_holds(
    states: &[FloatingStateResolution],
    host_backgrounds: [Color; 2],
    order: [usize; 3],
    relationships: [std::cmp::Ordering; 3],
) -> bool {
    host_backgrounds.into_iter().all(|host| {
        let fills = order.map(|index| states[index].fill.source_over(host));
        let luminance = fills.map(relative_luminance);
        let comparisons = [
            luminance[0].partial_cmp(&luminance[1]),
            luminance[0].partial_cmp(&luminance[2]),
            luminance[1].partial_cmp(&luminance[2]),
        ];
        comparisons == relationships.map(Some)
    })
}

fn resolve_floating_family<const N: usize>(
    states: [FloatingStateSpec; N],
    host_backgrounds: [Color; 2],
    orders: &[[usize; 3]],
) -> Result<Vec<FloatingStateResolution>, FloatingFamilyFailure> {
    let relationships: Vec<_> = orders
        .iter()
        .map(|&order| interaction_relationships(&states, order))
        .collect::<Option<_>>()
        .ok_or(FloatingFamilyFailure::ReferenceOrder)?;
    let first_alpha = states
        .iter()
        .filter(|state| !state.unpainted)
        .map(|state| state.initial_alpha)
        .max()
        .unwrap_or(0);
    for alpha in first_alpha..=254 {
        let Some(resolved) = states
            .iter()
            .map(|&state| resolve_floating_state_at_alpha(state, alpha, host_backgrounds))
            .collect::<Option<Vec<_>>>()
        else {
            continue;
        };
        if orders
            .iter()
            .zip(&relationships)
            .all(|(&order, &relationship)| {
                interaction_order_holds(&resolved, host_backgrounds, order, relationship)
            })
        {
            return Ok(resolved);
        }
    }
    Err(FloatingFamilyFailure::NoTranslucentSolution)
}

fn resolve_floating_family_for_presentation<const N: usize>(
    states: Result<[FloatingStateSpec; N], FloatingFamilyFailure>,
    host_backgrounds: [Color; 2],
    orders: &[[usize; 3]],
) -> Result<(Vec<FloatingStateResolution>, bool), FloatingFamilyFailure> {
    let states = states?;
    if host_backgrounds[0] != host_backgrounds[1]
        && let Ok(resolved) = resolve_floating_family(states, host_backgrounds, orders)
    {
        return Ok((resolved, false));
    }

    let resolved = states
        .iter()
        .copied()
        .map(|state| {
            resolve_floating_state_at_alpha_with_content_fallback(
                state,
                255,
                host_backgrounds,
                true,
            )
        })
        .collect::<Option<Vec<_>>>()
        .ok_or(FloatingFamilyFailure::NoTranslucentSolution)?;
    let used_content_fallback = states.iter().zip(&resolved).any(|(state, resolved)| {
        let background = state.target_fill;
        state
            .constraints
            .into_iter()
            .zip(resolved.content)
            .any(|(constraint, resolved)| {
                constraint.is_some_and(|constraint| {
                    matches!(constraint.background, FloatingConstraintBackground::Fill)
                        && content_is_lighter_than_background(constraint.proposed, background)
                            != content_is_lighter_than_background(resolved, background)
                })
            })
    });
    Ok((
        resolved,
        host_backgrounds[0] != host_backgrounds[1] || used_content_fallback,
    ))
}

#[cfg(test)]
fn resolve_floating_control_colors(
    authored: &ChromeColors,
    floors: FloatingContrastFloors,
    reference: &ChromeColors,
    paint: ChromeColors,
    material: Color,
    wash: Color,
) -> ChromeColors {
    let resolved = resolve_floating_control_colors_detailed(
        authored, floors, reference, paint, material, wash,
    );
    let _pending_diagnostics = resolved.fallback_families;
    resolved.colors
}

fn resolve_floating_control_colors_detailed(
    authored: &ChromeColors,
    floors: FloatingContrastFloors,
    reference: &ChromeColors,
    mut paint: ChromeColors,
    material: Color,
    wash: Color,
) -> FloatingColorResolution {
    let host_backgrounds = [Color::rgb(0x000000), Color::rgb(0xffffff)]
        .map(|underlay| wash.source_over(material.source_over(underlay)));
    if let Some(focus_floor) = floors.focus {
        paint.focus_ring =
            resolve_host_endpoint_color(paint.focus_ring, host_backgrounds, focus_floor);
    }
    let mut fallback_families = Vec::new();
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
    macro_rules! framed_state {
        ($fill:ident, $minimum:expr, $foreground:ident, $icon:ident, $border:ident) => {{
            let (fill, [foreground, icon], border) = resolve_floating_frame_with_boundary(
                reference.$fill,
                paint.$fill,
                reference.elevated_surface_background,
                host_backgrounds,
                [
                    (reference.$foreground, $minimum),
                    (reference.$icon, $minimum),
                ],
                paint.$border,
                floors.boundary,
            );
            paint.$fill = fill;
            paint.$foreground = foreground;
            paint.$icon = icon;
            paint.$border = border;
        }};
    }
    let element_state = |reference_fill, paint_fill, foreground, icon, border, content_floor| {
        floating_state(
            reference_fill,
            paint_fill,
            reference.elevated_surface_background,
            [
                floating_constraint(foreground, content_floor),
                floating_constraint(icon, content_floor),
                floating_host_constraint(border, floors.boundary),
                None,
            ],
        )
    };
    let element_states = rehost_floating_states(
        [
            element_state(
                reference.element_background,
                paint.element_background,
                reference.element_foreground,
                reference.element_icon,
                paint.element_border,
                floors.primary,
            ),
            element_state(
                reference.element_hover,
                paint.element_hover,
                reference.element_hover_foreground,
                reference.element_hover_icon,
                paint.element_hover_border,
                floors.primary,
            ),
            element_state(
                reference.element_active,
                paint.element_active,
                reference.element_active_foreground,
                reference.element_active_icon,
                paint.element_active_border,
                floors.primary,
            ),
            element_state(
                reference.element_disabled,
                paint.element_disabled,
                reference.element_disabled_foreground,
                reference.element_disabled_icon,
                paint.element_disabled_border,
                floors.disabled,
            ),
        ],
        [
            authored.element_background,
            authored.element_hover,
            authored.element_active,
            authored.element_disabled,
        ],
        authored.background,
        reference.elevated_surface_background,
    );
    if let Ok((states, used_opaque_fallback)) = resolve_floating_family_for_presentation(
        element_states,
        host_backgrounds,
        if floors.interactive {
            &[[0, 1, 2]]
        } else {
            &[]
        },
    ) {
        if used_opaque_fallback {
            fallback_families.push(FloatingControlFamily::Element);
        }
        paint.element_background = states[0].fill;
        paint.element_foreground = states[0].content[0];
        paint.element_icon = states[0].content[1];
        paint.element_border = states[0].content[2];
        paint.element_hover = states[1].fill;
        paint.element_hover_foreground = states[1].content[0];
        paint.element_hover_icon = states[1].content[1];
        paint.element_hover_border = states[1].content[2];
        paint.element_active = states[2].fill;
        paint.element_active_foreground = states[2].content[0];
        paint.element_active_icon = states[2].content[1];
        paint.element_active_border = states[2].content[2];
        paint.element_disabled = states[3].fill;
        paint.element_disabled_foreground = states[3].content[0];
        paint.element_disabled_icon = states[3].content[1];
        paint.element_disabled_border = states[3].content[2];
    } else {
        fallback_families.push(FloatingControlFamily::Element);
        framed_state!(
            element_background,
            floors.primary,
            element_foreground,
            element_icon,
            element_border
        );
        framed_state!(
            element_hover,
            floors.primary,
            element_hover_foreground,
            element_hover_icon,
            element_hover_border
        );
        framed_state!(
            element_active,
            floors.primary,
            element_active_foreground,
            element_active_icon,
            element_active_border
        );
        framed_state!(
            element_disabled,
            floors.disabled,
            element_disabled_foreground,
            element_disabled_icon,
            element_disabled_border
        );
    }

    let ghost_state = |reference_fill, paint_fill, foreground, icon, border, content_floor| {
        floating_state(
            reference_fill,
            paint_fill,
            reference.elevated_surface_background,
            [
                floating_constraint(foreground, content_floor),
                floating_constraint(icon, content_floor),
                floating_host_constraint(border, floors.boundary),
                None,
            ],
        )
    };
    let ghost_states = rehost_floating_states(
        [
            ghost_state(
                reference.ghost_element_background,
                paint.ghost_element_background,
                reference.ghost_element_foreground,
                reference.ghost_element_icon,
                paint.ghost_element_border,
                floors.primary,
            ),
            ghost_state(
                reference.ghost_element_hover,
                paint.ghost_element_hover,
                reference.ghost_element_hover_foreground,
                reference.ghost_element_hover_icon,
                paint.ghost_element_hover_border,
                floors.primary,
            ),
            ghost_state(
                reference.ghost_element_active,
                paint.ghost_element_active,
                reference.ghost_element_active_foreground,
                reference.ghost_element_active_icon,
                paint.ghost_element_active_border,
                floors.primary,
            ),
            ghost_state(
                reference.ghost_element_disabled,
                paint.ghost_element_disabled,
                reference.ghost_element_disabled_foreground,
                reference.ghost_element_disabled_icon,
                paint.ghost_element_disabled_border,
                floors.disabled,
            ),
        ],
        [
            authored.ghost_element_background,
            authored.ghost_element_hover,
            authored.ghost_element_active,
            authored.ghost_element_disabled,
        ],
        authored.background,
        reference.elevated_surface_background,
    );
    if let Ok((states, used_opaque_fallback)) = resolve_floating_family_for_presentation(
        ghost_states,
        host_backgrounds,
        if floors.interactive {
            &[[0, 1, 2]]
        } else {
            &[]
        },
    ) {
        if used_opaque_fallback {
            fallback_families.push(FloatingControlFamily::GhostElement);
        }
        paint.ghost_element_background = states[0].fill;
        paint.ghost_element_foreground = states[0].content[0];
        paint.ghost_element_icon = states[0].content[1];
        paint.ghost_element_border = states[0].content[2];
        paint.ghost_element_hover = states[1].fill;
        paint.ghost_element_hover_foreground = states[1].content[0];
        paint.ghost_element_hover_icon = states[1].content[1];
        paint.ghost_element_hover_border = states[1].content[2];
        paint.ghost_element_active = states[2].fill;
        paint.ghost_element_active_foreground = states[2].content[0];
        paint.ghost_element_active_icon = states[2].content[1];
        paint.ghost_element_active_border = states[2].content[2];
        paint.ghost_element_disabled = states[3].fill;
        paint.ghost_element_disabled_foreground = states[3].content[0];
        paint.ghost_element_disabled_icon = states[3].content[1];
        paint.ghost_element_disabled_border = states[3].content[2];
    } else {
        fallback_families.push(FloatingControlFamily::GhostElement);
        framed_state!(
            ghost_element_background,
            floors.primary,
            ghost_element_foreground,
            ghost_element_icon,
            ghost_element_border
        );
        framed_state!(
            ghost_element_hover,
            floors.primary,
            ghost_element_hover_foreground,
            ghost_element_hover_icon,
            ghost_element_hover_border
        );
        framed_state!(
            ghost_element_active,
            floors.primary,
            ghost_element_active_foreground,
            ghost_element_active_icon,
            ghost_element_active_border
        );
        framed_state!(
            ghost_element_disabled,
            floors.disabled,
            ghost_element_disabled_foreground,
            ghost_element_disabled_icon,
            ghost_element_disabled_border
        );
    }
    state!(
        ghost_element_selected,
        floors.primary,
        ghost_element_selected_foreground
    );
    state!(
        primary_background,
        floors.primary,
        primary_foreground,
        primary_icon
    );
    state!(
        primary_hover_background,
        floors.primary,
        primary_hover_foreground,
        primary_hover_icon
    );
    state!(
        primary_pressed_background,
        floors.primary,
        primary_pressed_foreground,
        primary_pressed_icon
    );
    state!(
        primary_disabled_background,
        floors.disabled,
        primary_disabled_foreground,
        primary_disabled_icon
    );
    state!(
        destructive_background,
        floors.primary,
        destructive_foreground,
        destructive_icon
    );
    state!(
        destructive_hover_background,
        floors.primary,
        destructive_hover_foreground,
        destructive_hover_icon
    );
    state!(
        destructive_pressed_background,
        floors.primary,
        destructive_pressed_foreground,
        destructive_pressed_icon
    );
    state!(
        destructive_disabled_background,
        floors.disabled,
        destructive_disabled_foreground,
        destructive_disabled_icon
    );
    state!(
        selection_background,
        floors.primary,
        selection_foreground,
        selection_icon
    );
    state!(
        selection_hover_background,
        floors.primary,
        selection_hover_foreground,
        selection_hover_icon
    );
    state!(
        selection_pressed_background,
        floors.primary,
        selection_pressed_foreground,
        selection_pressed_icon
    );
    state!(
        selection_disabled_background,
        floors.disabled,
        selection_disabled_foreground,
        selection_disabled_icon
    );
    if !floors.interactive {
        paint.selection_background = paint.element_background;
        paint.selection_foreground = paint.element_foreground;
        paint.selection_icon = paint.element_icon;
        paint.selection_border = paint.element_border;
        paint.selection_hover_background = paint.element_background;
        paint.selection_hover_foreground = paint.element_foreground;
        paint.selection_hover_icon = paint.element_icon;
        paint.selection_hover_border = paint.element_border;
        paint.selection_pressed_background = paint.element_background;
        paint.selection_pressed_foreground = paint.element_foreground;
        paint.selection_pressed_icon = paint.element_icon;
        paint.selection_pressed_border = paint.element_border;
    }
    let (toggle_off_background, text_accent) = resolve_floating_progress_accent(
        reference.toggle_off_background,
        paint.toggle_off_background,
        reference.elevated_surface_background,
        host_backgrounds,
        reference.text_accent,
        floors.primary,
    );
    paint.toggle_off_background = toggle_off_background;
    paint.text_accent = text_accent;

    let toggle_state =
        |reference_fill, paint_fill, mark, label, border, mark_floor, label_floor| {
            floating_state(
                reference_fill,
                paint_fill,
                reference.elevated_surface_background,
                [
                    floating_constraint(mark, mark_floor),
                    floating_host_constraint(label, label_floor),
                    floating_host_constraint(border, floors.boundary),
                    None,
                ],
            )
        };
    let toggle_states = rehost_floating_states(
        [
            toggle_state(
                reference.toggle_off_background,
                paint.toggle_off_background,
                reference.toggle_off_mark,
                reference.toggle_off_label,
                paint.toggle_off_border,
                floors.mark,
                floors.primary,
            ),
            toggle_state(
                reference.toggle_off_hover_background,
                paint.toggle_off_hover_background,
                reference.toggle_off_hover_mark,
                reference.toggle_off_hover_label,
                paint.toggle_off_hover_border,
                floors.mark,
                floors.primary,
            ),
            toggle_state(
                reference.toggle_off_pressed_background,
                paint.toggle_off_pressed_background,
                reference.toggle_off_pressed_mark,
                reference.toggle_off_pressed_label,
                paint.toggle_off_pressed_border,
                floors.mark,
                floors.primary,
            ),
            toggle_state(
                reference.toggle_off_disabled_background,
                paint.toggle_off_disabled_background,
                reference.toggle_off_disabled_mark,
                reference.toggle_off_disabled_label,
                paint.toggle_off_disabled_border,
                floors.disabled,
                floors.disabled,
            ),
            toggle_state(
                reference.toggle_on_background,
                paint.toggle_on_background,
                reference.toggle_on_mark,
                reference.toggle_on_label,
                paint.toggle_on_border,
                floors.mark,
                floors.primary,
            ),
            toggle_state(
                reference.toggle_on_hover_background,
                paint.toggle_on_hover_background,
                reference.toggle_on_hover_mark,
                reference.toggle_on_hover_label,
                paint.toggle_on_hover_border,
                floors.mark,
                floors.primary,
            ),
            toggle_state(
                reference.toggle_on_pressed_background,
                paint.toggle_on_pressed_background,
                reference.toggle_on_pressed_mark,
                reference.toggle_on_pressed_label,
                paint.toggle_on_pressed_border,
                floors.mark,
                floors.primary,
            ),
            toggle_state(
                reference.toggle_on_disabled_background,
                paint.toggle_on_disabled_background,
                reference.toggle_on_disabled_mark,
                reference.toggle_on_disabled_label,
                paint.toggle_on_disabled_border,
                floors.disabled,
                floors.disabled,
            ),
        ],
        [
            authored.toggle_off_background,
            authored.toggle_off_hover_background,
            authored.toggle_off_pressed_background,
            authored.toggle_off_disabled_background,
            authored.toggle_on_background,
            authored.toggle_on_hover_background,
            authored.toggle_on_pressed_background,
            authored.toggle_on_disabled_background,
        ],
        authored.background,
        reference.elevated_surface_background,
    );
    macro_rules! assign_toggle_state {
        ($states:ident, $index:expr, $fill:ident, $mark:ident, $label:ident, $border:ident) => {{
            paint.$fill = $states[$index].fill;
            paint.$mark = $states[$index].content[0];
            paint.$label = $states[$index].content[1];
            paint.$border = $states[$index].content[2];
        }};
    }
    let toggle_resolution = resolve_floating_family_for_presentation(
        toggle_states,
        host_backgrounds,
        if floors.interactive {
            &[[0, 1, 2], [4, 5, 6]]
        } else {
            &[]
        },
    );
    macro_rules! legacy_toggle_family {
        () => {{
            macro_rules! toggle_state_with_boundary {
                ($fill:ident, $minimum:expr, $mark:ident, $border:ident) => {{
                    let (fill, [mark], border) = resolve_floating_frame_with_boundary(
                        reference.$fill,
                        paint.$fill,
                        reference.elevated_surface_background,
                        host_backgrounds,
                        [(reference.$mark, $minimum)],
                        paint.$border,
                        floors.boundary,
                    );
                    paint.$fill = fill;
                    paint.$mark = mark;
                    paint.$border = border;
                }};
            }
            toggle_state_with_boundary!(
                toggle_off_background,
                floors.mark,
                toggle_off_mark,
                toggle_off_border
            );
            toggle_state_with_boundary!(
                toggle_off_hover_background,
                floors.mark,
                toggle_off_hover_mark,
                toggle_off_hover_border
            );
            toggle_state_with_boundary!(
                toggle_off_pressed_background,
                floors.mark,
                toggle_off_pressed_mark,
                toggle_off_pressed_border
            );
            toggle_state_with_boundary!(
                toggle_off_disabled_background,
                floors.disabled,
                toggle_off_disabled_mark,
                toggle_off_disabled_border
            );
            toggle_state_with_boundary!(
                toggle_on_background,
                floors.mark,
                toggle_on_mark,
                toggle_on_border
            );
            toggle_state_with_boundary!(
                toggle_on_hover_background,
                floors.mark,
                toggle_on_hover_mark,
                toggle_on_hover_border
            );
            toggle_state_with_boundary!(
                toggle_on_pressed_background,
                floors.mark,
                toggle_on_pressed_mark,
                toggle_on_pressed_border
            );
            toggle_state_with_boundary!(
                toggle_on_disabled_background,
                floors.disabled,
                toggle_on_disabled_mark,
                toggle_on_disabled_border
            );
            for (target, proposed, minimum) in [
                (
                    &mut paint.toggle_off_label,
                    reference.toggle_off_label,
                    floors.primary,
                ),
                (
                    &mut paint.toggle_off_hover_label,
                    reference.toggle_off_hover_label,
                    floors.primary,
                ),
                (
                    &mut paint.toggle_off_pressed_label,
                    reference.toggle_off_pressed_label,
                    floors.primary,
                ),
                (
                    &mut paint.toggle_off_disabled_label,
                    reference.toggle_off_disabled_label,
                    floors.disabled,
                ),
                (
                    &mut paint.toggle_on_label,
                    reference.toggle_on_label,
                    floors.primary,
                ),
                (
                    &mut paint.toggle_on_hover_label,
                    reference.toggle_on_hover_label,
                    floors.primary,
                ),
                (
                    &mut paint.toggle_on_pressed_label,
                    reference.toggle_on_pressed_label,
                    floors.primary,
                ),
                (
                    &mut paint.toggle_on_disabled_label,
                    reference.toggle_on_disabled_label,
                    floors.disabled,
                ),
            ] {
                *target = readable_on_backgrounds(proposed, host_backgrounds, minimum);
            }
        }};
    }
    match toggle_resolution {
        Ok((states, used_opaque_fallback)) => {
            if used_opaque_fallback {
                fallback_families.push(FloatingControlFamily::Toggle);
            }
            assign_toggle_state!(
                states,
                0,
                toggle_off_background,
                toggle_off_mark,
                toggle_off_label,
                toggle_off_border
            );
            assign_toggle_state!(
                states,
                1,
                toggle_off_hover_background,
                toggle_off_hover_mark,
                toggle_off_hover_label,
                toggle_off_hover_border
            );
            assign_toggle_state!(
                states,
                2,
                toggle_off_pressed_background,
                toggle_off_pressed_mark,
                toggle_off_pressed_label,
                toggle_off_pressed_border
            );
            assign_toggle_state!(
                states,
                3,
                toggle_off_disabled_background,
                toggle_off_disabled_mark,
                toggle_off_disabled_label,
                toggle_off_disabled_border
            );
            assign_toggle_state!(
                states,
                4,
                toggle_on_background,
                toggle_on_mark,
                toggle_on_label,
                toggle_on_border
            );
            assign_toggle_state!(
                states,
                5,
                toggle_on_hover_background,
                toggle_on_hover_mark,
                toggle_on_hover_label,
                toggle_on_hover_border
            );
            assign_toggle_state!(
                states,
                6,
                toggle_on_pressed_background,
                toggle_on_pressed_mark,
                toggle_on_pressed_label,
                toggle_on_pressed_border
            );
            assign_toggle_state!(
                states,
                7,
                toggle_on_disabled_background,
                toggle_on_disabled_mark,
                toggle_on_disabled_label,
                toggle_on_disabled_border
            );
        }
        Err(_) => {
            fallback_families.push(FloatingControlFamily::Toggle);
            legacy_toggle_family!();
        }
    }
    let (toggle_off_background, text_accent) = resolve_floating_progress_accent(
        reference.toggle_off_background,
        paint.toggle_off_background,
        reference.elevated_surface_background,
        host_backgrounds,
        reference.text_accent,
        floors.primary,
    );
    paint.toggle_off_background = toggle_off_background;
    paint.text_accent = text_accent;
    for (target, proposed, minimum) in [
        (&mut paint.link_text, reference.link_text, floors.primary),
        (
            &mut paint.link_text_hover,
            reference.link_text_hover,
            floors.primary,
        ),
        (
            &mut paint.link_text_pressed,
            reference.link_text_pressed,
            floors.primary,
        ),
        (
            &mut paint.link_text_disabled,
            reference.link_text_disabled,
            floors.disabled,
        ),
    ] {
        *target = readable_on_backgrounds(proposed, host_backgrounds, minimum);
    }
    let (progress_track, progress_indicator) = resolve_floating_progress_accent(
        reference.progress_track,
        paint.progress_track,
        reference.elevated_surface_background,
        host_backgrounds,
        reference.progress_indicator,
        floors.secondary,
    );
    paint.progress_track = progress_track;
    paint.progress_indicator = progress_indicator;
    paint.info = readable_on_backgrounds(reference.info, host_backgrounds, floors.boundary);
    paint.info_border =
        readable_on_backgrounds(reference.info_border, host_backgrounds, floors.boundary);
    paint.success = readable_on_backgrounds(reference.success, host_backgrounds, floors.boundary);
    paint.success_border =
        readable_on_backgrounds(reference.success_border, host_backgrounds, floors.boundary);
    paint.warning = readable_on_backgrounds(reference.warning, host_backgrounds, floors.boundary);
    paint.warning_border =
        readable_on_backgrounds(reference.warning_border, host_backgrounds, floors.boundary);
    paint.error = readable_on_backgrounds(reference.error, host_backgrounds, floors.boundary);
    paint.error_border =
        readable_on_backgrounds(reference.error_border, host_backgrounds, floors.boundary);
    FloatingColorResolution {
        colors: paint,
        fallback_families,
    }
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
    let exact_content = proposed.map(|(color, _)| color);
    let backgrounds = host_backgrounds.map(|background| paint_fill.source_over(background));
    if proposed
        .into_iter()
        .all(|(color, minimum)| content_meets_backgrounds(color, backgrounds, minimum))
    {
        return (paint_fill, exact_content);
    }
    let unpainted = reference_fill.a == 0 && paint_fill.a == 0;
    if !unpainted {
        let target = reference_fill.source_over(reference_surface);
        let endpoint = proposed
            .first()
            .map_or(Color::rgb(0x000000), |(content, _)| {
                if content_is_lighter_than_background(*content, target) {
                    Color::rgb(0xffffff)
                } else {
                    Color::rgb(0x000000)
                }
            });
        for alpha in paint_fill.a..=255 {
            let Some(fill) = equivalent_overlay(target, reference_surface, alpha) else {
                continue;
            };
            let backgrounds = host_backgrounds.map(|background| fill.source_over(background));
            let content = proposed.map(|(color, minimum)| {
                readable_toward_endpoint(color, endpoint, backgrounds, minimum)
            });
            if content.iter().zip(proposed).all(|(color, (_, minimum))| {
                content_meets_backgrounds(*color, backgrounds, minimum)
            }) {
                return (fill, content);
            }
        }
    }
    let fallback = if unpainted {
        Color::rgba(0)
    } else {
        reference_fill.source_over(reference_surface)
    };
    let fallback_backgrounds = if unpainted {
        host_backgrounds
    } else {
        [fallback; 2]
    };
    if let Some(content) = resolve_content_on_backgrounds(proposed, fallback_backgrounds) {
        return (fallback, content);
    }
    if unpainted {
        return (
            fallback,
            proposed.map(|(color, minimum)| {
                readable_on_backgrounds(color, fallback_backgrounds, minimum)
            }),
        );
    }

    let minimum = proposed
        .into_iter()
        .map(|(_, minimum)| minimum)
        .fold(0.0, f64::max);
    let fallback = super::chrome_state::contrast_host(fallback, minimum);
    (
        fallback,
        proposed.map(|(color, minimum)| readable_on_backgrounds(color, [fallback; 2], minimum)),
    )
}

fn resolve_floating_frame_with_boundary<const N: usize>(
    reference_fill: Color,
    paint_fill: Color,
    reference_surface: Color,
    host_backgrounds: [Color; 2],
    proposed: [(Color, f64); N],
    proposed_border: Color,
    boundary_floor: f64,
) -> (Color, [Color; N], Color) {
    if proposed_border.a == 0 {
        let (fill, content) = resolve_floating_frame(
            reference_fill,
            paint_fill,
            reference_surface,
            host_backgrounds,
            proposed,
        );
        return (fill, content, proposed_border);
    }

    let fill_backgrounds = host_backgrounds.map(|background| paint_fill.source_over(background));
    if proposed
        .into_iter()
        .all(|(color, minimum)| content_meets_backgrounds(color, fill_backgrounds, minimum))
        && let Some([border]) =
            resolve_content_on_backgrounds([(proposed_border, boundary_floor)], host_backgrounds)
    {
        return (paint_fill, proposed.map(|(color, _)| color), border);
    }

    let (fill, content) = resolve_floating_frame(
        reference_fill,
        paint_fill,
        reference_surface,
        host_backgrounds,
        proposed,
    );
    (
        fill,
        content,
        readable_on_backgrounds(proposed_border, host_backgrounds, boundary_floor),
    )
}

fn resolve_floating_progress_accent(
    reference_fill: Color,
    paint_fill: Color,
    reference_surface: Color,
    host_backgrounds: [Color; 2],
    proposed: Color,
    minimum: f64,
) -> (Color, Color) {
    let progress_backgrounds =
        host_backgrounds.map(|background| paint_fill.source_over(background));
    let backgrounds = [
        host_backgrounds[0],
        host_backgrounds[1],
        progress_backgrounds[0],
        progress_backgrounds[1],
    ];
    if let Some([resolved]) = resolve_content_on_backgrounds([(proposed, minimum)], backgrounds) {
        return (paint_fill, resolved);
    }

    let fallback = reference_fill.source_over(reference_surface);
    let fallback_backgrounds = [host_backgrounds[0], host_backgrounds[1], fallback, fallback];
    if let Some([resolved]) =
        resolve_content_on_backgrounds([(proposed, minimum)], fallback_backgrounds)
    {
        return (fallback, resolved);
    }

    if let Some([resolved]) =
        resolve_content_on_backgrounds([(proposed, minimum)], host_backgrounds)
    {
        let frame = if resolved
            .source_over(reference_surface)
            .contrast_ratio(reference_surface)
            >= minimum
        {
            reference_surface
        } else {
            host_backgrounds[0]
        };
        return (frame, resolved);
    }

    let resolved = readable_on_backgrounds(proposed, [fallback; 2], minimum);
    (fallback, resolved)
}

/// Resolves controls after their material fills have been expressed as overlays on a known host.
///
/// The opaque reference remains the safe fallback. A prepared overlay that already carries every
/// required content color is retained byte-for-byte; otherwise only the prepared presentation is
/// made opaque enough to carry that content.
fn resolve_material_control_colors(
    reference: &ChromeColors,
    mut paint: ChromeColors,
    reference_surface: Color,
    host: Color,
    floors: FloatingContrastFloors,
    accessible_boundaries: bool,
) -> ChromeColors {
    let hosts = [host; 2];

    macro_rules! on_host {
        ($minimum:expr; $($role:ident),+ $(,)?) => { $(
            paint.$role = readable_on_backgrounds(reference.$role, hosts, $minimum);
        )+ };
    }
    on_host!(floors.primary; text, text_accent, icon, link_text, link_text_hover, link_text_pressed);
    on_host!(floors.secondary; text_secondary, text_muted, text_placeholder, icon_muted);
    on_host!(floors.disabled; text_disabled, icon_disabled, link_text_disabled);

    macro_rules! framed_state {
        ($fill:ident, $minimum:expr, [$($content:ident),+ $(,)?], $border:ident) => {{
            if accessible_boundaries {
                let (fill, [$($content),+], border) = resolve_floating_frame_with_boundary(
                    reference.$fill,
                    paint.$fill,
                    reference_surface,
                    hosts,
                    [$( (reference.$content, $minimum) ),+],
                    paint.$border,
                    floors.boundary,
                );
                paint.$fill = fill;
                $(paint.$content = $content;)+
                paint.$border = border;
            } else {
                let (fill, [$($content),+]) = resolve_floating_frame(
                    reference.$fill,
                    paint.$fill,
                    reference_surface,
                    hosts,
                    [$( (reference.$content, $minimum) ),+],
                );
                paint.$fill = fill;
                $(paint.$content = $content;)+
            }
        }};
    }

    framed_state!(
        element_background,
        floors.primary,
        [element_foreground, element_icon],
        element_border
    );
    framed_state!(
        element_hover,
        floors.primary,
        [element_hover_foreground, element_hover_icon],
        element_hover_border
    );
    framed_state!(
        element_active,
        floors.primary,
        [element_active_foreground, element_active_icon],
        element_active_border
    );
    framed_state!(
        element_disabled,
        floors.disabled,
        [element_disabled_foreground, element_disabled_icon],
        element_disabled_border
    );
    framed_state!(
        ghost_element_background,
        floors.primary,
        [ghost_element_foreground, ghost_element_icon],
        ghost_element_border
    );
    framed_state!(
        ghost_element_hover,
        floors.primary,
        [ghost_element_hover_foreground, ghost_element_hover_icon],
        ghost_element_hover_border
    );
    framed_state!(
        ghost_element_active,
        floors.primary,
        [ghost_element_active_foreground, ghost_element_active_icon],
        ghost_element_active_border
    );
    framed_state!(
        ghost_element_disabled,
        floors.disabled,
        [
            ghost_element_disabled_foreground,
            ghost_element_disabled_icon
        ],
        ghost_element_disabled_border
    );
    framed_state!(
        primary_background,
        floors.primary,
        [primary_foreground, primary_icon],
        primary_border
    );
    framed_state!(
        primary_hover_background,
        floors.primary,
        [primary_hover_foreground, primary_hover_icon],
        primary_hover_border
    );
    framed_state!(
        primary_pressed_background,
        floors.primary,
        [primary_pressed_foreground, primary_pressed_icon],
        primary_pressed_border
    );
    framed_state!(
        primary_disabled_background,
        floors.disabled,
        [primary_disabled_foreground, primary_disabled_icon],
        primary_disabled_border
    );
    framed_state!(
        destructive_background,
        floors.primary,
        [destructive_foreground, destructive_icon],
        destructive_border
    );
    framed_state!(
        destructive_hover_background,
        floors.primary,
        [destructive_hover_foreground, destructive_hover_icon],
        destructive_hover_border
    );
    framed_state!(
        destructive_pressed_background,
        floors.primary,
        [destructive_pressed_foreground, destructive_pressed_icon],
        destructive_pressed_border
    );
    framed_state!(
        destructive_disabled_background,
        floors.disabled,
        [destructive_disabled_foreground, destructive_disabled_icon],
        destructive_disabled_border
    );
    framed_state!(
        selection_background,
        floors.primary,
        [selection_foreground, selection_icon],
        selection_border
    );
    framed_state!(
        selection_hover_background,
        floors.primary,
        [selection_hover_foreground, selection_hover_icon],
        selection_hover_border
    );
    framed_state!(
        selection_pressed_background,
        floors.primary,
        [selection_pressed_foreground, selection_pressed_icon],
        selection_pressed_border
    );
    framed_state!(
        selection_disabled_background,
        floors.disabled,
        [selection_disabled_foreground, selection_disabled_icon],
        selection_disabled_border
    );
    if !floors.interactive {
        paint.selection_background = paint.element_background;
        paint.selection_foreground = paint.element_foreground;
        paint.selection_icon = paint.element_icon;
        paint.selection_border = paint.element_border;
        paint.selection_hover_background = paint.element_background;
        paint.selection_hover_foreground = paint.element_foreground;
        paint.selection_hover_icon = paint.element_icon;
        paint.selection_hover_border = paint.element_border;
        paint.selection_pressed_background = paint.element_background;
        paint.selection_pressed_foreground = paint.element_foreground;
        paint.selection_pressed_icon = paint.element_icon;
        paint.selection_pressed_border = paint.element_border;
    }

    macro_rules! toggle_state {
        ($fill:ident, $mark_minimum:expr, $label_minimum:expr, $mark:ident, $label:ident, $border:ident) => {{
            if accessible_boundaries {
                let (fill, [mark], border) = resolve_floating_frame_with_boundary(
                    reference.$fill,
                    paint.$fill,
                    reference_surface,
                    hosts,
                    [(reference.$mark, $mark_minimum)],
                    paint.$border,
                    floors.boundary,
                );
                paint.$fill = fill;
                paint.$mark = mark;
                paint.$border = border;
            } else {
                let (fill, [mark]) = resolve_floating_frame(
                    reference.$fill,
                    paint.$fill,
                    reference_surface,
                    hosts,
                    [(reference.$mark, $mark_minimum)],
                );
                paint.$fill = fill;
                paint.$mark = mark;
            }
            paint.$label = readable_on_backgrounds(reference.$label, hosts, $label_minimum);
        }};
    }
    toggle_state!(
        toggle_off_background,
        floors.mark,
        floors.primary,
        toggle_off_mark,
        toggle_off_label,
        toggle_off_border
    );
    toggle_state!(
        toggle_off_hover_background,
        floors.mark,
        floors.primary,
        toggle_off_hover_mark,
        toggle_off_hover_label,
        toggle_off_hover_border
    );
    toggle_state!(
        toggle_off_pressed_background,
        floors.mark,
        floors.primary,
        toggle_off_pressed_mark,
        toggle_off_pressed_label,
        toggle_off_pressed_border
    );
    toggle_state!(
        toggle_off_disabled_background,
        floors.disabled,
        floors.disabled,
        toggle_off_disabled_mark,
        toggle_off_disabled_label,
        toggle_off_disabled_border
    );
    toggle_state!(
        toggle_on_background,
        floors.mark,
        floors.primary,
        toggle_on_mark,
        toggle_on_label,
        toggle_on_border
    );
    toggle_state!(
        toggle_on_hover_background,
        floors.mark,
        floors.primary,
        toggle_on_hover_mark,
        toggle_on_hover_label,
        toggle_on_hover_border
    );
    toggle_state!(
        toggle_on_pressed_background,
        floors.mark,
        floors.primary,
        toggle_on_pressed_mark,
        toggle_on_pressed_label,
        toggle_on_pressed_border
    );
    toggle_state!(
        toggle_on_disabled_background,
        floors.disabled,
        floors.disabled,
        toggle_on_disabled_mark,
        toggle_on_disabled_label,
        toggle_on_disabled_border
    );

    if accessible_boundaries {
        let (fill, [text, caret], border) = resolve_floating_frame_with_boundary(
            reference.input_background,
            paint.input_background,
            reference_surface,
            hosts,
            [
                (reference.input_text, floors.primary),
                (reference.input_caret, floors.boundary),
            ],
            paint.input_border,
            floors.boundary,
        );
        paint.input_background = fill;
        paint.input_text = text;
        paint.input_caret = caret;
        paint.input_border = border;
    } else {
        let (fill, [text, caret]) = resolve_floating_frame(
            reference.input_background,
            paint.input_background,
            reference_surface,
            hosts,
            [
                (reference.input_text, floors.primary),
                (reference.input_caret, floors.boundary),
            ],
        );
        paint.input_background = fill;
        paint.input_text = text;
        paint.input_caret = caret;
    }
    let input_background = paint.input_background.source_over(host);
    paint.input_placeholder = readable_on_backgrounds(
        reference.input_placeholder,
        [input_background; 2],
        floors.secondary,
    );
    framed_state!(
        input_disabled_background,
        floors.disabled,
        [input_disabled_text],
        input_disabled_border
    );
    let selection_background = paint
        .input_selection_background
        .source_over(input_background);
    paint.input_selection_foreground = readable_on_backgrounds(
        reference.input_selection_foreground,
        [selection_background; 2],
        floors.primary,
    );
    paint.input_focused_border = if floors.interactive {
        resolve_host_endpoint_color(reference.input_focused_border, hosts, floors.boundary)
    } else {
        paint.input_border
    };
    paint.input_invalid_border =
        resolve_host_endpoint_color(reference.input_invalid_border, hosts, floors.boundary);

    let (track, indicator) = resolve_floating_progress_accent(
        reference.progress_track,
        paint.progress_track,
        reference_surface,
        hosts,
        reference.progress_indicator,
        floors.secondary,
    );
    paint.progress_track = track;
    paint.progress_indicator = indicator;

    if accessible_boundaries {
        on_host!(floors.boundary;
            outline_border,
            outline_hover_border,
            outline_pressed_border,
            outline_disabled_border
        );
    }
    if let Some(focus) = floors.focus {
        paint.focus_ring = resolve_host_endpoint_color(reference.focus_ring, hosts, focus);
        paint.sidebar_focus = resolve_host_endpoint_color(reference.sidebar_focus, hosts, focus);
    }
    on_host!(floors.boundary; info, success, warning, error);
    paint
}

fn material_content_polarity_fallbacks(
    reference: &ChromeColors,
    paint: &ChromeColors,
    host: Color,
) -> Vec<FloatingControlFamily> {
    let changed = |reference_fill: Color,
                   paint_fill: Color,
                   reference_content: Color,
                   paint_content: Color| {
        let reference_background = reference_fill.source_over(host);
        let paint_background = paint_fill.source_over(host);
        reference_content.contrast_ratio(reference_background) >= 1.05
            && content_is_lighter_than_background(reference_content, reference_background)
                != content_is_lighter_than_background(paint_content, paint_background)
    };
    let mut fallbacks = Vec::new();
    macro_rules! family {
        ($family:expr; $(($fill:ident, $($content:ident),+)),+ $(,)?) => {
            if false $($(|| changed(
                reference.$fill,
                paint.$fill,
                reference.$content,
                paint.$content,
            ))+)+ {
                fallbacks.push($family);
            }
        };
    }
    family!(FloatingControlFamily::Element;
        (element_background, element_foreground, element_icon),
        (element_hover, element_hover_foreground, element_hover_icon),
        (element_active, element_active_foreground, element_active_icon),
        (primary_background, primary_foreground, primary_icon),
        (primary_hover_background, primary_hover_foreground, primary_hover_icon),
        (primary_pressed_background, primary_pressed_foreground, primary_pressed_icon),
        (destructive_background, destructive_foreground, destructive_icon),
        (destructive_hover_background, destructive_hover_foreground, destructive_hover_icon),
        (destructive_pressed_background, destructive_pressed_foreground, destructive_pressed_icon),
        (selection_background, selection_foreground, selection_icon),
        (selection_hover_background, selection_hover_foreground, selection_hover_icon),
        (selection_pressed_background, selection_pressed_foreground, selection_pressed_icon),
    );
    family!(FloatingControlFamily::GhostElement;
        (ghost_element_hover, ghost_element_hover_foreground, ghost_element_hover_icon),
        (ghost_element_active, ghost_element_active_foreground, ghost_element_active_icon),
    );
    family!(FloatingControlFamily::Toggle;
        (toggle_off_background, toggle_off_mark),
        (toggle_off_hover_background, toggle_off_hover_mark),
        (toggle_off_pressed_background, toggle_off_pressed_mark),
        (toggle_on_background, toggle_on_mark),
        (toggle_on_hover_background, toggle_on_hover_mark),
        (toggle_on_pressed_background, toggle_on_pressed_mark),
    );
    family!(FloatingControlFamily::Input; (input_background, input_text, input_caret));
    fallbacks
}

fn resolve_material_segmented_colors(
    reference: &ChromeColors,
    mut paint: ChromeColors,
    host: Color,
    floors: FloatingContrastFloors,
    accessible_boundaries: bool,
    explicit_track: bool,
) -> ChromeColors {
    let hosts = [host; 2];
    let reference_track = if explicit_track {
        reference.segmented_track_background
    } else {
        reference.element_background
    };
    let paint_track = if explicit_track {
        paint.segmented_track_background
    } else {
        paint.element_background
    };
    let (track, [text_secondary, text_disabled]) = resolve_floating_frame(
        reference_track,
        paint_track,
        reference.background,
        hosts,
        [
            (reference.text_secondary, floors.secondary),
            (reference.text_disabled, floors.disabled),
        ],
    );
    paint.segmented_track_background = track;
    paint.element_background = track;
    paint.text_secondary = text_secondary;
    paint.text_disabled = text_disabled;
    let track_background = paint.element_background.source_over(host);
    let track_hosts = [track_background; 2];

    macro_rules! option_state {
        ($fill:ident, $minimum:expr, $content:ident, $border:ident) => {{
            let fallback_fill = if paint.$fill.a == 0 {
                Color::rgba(0)
            } else {
                paint.$fill.source_over(track_background)
            };
            if accessible_boundaries {
                let (fill, [content], border) = resolve_floating_frame_with_boundary(
                    fallback_fill,
                    paint.$fill,
                    track_background,
                    track_hosts,
                    [(reference.$content, $minimum)],
                    paint.$border,
                    floors.boundary,
                );
                paint.$fill = fill;
                paint.$content = content;
                paint.$border = border;
            } else {
                let (fill, [content]) = resolve_floating_frame(
                    fallback_fill,
                    paint.$fill,
                    track_background,
                    track_hosts,
                    [(reference.$content, $minimum)],
                );
                paint.$fill = fill;
                paint.$content = content;
            }
        }};
    }
    option_state!(
        ghost_element_hover,
        floors.primary,
        ghost_element_hover_foreground,
        ghost_element_hover_border
    );
    option_state!(
        ghost_element_active,
        floors.primary,
        ghost_element_active_foreground,
        ghost_element_active_border
    );
    option_state!(
        selection_background,
        floors.primary,
        selection_foreground,
        selection_border
    );
    option_state!(
        selection_hover_background,
        floors.primary,
        selection_hover_foreground,
        selection_hover_border
    );
    option_state!(
        selection_pressed_background,
        floors.primary,
        selection_pressed_foreground,
        selection_pressed_border
    );
    option_state!(
        selection_disabled_background,
        floors.disabled,
        selection_disabled_foreground,
        selection_disabled_border
    );
    if accessible_boundaries {
        paint.element_border =
            readable_on_backgrounds(paint.element_border, hosts, floors.boundary);
        paint.ghost_element_border =
            readable_on_backgrounds(paint.ghost_element_border, track_hosts, floors.boundary);
        paint.ghost_element_disabled_border = readable_on_backgrounds(
            paint.ghost_element_disabled_border,
            track_hosts,
            floors.boundary,
        );
    }
    if let Some(focus) = floors.focus {
        paint.focus_ring = resolve_host_endpoint_color(reference.focus_ring, hosts, focus);
    }
    paint
}

fn resolve_content_on_backgrounds<const N: usize, const B: usize>(
    proposed: [(Color, f64); N],
    backgrounds: [Color; B],
) -> Option<[Color; N]> {
    let needs_fallback = proposed
        .into_iter()
        .any(|(color, minimum)| !content_meets_backgrounds(color, backgrounds, minimum));
    let endpoint = needs_fallback.then(|| preferred_readable_endpoint(backgrounds));
    let resolved = proposed.map(|(color, minimum)| {
        if let Some(endpoint) = endpoint {
            readable_toward_endpoint(color, endpoint, backgrounds, minimum)
        } else {
            color
        }
    });
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

struct SegmentedColorCompilation {
    colors: ChromeColors,
    used_host_fallback: bool,
}

fn compile_segmented_control_colors(
    authored: &ChromeColors,
    reference: &ChromeColors,
    paint: &ChromeColors,
    materials: SurfaceMaterials,
    explicit_track: bool,
) -> SegmentedColorCompilation {
    let mut semantic = reference.clone();
    let mut candidate = semantic.clone();
    let mut feasible = true;
    let authored_track = if explicit_track {
        authored
            .segmented_track_background
            .source_over(authored.background)
    } else {
        authored.background
    };
    let reference_track = if explicit_track {
        reference.segmented_track_background
    } else {
        reference.element_background
    };
    macro_rules! rehost_option {
        ($($field:ident),+ $(,)?) => { $(
            if authored.$field.a == 0 {
                candidate.$field = authored.$field;
            } else if let Some(fill) = host_relative_fill(
                authored.$field,
                authored_track,
                reference_track,
            ) {
                candidate.$field = fill;
            } else {
                feasible = false;
            }
        )+ };
    }
    rehost_option!(
        ghost_element_background,
        ghost_element_hover,
        ghost_element_active,
        ghost_element_disabled,
        selection_background,
        selection_hover_background,
        selection_pressed_background,
        selection_disabled_background,
    );
    if feasible {
        semantic = candidate;
    }

    let mut segmented = paint.clone();
    segmented.segmented_track_background = if explicit_track {
        paint.segmented_track_background
    } else {
        paint.element_background
    };
    segmented.element_background = segmented.segmented_track_background;
    segmented.border = materials.edge(reference_track, reference.border);
    let option = |target: Color| {
        if target.a == 0 {
            target
        } else {
            materials.paint(SurfaceRole::Surface, reference_track, target)
        }
    };
    macro_rules! option_fill {
        ($($field:ident),+ $(,)?) => { $(segmented.$field = option(semantic.$field);)+ };
    }
    option_fill!(
        ghost_element_background,
        ghost_element_hover,
        ghost_element_active,
        ghost_element_disabled,
        selection_background,
        selection_hover_background,
        selection_pressed_background,
        selection_disabled_background,
    );
    let filled_option = |fill: Color| {
        if fill.a == 0 { reference_track } else { fill }
    };
    segmented.border_variant = materials.edge(
        filled_option(semantic.ghost_element_active),
        reference.border_variant,
    );
    macro_rules! option_edge {
        ($($fill:ident => $border:ident),+ $(,)?) => { $(
            segmented.$border = materials.edge(filled_option(semantic.$fill), reference.$border);
        )+ };
    }
    option_edge!(
        ghost_element_background => ghost_element_border,
        ghost_element_hover => ghost_element_hover_border,
        ghost_element_active => ghost_element_active_border,
        ghost_element_disabled => ghost_element_disabled_border,
        selection_background => selection_border,
        selection_hover_background => selection_hover_border,
        selection_pressed_background => selection_pressed_border,
        selection_disabled_background => selection_disabled_border,
    );
    SegmentedColorCompilation {
        colors: segmented,
        used_host_fallback: !feasible,
    }
}

fn prepare_control_host(
    authored: &ChromeColors,
    host: Color,
    materials: SurfaceMaterials,
) -> PreparedControlHost {
    let (reference, mut fallback_families) = rehost_control_reference(authored, host);
    let colors = reference.material_presentation(materials);
    let segmented =
        compile_segmented_control_colors(authored, &reference, &colors, materials, false);
    if segmented.used_host_fallback {
        fallback_families.push(FloatingControlFamily::Segmented);
    }
    let reference = with_final_host_content(reference, &colors);
    PreparedControlHost {
        reference,
        colors,
        segmented: segmented.colors,
        fallback_families,
    }
}

fn non_floating_control_host_background(
    materials: SurfaceMaterials,
    colors: &ChromeColors,
    host: spaceterm_ui::ControlHost,
    active: bool,
) -> Color {
    let sheet = materials
        .paint(SurfaceRole::Sheet, colors.background, colors.background)
        .source_over(colors.background);
    let window = materials
        .paint(SurfaceRole::Base, colors.background, colors.background)
        .source_over(sheet);
    match host {
        spaceterm_ui::ControlHost::Window => window,
        spaceterm_ui::ControlHost::TitleBar => materials
            .paint(
                SurfaceRole::Base,
                colors.background,
                if active {
                    colors.title_bar_background
                } else {
                    colors.title_bar_inactive_background
                },
            )
            .source_over(sheet),
        spaceterm_ui::ControlHost::Panel => materials
            .paint(
                SurfaceRole::Base,
                colors.background,
                colors.panel_background,
            )
            .source_over(sheet),
        spaceterm_ui::ControlHost::Card => materials
            .paint(
                SurfaceRole::Surface,
                colors.background,
                colors.elevated_surface_background,
            )
            .source_over(window),
        spaceterm_ui::ControlHost::Floating => colors.elevated_surface_background,
    }
}

/// Moves one prepared semantic target only when its modeled material composite cannot carry the
/// requested contrast floor. The resolved scheme remains untouched, and rendering consumes the
/// returned target through the ordinary material path.
fn feasible_non_floating_material_target(
    materials: SurfaceMaterials,
    role: SurfaceRole,
    root: Color,
    target: Color,
    minimum: f64,
) -> Color {
    let rendered = |candidate| materials.paint(role, root, candidate).source_over(root);
    let feasible = |candidate| {
        let host = rendered(candidate);
        Color::rgb(0x000000).contrast_ratio(host) >= minimum
            || Color::rgb(0xffffff).contrast_ratio(host) >= minimum
    };
    if feasible(target) {
        return target;
    }

    let host = rendered(target);
    let dark = Color::rgb(0x000000);
    let light = Color::rgb(0xffffff);
    let preferred = if dark.contrast_ratio(host) >= light.contrast_ratio(host) {
        light
    } else {
        dark
    };
    for endpoint in [preferred, if preferred == light { dark } else { light }] {
        if !feasible(endpoint) {
            continue;
        }
        let (mut lower, mut upper) = (0.0, 1.0);
        let mut result = endpoint;
        for _ in 0..16 {
            let amount = (lower + upper) / 2.0;
            let candidate = target.mix(endpoint, amount);
            if feasible(candidate) {
                result = candidate;
                upper = amount;
            } else {
                lower = amount;
            }
        }
        return result;
    }

    // The modeled root is made feasible before this helper is called. Collapsing this host onto
    // that root preserves the floor if a future material cannot reach either endpoint directly.
    root
}

fn prepare_feasible_non_floating_hosts(
    colors: &mut ChromeColors,
    materials: SurfaceMaterials,
    minimum: f64,
) {
    let root = colors.background;
    colors.title_bar_background = feasible_non_floating_material_target(
        materials,
        SurfaceRole::Base,
        root,
        colors.title_bar_background,
        minimum,
    );
    colors.title_bar_inactive_background = feasible_non_floating_material_target(
        materials,
        SurfaceRole::Base,
        root,
        colors.title_bar_inactive_background,
        minimum,
    );
    colors.panel_background = feasible_non_floating_material_target(
        materials,
        SurfaceRole::Base,
        root,
        colors.panel_background,
        minimum,
    );
    colors.elevated_surface_background = feasible_non_floating_material_target(
        materials,
        SurfaceRole::Surface,
        root,
        colors.elevated_surface_background,
        minimum,
    );
}

#[derive(Clone, Copy)]
struct PreparedAppState<const N: usize> {
    fill: Color,
    content: [Color; N],
}

/// Resolves one app-drawn row or Tab state on the material it will actually paint.
///
/// The returned fill remains a semantic material target; renderers still apply the material once.
/// Candidate search prefers retaining authored RGB and increasing prepared opacity, then makes the
/// smallest endpoint move that can carry every content floor and any persistent-selection step.
fn prepare_app_state<const N: usize>(
    materials: SurfaceMaterials,
    semantic_host: Color,
    final_host: Color,
    fill: Color,
    proposed: [(Color, f64); N],
    selection_floor: Option<f64>,
) -> PreparedAppState<N> {
    let resolve = |candidate: Color| {
        let background = materials
            .paint(SurfaceRole::Surface, semantic_host, candidate)
            .source_over(final_host);
        if selection_floor.is_some_and(|minimum| background.contrast_ratio(final_host) < minimum) {
            return None;
        }
        resolve_content_on_backgrounds(proposed, [background]).map(|content| PreparedAppState {
            fill: candidate,
            content,
        })
    };
    if let Some(resolved) = resolve(fill) {
        return resolved;
    }

    let channel_distance = |a: Color, b: Color| {
        u32::from(a.r.abs_diff(b.r)) + u32::from(a.g.abs_diff(b.g)) + u32::from(a.b.abs_diff(b.b))
    };
    let content_distance = |content: [Color; N]| {
        content
            .into_iter()
            .zip(proposed)
            .map(|(resolved, (original, _))| channel_distance(resolved, original))
            .sum::<u32>()
    };
    let score = |resolved: PreparedAppState<N>| {
        (
            channel_distance(resolved.fill, fill),
            resolved.fill.a.abs_diff(fill.a),
            content_distance(resolved.content),
        )
    };
    let mut best: Option<PreparedAppState<N>> = None;
    let mut consider = |candidate| {
        let Some(resolved) = resolve(candidate) else {
            return;
        };
        if best.is_none_or(|current| score(resolved) < score(current)) {
            best = Some(resolved);
        }
    };
    for alpha in fill.a..=u8::MAX {
        consider(fill.with_alpha(alpha));
    }
    for endpoint in [Color::rgb(0), Color::rgb(0xffffff)] {
        for step in 1..=u8::MAX {
            consider(fill.mix(endpoint, f64::from(step) / 255.0));
        }
    }
    best.unwrap_or_else(|| {
        let background = materials
            .paint(SurfaceRole::Surface, semantic_host, fill)
            .source_over(final_host);
        PreparedAppState {
            fill,
            content: proposed
                .map(|(color, minimum)| readable_on_backgrounds(color, [background], minimum)),
        }
    })
}

/// Resolves a selected-state rim against the surrounding material host. Explicitly absent optional
/// edges remain absent; the Increase Contrast policy has already made required selection rims
/// nontransparent before this seam.
fn prepare_app_state_boundary<const N: usize>(
    materials: SurfaceMaterials,
    semantic_host: Color,
    final_host: Color,
    fills: [Color; N],
    proposed: Color,
    minimum: f64,
) -> Color {
    if proposed.a == 0 {
        return proposed;
    }
    let readable = |candidate: Color| {
        fills.into_iter().all(|fill| {
            materials
                .edge(fill.source_over(semantic_host), candidate)
                .source_over(final_host)
                .contrast_ratio(final_host)
                >= minimum
        })
    };
    if readable(proposed) {
        return proposed;
    }
    for alpha in proposed.a..=u8::MAX {
        let candidate = proposed.with_alpha(alpha);
        if readable(candidate) {
            return candidate;
        }
    }
    for step in 1..=u8::MAX {
        let amount = f64::from(step) / 255.0;
        for endpoint in [Color::rgb(0), Color::rgb(0xffffff)] {
            let candidate = proposed.mix(endpoint, amount);
            if readable(candidate) {
                return candidate;
            }
        }
    }
    proposed
}

fn prepare_app_owned_rows(
    colors: &mut ChromeColors,
    materials: SurfaceMaterials,
    semantic_host: Color,
    final_host: Color,
    floors: FloatingContrastFloors,
) {
    macro_rules! row {
        ($fill:ident, [$primary:ident, $secondary:ident, $icon:ident, $matched:ident], $selection:expr) => {{
            let state = prepare_app_state(
                materials,
                semantic_host,
                final_host,
                colors.$fill,
                [
                    (colors.$primary, floors.primary),
                    (colors.$secondary, floors.secondary),
                    (colors.$icon, floors.mark),
                    (colors.$matched, floors.primary),
                ],
                $selection,
            );
            colors.$fill = state.fill;
            let [primary, secondary, icon, matched] = state.content;
            colors.$primary = primary;
            colors.$secondary = secondary;
            colors.$icon = icon;
            colors.$matched = matched;
        }};
    }
    row!(
        row_background,
        [row_foreground, row_secondary, row_icon, row_match],
        None
    );
    row!(
        row_hover_background,
        [
            row_hover_foreground,
            row_hover_secondary,
            row_hover_icon,
            row_hover_match
        ],
        None
    );
    row!(
        row_selected_background,
        [
            row_selected_foreground,
            row_selected_secondary,
            row_selected_icon,
            row_selected_match
        ],
        Some(1.4)
    );
    row!(
        row_selected_hover_background,
        [
            row_selected_hover_foreground,
            row_selected_hover_secondary,
            row_selected_hover_icon,
            row_selected_hover_match
        ],
        Some(1.4)
    );
    let selected = prepare_app_state(
        materials,
        semantic_host,
        final_host,
        colors.navigation_selected_background,
        [
            (colors.navigation_selected_foreground, floors.primary),
            (colors.navigation_selected_secondary, floors.secondary),
            (colors.navigation_selected_icon, floors.mark),
        ],
        Some(1.4),
    );
    colors.navigation_selected_background = selected.fill;
    let [foreground, secondary, icon] = selected.content;
    colors.navigation_selected_foreground = foreground;
    colors.navigation_selected_secondary = secondary;
    colors.navigation_selected_icon = icon;
    colors.row_selected_border = prepare_app_state_boundary(
        materials,
        semantic_host,
        final_host,
        [colors.row_selected_background],
        colors.row_selected_border,
        floors.boundary,
    );
    colors.row_selected_hover_border = prepare_app_state_boundary(
        materials,
        semantic_host,
        final_host,
        [colors.row_selected_hover_background],
        colors.row_selected_hover_border,
        floors.boundary,
    );
}

fn prepare_app_owned_tabs(
    colors: &mut ChromeColors,
    materials: SurfaceMaterials,
    semantic_host: Color,
    final_host: Color,
    floors: FloatingContrastFloors,
) {
    macro_rules! tab {
        ($fill:ident, [$foreground:ident, $icon:ident], $selection:expr) => {{
            let state = prepare_app_state(
                materials,
                semantic_host,
                final_host,
                colors.$fill,
                [
                    (colors.$foreground, floors.primary),
                    (colors.$icon, floors.mark),
                ],
                $selection,
            );
            colors.$fill = state.fill;
            let [foreground, icon] = state.content;
            colors.$foreground = foreground;
            colors.$icon = icon;
        }};
    }
    tab!(
        tab_inactive_background,
        [tab_inactive_foreground, tab_inactive_icon],
        None
    );
    tab!(
        tab_hover_background,
        [tab_hover_foreground, tab_hover_icon],
        None
    );
    tab!(
        tab_active_background,
        [tab_active_foreground, tab_active_icon],
        Some(1.4)
    );
    tab!(
        tab_active_hover_background,
        [tab_active_hover_foreground, tab_active_hover_icon],
        Some(1.4)
    );
    tab!(
        tab_inactive_selected_background,
        [tab_inactive_selected_foreground, tab_inactive_selected_icon],
        Some(1.4)
    );
    colors.tab_active_border = prepare_app_state_boundary(
        materials,
        semantic_host,
        final_host,
        [
            colors.tab_active_background,
            colors.tab_active_hover_background,
        ],
        colors.tab_active_border,
        floors.boundary,
    );
    colors.tab_inactive_selected_border = prepare_app_state_boundary(
        materials,
        semantic_host,
        final_host,
        [colors.tab_inactive_selected_background],
        colors.tab_inactive_selected_border,
        floors.boundary,
    );
}

#[expect(
    clippy::too_many_arguments,
    reason = "host preparation receives material, window state, and authored-role policies together"
)]
fn prepare_state_control_host(
    authored: &ChromeColors,
    (host, final_host): (Color, Color),
    host_role: spaceterm_ui::ControlHost,
    materials: SurfaceMaterials,
    state: ChromeStatePolicy,
    floors: FloatingContrastFloors,
    explicit_segmented_track: bool,
    built_in_light: bool,
) -> PreparedControlHost {
    let (mut reference, mut fallback_families) = rehost_control_reference(authored, host);
    if host_role == spaceterm_ui::ControlHost::Panel
        && reference.navigation_selected_background != reference.row_selected_background
    {
        // Navigation rests on the sidebar, not on the raised surface used by menu rows.
        reference.row_selected_background = reference.navigation_selected_background;
        reference.row_selected_hover_background = reference
            .navigation_selected_background
            .mix(reference.text, 0.06);
        reference.row_selected_foreground = reference.navigation_selected_foreground;
        reference.row_selected_secondary = reference.navigation_selected_secondary;
        reference.row_selected_icon = reference.navigation_selected_icon;
        reference.row_selected_match = reference.navigation_selected_foreground;
        reference.row_selected_hover_foreground = reference.navigation_selected_foreground;
        reference.row_selected_hover_secondary = reference.navigation_selected_secondary;
        reference.row_selected_hover_icon = reference.navigation_selected_icon;
        reference.row_selected_hover_match = reference.navigation_selected_foreground;
    }
    let mut reference = if built_in_light {
        built_in_light::prepare_state_colors(state, &reference, host)
    } else {
        state.colors(&reference, host)
    };
    if state.capabilities.increase_contrast || !built_in_light {
        let semantic_host = match host_role {
            spaceterm_ui::ControlHost::Panel => reference.panel_background,
            spaceterm_ui::ControlHost::Card => reference.elevated_surface_background,
            spaceterm_ui::ControlHost::Window
            | spaceterm_ui::ControlHost::TitleBar
            | spaceterm_ui::ControlHost::Floating => host,
        };
        prepare_app_owned_rows(
            &mut reference,
            materials,
            semantic_host,
            final_host,
            FloatingContrastFloors::for_increase_contrast(state.capabilities.increase_contrast),
        );
    }
    let colors = resolve_material_control_colors(
        &reference,
        if built_in_light {
            built_in_light::control_paints(&reference, materials)
        } else {
            reference.material_presentation(materials)
        },
        reference.background,
        final_host,
        floors,
        state.capabilities.increase_contrast || state.capabilities.show_borders,
    );
    for family in material_content_polarity_fallbacks(&reference, &colors, final_host) {
        if !fallback_families.contains(&family) {
            fallback_families.push(family);
        }
    }
    let segmented_authored = if built_in_light {
        built_in_light::prepare_state_colors(state, authored, authored.background)
    } else {
        state.colors(authored, authored.background)
    };
    let segmented = compile_segmented_control_colors(
        &segmented_authored,
        &reference,
        &colors,
        materials,
        explicit_segmented_track,
    );
    if segmented.used_host_fallback {
        fallback_families.push(FloatingControlFamily::Segmented);
    }
    let segmented = resolve_material_segmented_colors(
        &reference,
        segmented.colors,
        final_host,
        floors,
        state.capabilities.increase_contrast || state.capabilities.show_borders,
        explicit_segmented_track,
    );
    let reference = with_final_host_content(reference, &colors);
    PreparedControlHost {
        reference,
        colors,
        segmented,
        fallback_families,
    }
}

/// Applies the same authored luminance relationship to controls on non-floating hosts.
fn rehost_control_reference(
    authored: &ChromeColors,
    host: Color,
) -> (ChromeColors, Vec<FloatingControlFamily>) {
    let mut reference = authored.host_presentation(host);
    let mut fallback_families = Vec::new();
    macro_rules! family {
        ($family:ident; $($field:ident),+ $(,)?) => {{
            let mut candidate = reference.clone();
            let mut feasible = true;
            $(if !(reference.$field.a == 0 && authored.$field.a == 0) {
                if let Some(fill) = host_relative_fill(authored.$field, authored.background, host) {
                    candidate.$field = fill;
                } else {
                    feasible = false;
                }
            })+
            if feasible { reference = candidate; } else { fallback_families.push(FloatingControlFamily::$family); }
        }};
    }
    family!(Element; element_background, element_hover, element_active, element_selected, element_disabled);
    family!(GhostElement; ghost_element_background, ghost_element_hover, ghost_element_active, ghost_element_selected, ghost_element_disabled);
    family!(Toggle; toggle_off_background, toggle_off_hover_background, toggle_off_pressed_background, toggle_off_disabled_background,
        toggle_on_background, toggle_on_hover_background, toggle_on_pressed_background, toggle_on_disabled_background);
    family!(Input; input_background, input_disabled_background);
    (reference, fallback_families)
}

fn state_floating_material(
    appearance: Appearance,
    materials: SurfaceMaterials,
    root: Color,
    surface: Color,
    increase_contrast: bool,
) -> (Color, Color) {
    let (tone, wash) = resolved_floating_material(appearance, materials, root, surface);
    if increase_contrast {
        let tone = tone.with_alpha(tone.a.max(243));
        (
            floating_material_with_floor(appearance, tone, wash, 7.0).unwrap_or(surface),
            wash,
        )
    } else {
        (tone, wash)
    }
}

#[cfg(test)]
fn resolve_floating_segmented_colors(
    authored: &ChromeColors,
    floors: FloatingContrastFloors,
    reference: &ChromeColors,
    paint: ChromeColors,
    material: Color,
    wash: Color,
) -> ChromeColors {
    let resolved = resolve_floating_segmented_colors_detailed(
        authored, floors, reference, paint, material, wash, false,
    );
    let _pending_diagnostics = resolved.fallback_families;
    resolved.colors
}

fn resolve_floating_segmented_colors_detailed(
    authored: &ChromeColors,
    floors: FloatingContrastFloors,
    reference: &ChromeColors,
    mut paint: ChromeColors,
    material: Color,
    wash: Color,
    explicit_track: bool,
) -> FloatingColorResolution {
    let host_backgrounds = [Color::rgb(0x000000), Color::rgb(0xffffff)]
        .map(|underlay| wash.source_over(material.source_over(underlay)));
    let mut fallback_families = Vec::new();
    let semantic_track = if explicit_track {
        reference.segmented_track_background
    } else {
        host_relative_fill(
            authored.element_background,
            authored.background,
            reference.elevated_surface_background,
        )
        .unwrap_or_else(|| {
            fallback_families.push(FloatingControlFamily::Segmented);
            reference.element_background
        })
    };
    let track_overlay = if explicit_track {
        paint.segmented_track_background
    } else {
        equivalent_overlay(
            semantic_track,
            reference.elevated_surface_background,
            paint.element_background.a,
        )
        .unwrap_or(semantic_track)
    };
    let (track, [text_secondary, text_disabled]) = resolve_floating_frame(
        semantic_track,
        track_overlay,
        reference.elevated_surface_background,
        host_backgrounds,
        [
            (reference.text_secondary, floors.secondary),
            (reference.text_disabled, floors.disabled),
        ],
    );
    paint.segmented_track_background = track;
    paint.element_background = track;
    paint.text_secondary = text_secondary;
    paint.text_disabled = text_disabled;
    let track_backgrounds =
        host_backgrounds.map(|background| paint.element_background.source_over(background));
    let option_reference_surface = if explicit_track {
        semantic_track
    } else {
        reference.element_background
    };
    macro_rules! state {
        ($fill:ident, $minimum:expr, $($content:ident),+ $(,)?) => {{
            let (fill, [$($content),+]) = resolve_floating_state(
                reference.$fill,
                paint.$fill,
                option_reference_surface,
                track_backgrounds,
                [$(reference.$content),+],
                $minimum,
            );
            paint.$fill = fill;
            $(paint.$content = $content;)+
        }};
    }
    let segmented_state = |reference_fill, paint_fill, label, border, content_floor| {
        floating_state(
            reference_fill,
            paint_fill,
            semantic_track,
            [
                floating_constraint(label, content_floor),
                floating_host_constraint(border, floors.boundary),
                None,
                None,
            ],
        )
    };
    let segmented_states = [
        segmented_state(
            reference.ghost_element_background,
            paint.ghost_element_background,
            reference.text_secondary,
            paint.border_transparent,
            floors.secondary,
        ),
        segmented_state(
            reference.ghost_element_hover,
            paint.ghost_element_hover,
            reference.ghost_element_hover_foreground,
            paint.border_transparent,
            floors.primary,
        ),
        segmented_state(
            reference.ghost_element_active,
            paint.ghost_element_active,
            reference.ghost_element_active_foreground,
            paint.border_variant,
            floors.primary,
        ),
        segmented_state(
            reference.ghost_element_disabled,
            paint.ghost_element_disabled,
            reference.text_disabled,
            paint.border_transparent,
            floors.disabled,
        ),
        segmented_state(
            reference.selection_background,
            paint.selection_background,
            reference.selection_foreground,
            paint.selection_border,
            floors.primary,
        ),
        segmented_state(
            reference.selection_hover_background,
            paint.selection_hover_background,
            reference.selection_hover_foreground,
            paint.selection_hover_border,
            floors.primary,
        ),
        segmented_state(
            reference.selection_pressed_background,
            paint.selection_pressed_background,
            reference.selection_pressed_foreground,
            paint.selection_pressed_border,
            floors.primary,
        ),
        segmented_state(
            reference.selection_disabled_background,
            paint.selection_disabled_background,
            reference.selection_disabled_foreground,
            paint.selection_disabled_border,
            floors.disabled,
        ),
    ];
    let segmented_states = if explicit_track {
        // These fills have already been materialized against their authored track. Rehosting
        // their opaque targets would discard transparency and force bright selections opaque.
        Ok(segmented_states)
    } else {
        rehost_floating_states(
            segmented_states,
            [
                authored.ghost_element_background,
                authored.ghost_element_hover,
                authored.ghost_element_active,
                authored.ghost_element_disabled,
                authored.selection_background,
                authored.selection_hover_background,
                authored.selection_pressed_background,
                authored.selection_disabled_background,
            ],
            authored.background,
            semantic_track,
        )
    };
    let segmented_resolution = if explicit_track {
        // The unselected option includes an unpainted resting state. Its hover may require a
        // stronger backing without forcing the independently painted selected option opaque.
        segmented_states.and_then(
            |[
                normal,
                hover,
                pressed,
                disabled,
                selected,
                selected_hover,
                selected_pressed,
                selected_disabled,
            ]| {
                let orders: &[[usize; 3]] = if floors.interactive {
                    &[[0, 1, 2]]
                } else {
                    &[]
                };
                let (mut states, unselected_fallback) = resolve_floating_family_for_presentation(
                    Ok([normal, hover, pressed, disabled]),
                    track_backgrounds,
                    orders,
                )?;
                let (selected, selected_fallback) = resolve_floating_family_for_presentation(
                    Ok([
                        selected,
                        selected_hover,
                        selected_pressed,
                        selected_disabled,
                    ]),
                    track_backgrounds,
                    orders,
                )?;
                states.extend(selected);
                Ok((states, unselected_fallback || selected_fallback))
            },
        )
    } else {
        resolve_floating_family_for_presentation(
            segmented_states,
            track_backgrounds,
            if floors.interactive {
                &[[0, 1, 2], [4, 5, 6]]
            } else {
                &[]
            },
        )
    };
    if let Ok((states, used_opaque_fallback)) = segmented_resolution {
        if used_opaque_fallback {
            fallback_families.push(FloatingControlFamily::Segmented);
        }
        paint.ghost_element_background = states[0].fill;
        paint.text_secondary = states[0].content[0];
        paint.ghost_element_hover = states[1].fill;
        paint.ghost_element_hover_foreground = states[1].content[0];
        paint.ghost_element_active = states[2].fill;
        paint.ghost_element_active_foreground = states[2].content[0];
        paint.border_variant = states[2].content[1];
        paint.ghost_element_disabled = states[3].fill;
        paint.text_disabled = states[3].content[0];
        paint.selection_background = states[4].fill;
        paint.selection_foreground = states[4].content[0];
        paint.selection_border = states[4].content[1];
        paint.selection_hover_background = states[5].fill;
        paint.selection_hover_foreground = states[5].content[0];
        paint.selection_hover_border = states[5].content[1];
        paint.selection_pressed_background = states[6].fill;
        paint.selection_pressed_foreground = states[6].content[0];
        paint.selection_pressed_border = states[6].content[1];
        paint.selection_disabled_background = states[7].fill;
        paint.selection_disabled_foreground = states[7].content[0];
        paint.selection_disabled_border = states[7].content[1];
    } else {
        fallback_families.push(FloatingControlFamily::Segmented);
        macro_rules! segmented_state_with_boundary {
            ($fill:ident, $minimum:expr, $foreground:ident, $border:ident) => {{
                let (fill, [foreground], border) = resolve_floating_frame_with_boundary(
                    reference.$fill,
                    paint.$fill,
                    option_reference_surface,
                    track_backgrounds,
                    [(reference.$foreground, $minimum)],
                    paint.$border,
                    floors.boundary,
                );
                paint.$fill = fill;
                paint.$foreground = foreground;
                paint.$border = border;
            }};
        }
        state!(
            ghost_element_hover,
            floors.primary,
            ghost_element_hover_foreground
        );
        segmented_state_with_boundary!(
            ghost_element_active,
            floors.primary,
            ghost_element_active_foreground,
            border_variant
        );
        segmented_state_with_boundary!(
            selection_background,
            floors.primary,
            selection_foreground,
            selection_border
        );
        segmented_state_with_boundary!(
            selection_hover_background,
            floors.primary,
            selection_hover_foreground,
            selection_hover_border
        );
        segmented_state_with_boundary!(
            selection_pressed_background,
            floors.primary,
            selection_pressed_foreground,
            selection_pressed_border
        );
        segmented_state_with_boundary!(
            selection_disabled_background,
            floors.disabled,
            selection_disabled_foreground,
            selection_disabled_border
        );
    }
    FloatingColorResolution {
        colors: paint,
        fallback_families,
    }
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

/// Keeps a prominent surface at its authored host-relative contrast using material opacity only.
pub(super) fn prominent_surface_with(
    materials: SurfaceMaterials,
    host: Color,
    color: Color,
) -> Color {
    let paint = materials.paint(SurfaceRole::Surface, host, color);
    if materials.is_opaque() || paint.a == 0 {
        return paint;
    }
    let minimum = color.source_over(host).contrast_ratio(host).min(1.22);
    let visible = |alpha| {
        paint
            .with_alpha(alpha)
            .source_over(host)
            .contrast_ratio(host)
            >= minimum
    };
    if visible(paint.a) {
        return paint;
    }
    let mut lower = paint.a;
    let mut upper = 254;
    while lower < upper {
        let middle = lower + (upper - lower) / 2;
        if visible(middle) {
            upper = middle;
        } else {
            lower = middle + 1;
        }
    }
    paint.with_alpha(upper)
}

impl ChromeAppearance {
    /// Active-window selection paints used while a collection lacks keyboard focus.
    pub(crate) fn unfocused_selection_colors(
        &self,
        host: spaceterm_ui::ControlHost,
    ) -> &ChromeColors {
        self.unfocused_selection.colors(host)
    }

    /// Content paints resolved for the semantic surface that actually owns the content.
    pub(crate) fn host_colors(&self, host: spaceterm_ui::ControlHost) -> &ChromeColors {
        match host {
            spaceterm_ui::ControlHost::Window => &self.colors,
            spaceterm_ui::ControlHost::Panel => &self.panel_controls.reference,
            spaceterm_ui::ControlHost::TitleBar => &self.title_bar_controls.reference,
            spaceterm_ui::ControlHost::Card => &self.card_controls.reference,
            spaceterm_ui::ControlHost::Floating => &self.floating_colors,
        }
    }

    /// Persistent selection uses the shared host-relative prominent-surface material.
    pub(crate) fn selection_surface(&self, host: Color, color: Color) -> Color {
        self.prominent_surface(host, color)
    }

    /// Strengthens a selected chip's hover rim when its fill remains unchanged.
    pub(crate) fn selection_hover_rim(&self, host: Color, rim: Color) -> Color {
        const GAIN: f64 = 1.18;
        const CEILING: f64 = 1.8;

        let weight = |edge: Color| edge.source_over(host).contrast_ratio(host);
        let target = (weight(rim) * GAIN).min(CEILING);
        if rim.a == 0 || weight(rim) >= target {
            return rim;
        }
        // Prefer increased opacity; change ink only when opacity cannot reach the target.
        let least = |reaches: &dyn Fn(u16) -> bool| {
            let mut lower = 0_u16;
            let mut upper = 255_u16;
            while lower + 1 < upper {
                let middle = (lower + upper) / 2;
                if reaches(middle) {
                    upper = middle;
                } else {
                    lower = middle;
                }
            }
            upper
        };
        if weight(rim.with_alpha(u8::MAX)) >= target {
            let alpha = least(&|alpha| weight(rim.with_alpha(alpha as u8)) >= target);
            return rim.with_alpha(alpha as u8);
        }
        let resting = rim.source_over(host);
        let endpoint = Color::rgb(if content_is_lighter_than_background(resting, host) {
            0xffffff
        } else {
            0
        });
        let toward = |amount: u16| resting.mix(endpoint, f64::from(amount) / 255.0);
        let amount = least(&|amount| toward(amount).contrast_ratio(host) >= target);
        self.materials.edge(host, toward(amount))
    }

    /// Selected navigation and Light Pane interiors share one material-strength policy.
    fn prominent_surface(&self, host: Color, color: Color) -> Color {
        prominent_surface_with(self.materials, host, color)
    }

    /// Applies the window's material for one surface role to an authored background color.
    ///
    /// Only the owner of a painted background calls this, once. Authored translucency is scaled
    /// rather than replaced, so a theme that authored a translucent surface keeps its intent.
    pub(crate) fn surface(&self, role: SurfaceRole, color: Color) -> Color {
        self.materials.paint(role, self.colors.background, color)
    }

    /// The deterministic in-window backdrop beneath controls on a host.
    ///
    /// The native desktop is intentionally outside this contract. The scheme's opaque reference
    /// backs the same Sheet, Base and Surface paints that the corresponding window roots render.
    /// Floating callers that need transmission bounds use the shell's two endpoint backgrounds;
    /// its value here is the opaque semantic reference only.
    pub(crate) fn control_host_background(&self, host: spaceterm_ui::ControlHost) -> Color {
        if let Some(settings) = self.settings_hosts
            && let Some(background) = settings.background(host)
        {
            return background;
        }
        match host {
            spaceterm_ui::ControlHost::Floating => self.floating_colors.elevated_surface_background,
            host => non_floating_control_host_background(
                self.materials,
                &self.colors,
                host,
                self.active,
            ),
        }
    }

    /// A functional rule on a nonfloating host, independent of its decorative outer border.
    pub(crate) fn separator(&self, host: spaceterm_ui::ControlHost) -> Color {
        separator::prepare_in_band(
            self.rule_seed(),
            self.colors.background,
            self.host_colors(host).text,
            [self.control_host_background(host)],
            self.capabilities.increase_contrast,
            self.rule_band(),
        )
    }

    /// Prepares a card, Pane, or popup edge independently of its internal dividers.
    pub(crate) fn surface_edge(&self, host: spaceterm_ui::ControlHost) -> Color {
        let background = self.control_host_background(host);
        self.surface_edge_on([background], self.host_colors(host).text)
    }

    fn surface_edge_on<const N: usize>(&self, backgrounds: [Color; N], text: Color) -> Color {
        separator::prepare_in_band(
            self.colors.border,
            self.colors.background,
            text,
            backgrounds,
            self.capabilities.increase_contrast || self.capabilities.show_borders,
            self.surface_band(),
        )
    }

    /// Functional rules take the quiet boundary where one definition ranks its own edges.
    fn rule_seed(&self) -> Color {
        if self.built_in_light || self.built_in_dark {
            self.colors.border_variant
        } else {
            self.colors.border
        }
    }

    fn rule_band(&self) -> separator::SeparatorBand {
        if self.built_in_light {
            built_in_light::RULE_BAND
        } else if self.built_in_dark {
            built_in_dark::RULE_BAND
        } else {
            separator::SeparatorBand::FUNCTIONAL
        }
    }

    fn surface_band(&self) -> separator::SeparatorBand {
        if self.built_in_light {
            built_in_light::SURFACE_BAND
        } else if self.built_in_dark {
            built_in_dark::SURFACE_BAND
        } else {
            separator::SeparatorBand::FUNCTIONAL
        }
    }

    /// Bounded, content-free evidence when visibility requires relaxing the quietness ceiling.
    #[cfg(feature = "appearance-exerciser")]
    pub(crate) fn separator_ceiling_fallbacks(&self) -> Vec<&'static str> {
        use spaceterm_ui::{ControlHost, FloatingRole};

        if self.capabilities.increase_contrast {
            return Vec::new();
        }
        let ceiling = self.rule_band().ceiling;
        let mut hosts = Vec::new();
        for (label, host) in [
            ("Window", ControlHost::Window),
            ("Panel", ControlHost::Panel),
            ("Card", ControlHost::Card),
        ] {
            let background = self.control_host_background(host);
            if self
                .separator(host)
                .source_over(background)
                .contrast_ratio(background)
                > ceiling
            {
                hosts.push(label);
            }
        }
        let surfaces = self.floating_surfaces();
        for (label, role) in [
            ("Popover", FloatingRole::Popover),
            ("Command", FloatingRole::Command),
            ("Modal", FloatingRole::Modal),
            ("Tooltip", FloatingRole::Tooltip),
            ("Notice", FloatingRole::Notice),
            ("Readout", FloatingRole::Readout),
        ] {
            let shell = surfaces.shell(role);
            let tone = Color::rgba(u32::from(shell.backdrop_tone()));
            let wash = Color::rgba(u32::from(shell.material()));
            let divider = Color::rgba(u32::from(shell.divider()));
            if [Color::rgb(0), Color::rgb(0xffffff)]
                .into_iter()
                .any(|underlay| {
                    let background = wash.source_over(tone.source_over(underlay));
                    divider.source_over(background).contrast_ratio(background) > ceiling
                })
            {
                hosts.push(label);
            }
        }
        hosts
    }

    /// Returns the combined tone and wash used to evaluate content over a floating surface.
    pub(crate) fn floating_surface(&self, color: Color) -> Color {
        let (tone, wash) = state_floating_material(
            self.appearance,
            self.floating_materials,
            self.colors.background,
            color,
            self.capabilities.increase_contrast,
        );
        wash.source_over(tone)
    }

    /// The backdrop a Pane paints beneath its Terminal.
    ///
    /// Light uses the selected navigation material with the accepted Terminal background. Dark
    /// retains its elevation tint and readability backing. Explicit cell backgrounds are separate.
    pub(crate) fn pane_surface(&self, terminal_background: Color) -> Color {
        if self.appearance == Appearance::Light {
            return self.prominent_surface(self.colors.background, terminal_background);
        }
        self.materials.pane_surface(
            self.colors.background,
            self.colors.elevated_surface_background,
            terminal_background,
        )
    }

    /// Prepares the Pane boundary independently of selected-row rims and short Tab separators.
    pub(crate) fn pane_rim(&self) -> Color {
        if self.built_in_light || self.built_in_dark {
            return self.surface_edge(spaceterm_ui::ControlHost::Window);
        }
        self.materials
            .edge(self.colors.panel_background, self.colors.tab_separator)
    }

    /// Resolves the Pane rim against the Terminal surface it is actually painted over.
    pub(crate) fn pane_rim_on(&self, terminal_background: Color) -> Color {
        if !self.built_in_light && !self.built_in_dark {
            return self.pane_rim();
        }
        let pane = self
            .pane_surface(terminal_background)
            .source_over(self.control_host_background(spaceterm_ui::ControlHost::Window));
        self.surface_edge_on([pane], self.colors.text)
    }

    /// Resolves shared floating paints; the control catalog applies density once on installation.
    pub(crate) fn floating_surfaces(&self) -> spaceterm_ui::FloatingSurfaceTheme {
        use spaceterm_ui::{FloatingSurfacePaint, FloatingSurfacePaints, FloatingSurfaceTheme};

        let paint = |color: Color, text_dense: bool| {
            let (mut tone, wash) = state_floating_material(
                self.appearance,
                self.floating_materials,
                self.colors.background,
                color,
                self.capabilities.increase_contrast,
            );
            if text_dense && tone.a > 0 {
                tone = tone.with_alpha(tone.a.max(230));
            }
            let endpoints = [
                wash.source_over(tone.source_over(Color::rgb(0))),
                wash.source_over(tone.source_over(Color::rgb(0xffffff))),
            ];
            let edge = if self.capabilities.increase_contrast || self.capabilities.show_borders {
                readable_on_material(self.colors.border, tone, wash, 3.0)
            } else if self.built_in_light || self.built_in_dark {
                self.surface_edge_on(endpoints, self.floating_colors.text)
            } else {
                self.colors.shadow.with_alpha(115)
            };
            let divider = separator::prepare_in_band(
                self.rule_seed(),
                self.colors.background,
                self.floating_colors.text,
                endpoints,
                self.capabilities.increase_contrast,
                self.rule_band(),
            );
            FloatingSurfacePaint::new(
                rgba(wash.rgba_hex()),
                rgba(edge.rgba_hex()),
                rgba(divider.rgba_hex()),
            )
            .backdrop_tone(rgba(tone.rgba_hex()))
        };
        FloatingSurfaceTheme::new(
            FloatingSurfacePaints::new(
                paint(self.floating_colors.elevated_surface_background, false),
                paint(self.colors.preview_background, true),
            )
            .tooltip(paint(self.colors.elevated_surface_background, true)),
            rgba(
                self.colors
                    .shadow
                    .with_alpha(
                        (f32::from(self.colors.shadow.a) * if self.active { 1.0 } else { 0.6 })
                            .round() as u8,
                    )
                    .rgba_hex(),
            )
            .into(),
            rgba(self.colors.modal_scrim.rgba_hex()),
        )
        .corner_radii(
            super::chrome_geometry::RadiusRole::Card.pixels(),
            super::chrome_geometry::RadiusRole::Surface.pixels(),
            super::chrome_geometry::RadiusRole::SurfaceLarge.pixels(),
        )
        .backdrop_alpha_limit(self.materials.floating_backdrop_alpha_limit())
        .backdrop_blur(if self.floating_blur {
            px(20.0)
        } else {
            px(0.0)
        })
    }

    #[cfg(test)]
    pub(crate) fn prepare(resolved: &ResolvedChromeAppearance) -> Self {
        Self::prepare_variants(resolved).0
    }

    /// Compiles both window variants and retains one canonical disabled presentation.
    pub(crate) fn prepare_variants(resolved: &ResolvedChromeAppearance) -> (Self, Self) {
        let mut active = Self::prepare_variant(resolved, true);
        let mut inactive = Self::prepare_variant(resolved, false);
        built_in_light::prepare_active_segmented_controls(&mut active, resolved);
        disabled_union::reconcile(&mut active, &mut inactive, &resolved.colors);
        built_in_light::finalize_inactive_segmented_controls(&mut inactive);
        active.unfocused_selection = collection_selection::prepare(&active, &inactive);
        (active, inactive)
    }

    #[cfg(test)]
    pub(crate) fn prepare_for_activity(resolved: &ResolvedChromeAppearance, active: bool) -> Self {
        if active {
            Self::prepare(resolved)
        } else {
            Self::prepare_variants(resolved).1
        }
    }

    fn prepare_variant(resolved: &ResolvedChromeAppearance, active: bool) -> Self {
        let capabilities = resolved.composition.capabilities;
        let built_in_light = built_in_light::applies(resolved);
        let built_in_dark = resolved.effective_scheme == crate::appearance::builtin_dark_chrome();
        let explicit_segmented_track = matches!(
            resolved.provenance.get("segmented_track_background"),
            Some(ColorProvenance::Authored | ColorProvenance::Overridden)
        );
        let state = ChromeStatePolicy {
            active,
            capabilities,
        };
        let authored = state.surfaces(&resolved.colors);
        let source = if built_in_light {
            built_in_light::prepare_state_colors(state, &authored, authored.background)
        } else {
            state.colors(&authored, authored.background)
        };
        let typography = ChromeTypography::prepare(&resolved.typography, resolved.density);
        let icons = ChromeIcons::prepare(&typography, resolved.density);
        let floating_contrast_floors = FloatingContrastFloors {
            secondary: if active || capabilities.increase_contrast {
                FloatingContrastFloors::for_increase_contrast(capabilities.increase_contrast)
                    .secondary
            } else {
                3.0
            },
            interactive: active,
            ..FloatingContrastFloors::for_increase_contrast(capabilities.increase_contrast)
        };
        let mut opaque = authored.opaque_presentation();
        if capabilities.increase_contrast {
            prepare_feasible_non_floating_hosts(
                &mut opaque,
                resolved.composition.materials,
                FloatingContrastFloors::INCREASED.primary,
            );
        }
        let mut colors = if built_in_light {
            built_in_light::prepare_state_colors(state, &opaque, opaque.background)
        } else {
            state.colors(&opaque, opaque.background)
        };
        let sheet = resolved
            .composition
            .materials
            .paint(SurfaceRole::Sheet, colors.background, colors.background)
            .source_over(colors.background);
        let title_bar = if active {
            colors.title_bar_background
        } else {
            colors.title_bar_inactive_background
        };
        let final_title_bar = resolved
            .composition
            .materials
            .paint(SurfaceRole::Base, colors.background, title_bar)
            .source_over(sheet);
        if capabilities.increase_contrast || !built_in_light {
            let app_owned_floors =
                FloatingContrastFloors::for_increase_contrast(capabilities.increase_contrast);
            prepare_app_owned_rows(
                &mut colors,
                resolved.composition.materials,
                title_bar,
                final_title_bar,
                app_owned_floors,
            );
            prepare_app_owned_tabs(
                &mut colors,
                resolved.composition.materials,
                title_bar,
                final_title_bar,
                app_owned_floors,
            );
        }
        let floating_reference = if built_in_light {
            built_in_light::floating_reference(&authored, resolved)
        } else {
            authored.floating_presentation()
        };
        let (floating_raised_material, floating_raised_wash) = state_floating_material(
            resolved.appearance,
            resolved.composition.floating_materials,
            colors.background,
            floating_reference.elevated_surface_background,
            capabilities.increase_contrast,
        );
        let (floating_readout_material, floating_readout_wash) = state_floating_material(
            resolved.appearance,
            resolved.composition.floating_materials,
            colors.background,
            colors.preview_background,
            capabilities.increase_contrast,
        );
        let active_rows = ChromeStatePolicy {
            active: true,
            capabilities,
        }
        .colors(
            &floating_reference,
            floating_reference.elevated_surface_background,
        );
        let floating_disabled_selected_background = active_rows
            .row_selected_background
            .mix(floating_reference.elevated_surface_background, 0.60);
        let floating_colors = floating_host_colors(
            state.colors(
                &floating_reference,
                floating_reference.elevated_surface_background,
            ),
            floating_raised_material,
            floating_raised_wash,
            floating_readout_material,
            floating_readout_wash,
            floating_contrast_floors,
        );
        let window_host = non_floating_control_host_background(
            resolved.composition.materials,
            &colors,
            spaceterm_ui::ControlHost::Window,
            active,
        );
        let control_colors = resolve_material_control_colors(
            &colors,
            if built_in_light {
                built_in_light::control_paints(&colors, resolved.composition.materials)
            } else {
                colors.material_presentation(resolved.composition.materials)
            },
            colors.background,
            window_host,
            floating_contrast_floors,
            capabilities.increase_contrast || capabilities.show_borders,
        );
        let segmented_compilation = compile_segmented_control_colors(
            &source,
            &colors,
            &control_colors,
            resolved.composition.materials,
            explicit_segmented_track,
        );
        let mut window_control_fallbacks = segmented_compilation
            .used_host_fallback
            .then_some(FloatingControlFamily::Segmented)
            .into_iter()
            .collect::<Vec<_>>();
        for family in material_content_polarity_fallbacks(&colors, &control_colors, window_host) {
            if !window_control_fallbacks.contains(&family) {
                window_control_fallbacks.push(family);
            }
        }
        let segmented_control_colors = resolve_material_segmented_colors(
            &colors,
            segmented_compilation.colors,
            window_host,
            floating_contrast_floors,
            capabilities.increase_contrast || capabilities.show_borders,
            explicit_segmented_track,
        );
        let panel_controls = prepare_state_control_host(
            &authored,
            (
                colors.panel_background,
                non_floating_control_host_background(
                    resolved.composition.materials,
                    &colors,
                    spaceterm_ui::ControlHost::Panel,
                    active,
                ),
            ),
            spaceterm_ui::ControlHost::Panel,
            resolved.composition.materials,
            state,
            floating_contrast_floors,
            explicit_segmented_track,
            built_in_light,
        );
        let card_host = non_floating_control_host_background(
            resolved.composition.materials,
            &colors,
            spaceterm_ui::ControlHost::Card,
            active,
        );
        let card_controls = prepare_state_control_host(
            &authored,
            (colors.elevated_surface_background, card_host),
            spaceterm_ui::ControlHost::Card,
            resolved.composition.materials,
            state,
            floating_contrast_floors,
            explicit_segmented_track,
            built_in_light,
        );
        let title_bar_controls = prepare_state_control_host(
            &authored,
            (
                if active {
                    colors.title_bar_background
                } else {
                    colors.title_bar_inactive_background
                },
                non_floating_control_host_background(
                    resolved.composition.materials,
                    &colors,
                    spaceterm_ui::ControlHost::TitleBar,
                    active,
                ),
            ),
            spaceterm_ui::ControlHost::TitleBar,
            resolved.composition.materials,
            state,
            floating_contrast_floors,
            explicit_segmented_track,
            built_in_light,
        );
        let FloatingColorResolution {
            colors: floating_control_colors,
            mut fallback_families,
        } = resolve_floating_control_colors_detailed(
            &source,
            floating_contrast_floors,
            &floating_colors,
            if built_in_light {
                built_in_light::control_paints(
                    &floating_colors,
                    resolved.composition.floating_materials,
                )
            } else {
                floating_colors.material_presentation(resolved.composition.floating_materials)
            },
            floating_raised_material,
            floating_raised_wash,
        );
        let FloatingColorResolution {
            colors: floating_segmented_colors,
            fallback_families: segmented_fallbacks,
        } = resolve_floating_segmented_colors_detailed(
            &source,
            floating_contrast_floors,
            &floating_colors,
            compile_segmented_control_colors(
                &source,
                &floating_colors,
                &floating_control_colors,
                resolved.composition.floating_materials,
                explicit_segmented_track,
            )
            .colors,
            floating_raised_material,
            floating_raised_wash,
            explicit_segmented_track,
        );
        let FloatingFieldResolution {
            reference: floating_field_reference,
            colors: floating_field_colors,
            fallback_families: field_fallbacks,
        } = resolve_floating_field_colors_detailed(
            &source,
            floating_contrast_floors,
            floating_colors.clone(),
            floating_control_colors.clone(),
            floating_raised_material,
            floating_raised_wash,
        );
        let colors = with_final_host_content(colors, &control_colors);
        let semantic_text_pairs = super::chrome_semantic_pairs::prepare_semantic_text_pairs(
            &colors,
            &card_controls.reference,
            resolved.composition.materials,
            window_host,
            card_host,
            capabilities.increase_contrast,
        );
        let unfocused_selection = collection_selection::PreparedCollectionSelection::identity(
            &colors,
            &title_bar_controls.reference,
            &panel_controls.reference,
            &card_controls.reference,
            &floating_colors,
        );
        Self {
            appearance: resolved.appearance,
            active,
            capabilities,
            typography,
            icons,
            semantic_text_pairs,
            unfocused_selection,
            control_colors,
            segmented_control_colors,
            window_control_fallbacks,
            panel_controls,
            title_bar_controls,
            card_controls,
            floating_control_colors,
            floating_segmented_colors,
            floating_disabled_selected_background,
            floating_colors,
            floating_field_reference,
            floating_field_colors,
            colors,
            floating_fallbacks: {
                for family in segmented_fallbacks.into_iter().chain(field_fallbacks) {
                    if !fallback_families.contains(&family) {
                        fallback_families.push(family);
                    }
                }
                fallback_families
            },
            disabled_diagnostics: Vec::new(),
            materials: resolved.composition.materials,
            floating_materials: resolved.composition.floating_materials,
            floating_blur: resolved.composition.floating_blur,
            text_scale: resolved.typography.body.size / 13.0,
            spacing_scale: Self::density_spacing_scale(resolved.density),
            settings_hosts: None,
            built_in_light,
            built_in_dark,
        }
    }

    /// The factor every density-scaled Chrome length is multiplied by.
    pub(crate) fn density_spacing_scale(density: ChromeDensity) -> f32 {
        match density {
            ChromeDensity::Compact => 1.0,
            ChromeDensity::Comfortable => 1.25,
        }
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
}

#[cfg(test)]
mod typography_tests {
    use super::{
        ChromeAppearance, FloatingContrastFloors, floating_constraint, floating_host_constraint,
        floating_state, host_relative_fill, relative_luminance, resolve_floating_control_colors,
        resolve_floating_field_colors, resolve_floating_frame, resolve_floating_segmented_colors,
        resolve_floating_state_at_alpha, resolve_material_control_colors,
    };

    #[test]
    fn material_frame_strengthens_the_fill_before_changing_authored_content_polarity() {
        use crate::appearance::Color;

        let host = Color::rgb(0xffffff);
        let opaque_seed = Color::rgb(0x0055aa);
        let translucent_fill = Color::rgba(0x0055aa80);
        let authored_content = Color::rgb(0xffffff);
        let (fill, [content]) = resolve_floating_frame(
            opaque_seed,
            translucent_fill,
            host,
            [host; 2],
            [(authored_content, 4.5)],
        );
        let background = fill.source_over(host);

        assert_eq!(
            content, authored_content,
            "a representable opaque seed must preserve authored content polarity"
        );
        assert_ne!(
            fill, translucent_fill,
            "the prepared fill must strengthen before content changes"
        );
        assert!(content.contrast_ratio(background) >= 4.5);
    }

    #[test]
    fn nonfloating_filled_polarity_flip_is_coherent_and_diagnosed() {
        use crate::appearance::{ChromeColors, Color};

        let host = Color::rgb(0xffffff);
        let reference = ChromeColors {
            background: host,
            primary_background: Color::rgb(0xaaaaaa),
            primary_foreground: Color::rgb(0xffffff),
            primary_icon: Color::rgb(0x111111),
            ..ChromeColors::default()
        };
        let resolved = resolve_material_control_colors(
            &reference,
            reference.clone(),
            host,
            host,
            FloatingContrastFloors::STANDARD,
            false,
        );
        let background = resolved.primary_background.source_over(host);

        for content in [resolved.primary_foreground, resolved.primary_icon] {
            assert!(relative_luminance(content) < relative_luminance(background));
            assert!(content.contrast_ratio(background) >= 4.5);
        }
        assert!(
            super::material_content_polarity_fallbacks(&reference, &resolved, host)
                .contains(&super::FloatingControlFamily::Element)
        );
    }

    #[test]
    fn material_control_resolution_escapes_an_infeasible_increased_contrast_fill() {
        use crate::appearance::{ChromeColors, Color};
        let host = Color::rgb(0xffffff);
        let reference = ChromeColors {
            background: host,
            primary_background: Color::rgb(0x767676),
            primary_foreground: Color::rgb(0xffffff),
            primary_icon: Color::rgb(0xffffff),
            ..ChromeColors::default()
        };
        let paint = ChromeColors {
            primary_background: Color::rgba(0x00000089),
            ..reference.clone()
        };

        let resolved = resolve_material_control_colors(
            &reference,
            paint,
            host,
            host,
            FloatingContrastFloors::INCREASED,
            true,
        );
        let background = resolved.primary_background.source_over(host);

        assert!(
            resolved
                .primary_foreground
                .source_over(background)
                .contrast_ratio(background)
                >= 7.0
        );
        assert!(
            resolved
                .primary_icon
                .source_over(background)
                .contrast_ratio(background)
                >= 7.0
        );
        assert_ne!(
            background,
            Color::rgb(0x767676),
            "an opaque midtone reference cannot carry 7:1 content"
        );
    }

    #[test]
    fn inactive_material_resolution_keeps_focus_absent_and_collapses_the_field_frame() {
        use crate::appearance::{ChromeColors, Color};
        let host = Color::rgb(0xffffff);
        let reference = ChromeColors {
            background: host,
            focus_ring: Color::rgba(0),
            input_border: Color::rgba(0x10101080),
            input_focused_border: Color::rgba(0),
            input_invalid_border: Color::rgb(0xaa0000),
            ..ChromeColors::default()
        };
        let mut floors = FloatingContrastFloors::STANDARD;
        floors.interactive = false;
        let resolved = resolve_material_control_colors(
            &reference,
            reference.clone(),
            host,
            host,
            floors,
            false,
        );

        assert_eq!(resolved.focus_ring.a, 0);
        assert_eq!(resolved.input_focused_border, resolved.input_border);
        assert_ne!(
            resolved.input_invalid_border.a, 0,
            "invalid state remains independently visible while inactive"
        );
    }

    #[test]
    fn active_material_resolution_preserves_explicitly_absent_state_edges() {
        use crate::appearance::{ChromeColors, Color};
        let host = Color::rgb(0xffffff);
        let absent = Color::rgba(0x12345600);
        let reference = ChromeColors {
            background: host,
            focus_ring: absent,
            input_focused_border: absent,
            input_invalid_border: absent,
            ..ChromeColors::default()
        };

        for floors in [
            FloatingContrastFloors::STANDARD,
            FloatingContrastFloors::INCREASED,
        ] {
            let resolved = resolve_material_control_colors(
                &reference,
                reference.clone(),
                host,
                host,
                floors,
                floors.focus.is_some(),
            );

            assert_eq!(resolved.focus_ring, absent);
            assert_eq!(resolved.input_focused_border, absent);
            assert_eq!(resolved.input_invalid_border, absent);
        }
    }

    #[test]
    fn floating_constraints_distinguish_inside_content_from_host_adjacent_paint() {
        use crate::appearance::Color;

        let white = Color::rgb(0xffffff);
        let resolved = resolve_floating_state_at_alpha(
            floating_state(
                Color::rgb(0x000000),
                Color::rgb(0x000000),
                white,
                [
                    floating_constraint(white, 4.5),
                    floating_host_constraint(white, 4.5),
                    None,
                    None,
                ],
            ),
            255,
            [white; 2],
        )
        .expect("opposite inside and outside paints are jointly representable");

        assert_eq!(
            resolved.content[0], white,
            "inside content reads on the fill"
        );
        assert!(
            resolved.content[1].source_over(white).contrast_ratio(white) >= 4.5,
            "adjacent labels and perimeter strokes read on the host"
        );
    }

    #[test]
    fn unpainted_floating_content_resolves_on_the_host_without_forcing_family_opacity() {
        use crate::appearance::Color;

        let host = Color::rgb(0xffffff);
        let proposed = Color::rgb(0x999999);
        let resolved = resolve_floating_state_at_alpha(
            floating_state(
                Color::rgba(0),
                Color::rgba(0),
                host,
                [floating_constraint(proposed, 4.5), None, None, None],
            ),
            0,
            [host; 2],
        )
        .expect("unpainted content can move on its actual host without changing the fill");

        assert_eq!(resolved.fill.a, 0);
        assert!(resolved.content[0].contrast_ratio(host) >= 4.5);
    }

    #[test]
    fn filled_fallback_keeps_label_and_icon_on_one_readable_polarity() {
        use crate::appearance::Color;

        let fill = Color::rgb(0x333333);
        let states = [floating_state(
            fill,
            fill,
            fill,
            [
                floating_constraint(Color::rgb(0xffffff), 4.5),
                floating_constraint(Color::rgb(0x222222), 4.5),
                None,
                None,
            ],
        )];
        let (resolved, used_fallback) =
            super::resolve_floating_family_for_presentation(Ok(states), [fill; 2], &[])
                .expect("the opaque fill has a coherent readable endpoint");

        assert!(used_fallback);
        for content in [resolved[0].content[0], resolved[0].content[1]] {
            assert!(relative_luminance(content) > relative_luminance(fill));
            assert!(content.contrast_ratio(fill) >= 4.5);
        }
    }

    #[test]
    fn fixed_polarity_readability_moves_an_already_readable_opposite_ink() {
        use crate::appearance::Color;

        let fill = Color::rgb(0x757575);
        let opposite = Color::rgb(0x000000);
        assert!(opposite.contrast_ratio(fill) >= 4.5);

        let resolved = super::readable_toward_endpoint(opposite, Color::rgb(0xffffff), [fill], 4.5);

        assert!(relative_luminance(resolved) > relative_luminance(fill));
        assert!(resolved.contrast_ratio(fill) >= 4.5);
    }

    #[test]
    fn panel_and_card_controls_preserve_the_authored_root_relative_step() {
        use crate::appearance::{ChromeColors, Color, CompositionCapabilities, SurfaceMaterials};
        let authored = ChromeColors::default();
        for host in [
            authored.background,
            Color::rgb(0x202020),
            Color::rgb(0x303030),
        ] {
            let prepared = super::prepare_state_control_host(
                &authored,
                (host, host),
                spaceterm_ui::ControlHost::Panel,
                SurfaceMaterials::OPAQUE,
                super::ChromeStatePolicy {
                    active: true,
                    capabilities: CompositionCapabilities::default(),
                },
                super::FloatingContrastFloors::STANDARD,
                false,
                false,
            );
            for (fill, actual) in [
                (
                    authored.element_background,
                    prepared.reference.element_background,
                ),
                (authored.element_hover, prepared.reference.element_hover),
                (authored.element_active, prepared.reference.element_active),
            ] {
                assert_eq!(
                    actual,
                    host_relative_fill(fill, authored.background, host).unwrap(),
                    "host={host:?}, authored fill={fill:?}"
                );
            }
        }
    }

    #[test]
    fn host_relative_fill_preserves_identity_and_mixed_direction_relationships() {
        use crate::appearance::Color;

        let root = Color::rgb(0x151515);
        let host = Color::rgb(0x202020);
        let authored = [
            Color::rgb(0x202020),
            Color::rgb(0x1d1d1d),
            Color::rgb(0x242424),
        ];
        assert_eq!(
            authored.map(|fill| host_relative_fill(fill, root, root).unwrap()),
            authored,
        );

        let rehosted = authored.map(|fill| host_relative_fill(fill, root, host).unwrap());
        for (left, right) in [(0, 1), (0, 2), (1, 2)] {
            assert_eq!(
                relative_luminance(authored[left])
                    .partial_cmp(&relative_luminance(authored[right])),
                relative_luminance(rehosted[left])
                    .partial_cmp(&relative_luminance(rehosted[right])),
            );
            let authored_ratio = (relative_luminance(authored[left]) + 0.05)
                / (relative_luminance(authored[right]) + 0.05);
            let rehosted_ratio = (relative_luminance(rehosted[left]) + 0.05)
                / (relative_luminance(rehosted[right]) + 0.05);
            assert!(
                (authored_ratio - rehosted_ratio).abs() < 0.012,
                "pair {left}-{right}: authored={authored_ratio}, rehosted={rehosted_ratio}, colors={rehosted:?}",
            );
        }
    }

    #[test]
    fn host_relative_fill_handles_black_and_reports_out_of_gamut_steps() {
        use crate::appearance::Color;

        assert_eq!(
            host_relative_fill(
                Color::rgb(0x000000),
                Color::rgb(0x000000),
                Color::rgb(0x202020),
            ),
            Some(Color::rgb(0x202020)),
        );
        assert_eq!(
            host_relative_fill(
                Color::rgb(0xffffff),
                Color::rgb(0x000000),
                Color::rgb(0xffffff),
            ),
            None,
        );
    }

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
        let (_, resolved) = resolve_floating_field_colors(
            &reference,
            FloatingContrastFloors::STANDARD,
            reference.clone(),
            paint,
            Color::rgba(0),
            Color::rgba(0),
        );

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
        let resolved = resolve_floating_segmented_colors(
            &reference,
            FloatingContrastFloors::STANDARD,
            &reference,
            paint,
            Color::rgba(0),
            Color::rgba(0),
        );

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
        let resolved = resolve_floating_control_colors(
            &reference,
            FloatingContrastFloors::STANDARD,
            &reference,
            paint,
            material,
            Color::rgba(0),
        );

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
            FloatingContrastFloors::STANDARD,
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
}

#[derive(Clone)]
pub(crate) struct InstalledChrome {
    pub(crate) active: Arc<ChromeAppearance>,
    pub(crate) inactive: Arc<ChromeAppearance>,
}
impl Global for InstalledChrome {}

impl InstalledChrome {
    pub(crate) fn single(appearance: Arc<ChromeAppearance>) -> Self {
        Self {
            active: appearance.clone(),
            inactive: appearance,
        }
    }
}

pub(crate) fn chrome(cx: &App) -> &ChromeAppearance {
    if spaceterm_ui::ControlThemeScope::current() == spaceterm_ui::ControlThemeScope::Settings
        && cx.has_global::<settings::InstalledSettingsChrome>()
    {
        return settings::selected(cx).chrome.as_ref();
    }
    selected_chrome(cx)
}

/// Retains the immutable prepared variant for render closures without copying its catalogs.
pub(crate) fn shared_chrome(cx: &App) -> Arc<ChromeAppearance> {
    if spaceterm_ui::ControlThemeScope::current() == spaceterm_ui::ControlThemeScope::Settings
        && cx.has_global::<settings::InstalledSettingsChrome>()
    {
        return Arc::clone(&settings::selected(cx).chrome);
    }
    Arc::clone(selected_chrome(cx))
}

fn selected_chrome(cx: &App) -> &Arc<ChromeAppearance> {
    let installed = cx.global::<InstalledChrome>();
    match spaceterm_ui::ControlWindowActivity::current() {
        spaceterm_ui::ControlWindowActivity::Active => &installed.active,
        spaceterm_ui::ControlWindowActivity::Inactive => &installed.inactive,
    }
}

pub(crate) fn window_activity(window: &Window) -> spaceterm_ui::ControlWindowActivity {
    if window.is_window_active() {
        spaceterm_ui::ControlWindowActivity::Active
    } else {
        spaceterm_ui::ControlWindowActivity::Inactive
    }
}

pub(crate) fn initialize(cx: &mut App) {
    if !cx.has_global::<InstalledChrome>() {
        cx.set_global(InstalledChrome::single(Arc::new(
            ChromeAppearance::default(),
        )));
    }
    if !cx.has_global::<settings::InstalledSettingsChrome>() {
        let chrome = cx.global::<InstalledChrome>().active.as_ref().clone();
        cx.set_global(settings::InstalledSettingsChrome::single(chrome));
    }
}
