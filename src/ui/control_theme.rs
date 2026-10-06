mod button;
mod combo_box;
mod command_palette;
mod menu;
pub(super) mod modal;
pub(super) mod progress;
pub(super) mod resize_handle;
mod scrollbar;
mod search_field;
pub(super) mod segmented_control;
mod text_input;
mod toggle;
mod tooltip;

use crate::ui::appearance::gpui_color;
use gpui::px;
use spaceterm_ui::{ControlShadow, ControlShadowLayer, ControlThemeCatalog};

use crate::appearance::{Appearance, Color};

use super::chrome_typography::{ChromeTypography, TextRole};

pub(super) struct PopupUnfocusedRows {
    pub(super) reference: crate::appearance::ChromeColors,
    pub(super) paint: crate::appearance::ChromeColors,
}

pub(super) fn popup_unfocused_rows(
    appearance: &super::appearance::ChromeAppearance,
    popup: &crate::appearance::ChromeColors,
) -> PopupUnfocusedRows {
    let reference = appearance
        .unfocused_selection_colors(spaceterm_ui::ControlHost::Floating)
        .clone();
    let mut paint = reference.clone();
    paint.elevated_surface_background = popup.elevated_surface_background;
    PopupUnfocusedRows { reference, paint }
}

pub(super) fn catalog(
    appearance: &super::appearance::ChromeAppearance,
    motion: spaceterm_ui::ControlMotion,
) -> ControlThemeCatalog {
    // Controls paint the window's material. Floating controls use the same semantic colors as
    // their host, with fills compiled into overlays so the shell remains visible beneath them.
    // Overlay rows also receive the opaque presentation as their contrast reference.
    let reference = &appearance.colors;
    let colors = &appearance.control_colors;
    let segmented = &appearance.segmented_control_colors;
    let host = &appearance.floating_colors;
    let floating_controls = &appearance.floating_control_colors;
    let floating_segmented = &appearance.floating_segmented_colors;
    let field_reference = &appearance.floating_field_reference;
    let field = &appearance.floating_field_colors;
    // The shell applies its backdrop tone and elevation wash once. Idle rows inherit that host,
    // while row states keep their complete host-relative paints and content contrast reference.
    let mut popup = host.clone();
    popup.elevated_surface_background =
        appearance.floating_surface(host.elevated_surface_background);
    let unfocused_rows = popup_unfocused_rows(appearance, &popup);
    let row_reference = host.clone();
    let row_policy = OverlayRowPolicy::prepared(appearance);
    let title_bar_controls = surface_control_themes(
        &appearance.title_bar_controls,
        &row_reference,
        &popup,
        appearance,
    );
    let panel_controls = surface_control_themes(
        &appearance.panel_controls,
        &row_reference,
        &popup,
        appearance,
    );
    let card_controls = surface_control_themes(
        &appearance.card_controls,
        &row_reference,
        &popup,
        appearance,
    );
    let catalog = ControlThemeCatalog::new(
        button::prepared(
            colors,
            &appearance.typography,
            &appearance.icons,
            appearance.capabilities.show_borders,
        ),
        toggle::prepared(colors, &appearance.typography),
        progress::theme(colors),
        scrollbar::theme(colors),
        resize_handle::theme(colors),
        segmented_control::prepared(
            segmented,
            &appearance.typography,
            appearance.capabilities.show_borders,
        ),
        search_field::prepared(reference, colors, &appearance.typography, &appearance.icons),
        menu::prepared_with_rows(
            &row_reference,
            colors,
            &popup,
            Some((&unfocused_rows.reference, &unfocused_rows.paint)),
            &appearance.typography,
            &appearance.icons,
            row_policy,
        ),
        command_palette::prepared(
            &row_reference,
            &popup,
            Some((&unfocused_rows.reference, &unfocused_rows.paint)),
            &appearance.typography,
            &appearance.icons,
            row_policy,
        ),
        combo_box::prepared_with_rows(
            &row_reference,
            colors,
            &popup,
            Some((&unfocused_rows.reference, &unfocused_rows.paint)),
            &appearance.typography,
            &appearance.icons,
            row_policy,
        ),
        text_input::theme(colors),
        tooltip::prepared(host, &appearance.typography),
        modal::theme(floating_controls),
        motion,
    )
    .title_bar_controls(title_bar_controls)
    .resting_controls(panel_controls, card_controls)
    .floating(
        appearance.floating_surfaces(),
        spaceterm_ui::SurfaceControlThemes::new(
            button::prepared(
                floating_controls,
                &appearance.typography,
                &appearance.icons,
                appearance.capabilities.show_borders,
            ),
            toggle::prepared(floating_controls, &appearance.typography),
            progress::theme(floating_controls),
            segmented_control::prepared(
                floating_segmented,
                &appearance.typography,
                appearance.capabilities.show_borders,
            ),
            search_field::prepared(
                field_reference,
                field,
                &appearance.typography,
                &appearance.icons,
            ),
            text_input::themed(field, host),
        )
        .triggers(
            menu::prepared_with_rows(
                &row_reference,
                floating_controls,
                &popup,
                Some((&unfocused_rows.reference, &unfocused_rows.paint)),
                &appearance.typography,
                &appearance.icons,
                row_policy,
            ),
            combo_box::prepared_with_rows(
                &row_reference,
                floating_controls,
                &popup,
                Some((&unfocused_rows.reference, &unfocused_rows.paint)),
                &appearance.typography,
                &appearance.icons,
                row_policy,
            ),
        ),
    )
    .typography(prepared_control_typography(&appearance.typography))
    // Typography already includes base-size and density changes.
    .scale_spacing(appearance.spacing_scale);

    let shadow_opacity = if appearance.active { 89 } else { 53 };
    let shadow = ControlShadow::single(ControlShadowLayer::new(
        gpui_color(appearance.colors.shadow.multiply_opacity(shadow_opacity)).into(),
        px(0.0),
        px(1.0),
        px(2.0),
        px(-1.0),
    ));
    let preserve_accessibility_border =
        appearance.capabilities.increase_contrast || appearance.capabilities.show_borders;
    let light = appearance.appearance == Appearance::Light;
    // Light controls need a stronger edge than their card or popup host.
    let ink = crate::appearance::LIGHT_BOUNDARY_INK;
    let rest_edge = ink.control;
    let selected_edge = ink.rule;
    let border = (light && !preserve_accessibility_border).then_some(gpui_color(rest_edge));
    let catalog = catalog.toggle_segmented_elevation(
        if light { shadow } else { ControlShadow::none() },
        border,
        shadow,
        (light && !preserve_accessibility_border).then_some(gpui_color(selected_edge)),
    );

    if !light {
        return catalog;
    }

    let catalog = catalog.ordinary_control_elevation(shadow, border);
    if !preserve_accessibility_border {
        let interaction_edge = if appearance.active {
            ink.strong
        } else {
            rest_edge
        };
        catalog.ordinary_control_borders(spaceterm_ui::ControlBorderStates::new(
            gpui_color(rest_edge),
            gpui_color(interaction_edge),
            gpui_color(interaction_edge),
            gpui_color(ink.disabled),
        ))
    } else {
        catalog
    }
}

fn prepared_control_typography(typography: &ChromeTypography) -> spaceterm_ui::ControlTypography {
    spaceterm_ui::ControlTypography::new(
        typography.style(TextRole::Body).font.clone(),
        typography.style(TextRole::BodyEmphasis).font.clone(),
        typography.style(TextRole::Title).font.clone(),
    )
    .section(typography.style(TextRole::Section).font.clone())
    .semantic_fonts(
        typography.style(TextRole::Shortcut).font.clone(),
        typography.style(TextRole::Caption).font.clone(),
        typography.style(TextRole::Badge).font.clone(),
    )
}

pub(super) fn surface_control_themes(
    host: &super::appearance::PreparedControlHost,
    row_reference: &crate::appearance::ChromeColors,
    popup: &crate::appearance::ChromeColors,
    appearance: &super::appearance::ChromeAppearance,
) -> spaceterm_ui::SurfaceControlThemes {
    let typography = &appearance.typography;
    let icons = &appearance.icons;
    let show_borders = appearance.capabilities.show_borders;
    let row_policy = OverlayRowPolicy::prepared(appearance);
    let unfocused_rows = popup_unfocused_rows(appearance, popup);
    spaceterm_ui::SurfaceControlThemes::new(
        button::prepared(&host.colors, typography, icons, show_borders),
        toggle::prepared(&host.colors, typography),
        progress::theme(&host.colors),
        segmented_control::prepared(&host.segmented, typography, show_borders),
        search_field::prepared(&host.reference, &host.colors, typography, icons),
        text_input::theme(&host.colors),
    )
    .triggers(
        menu::prepared_with_rows(
            row_reference,
            &host.colors,
            popup,
            Some((&unfocused_rows.reference, &unfocused_rows.paint)),
            typography,
            icons,
            row_policy,
        ),
        combo_box::prepared_with_rows(
            row_reference,
            &host.colors,
            popup,
            Some((&unfocused_rows.reference, &unfocused_rows.paint)),
            typography,
            icons,
            row_policy,
        ),
    )
}

/// One overlay row state in application colors.
#[derive(Clone, Copy)]
pub(super) struct OverlayRowPolicy {
    primary: f64,
    secondary: f64,
    disabled: f64,
    selection: Option<f64>,
    boundary: Option<f64>,
    disabled_selected_fill: Option<Color>,
    active: bool,
}

impl Default for OverlayRowPolicy {
    fn default() -> Self {
        Self {
            primary: 4.5,
            secondary: 4.5,
            disabled: 4.5,
            selection: None,
            boundary: None,
            disabled_selected_fill: None,
            active: true,
        }
    }
}

impl OverlayRowPolicy {
    pub(super) fn prepared(appearance: &super::appearance::ChromeAppearance) -> Self {
        let increased = appearance.capabilities.increase_contrast;
        // Preserve an authored neutral selection when its host already distinguishes it. A
        // universal 1.25 floor would invert near-white selected chips on light raised surfaces.
        let reference = &appearance.floating_colors;
        let host = reference.elevated_surface_background;
        let authored_selection = reference
            .row_selected_background
            .source_over(host)
            .contrast_ratio(host)
            .clamp(1.12, 1.25);
        Self {
            primary: if increased { 7.0 } else { 4.5 },
            secondary: if increased { 4.5 } else { 3.0 },
            disabled: if increased { 4.5 } else { 3.0 },
            selection: Some(if increased {
                1.40
            } else if appearance.active {
                authored_selection
            } else {
                super::appearance::SUBDUED_SELECTION_CONTRAST
            }),
            boundary: increased.then_some(3.0),
            disabled_selected_fill: Some(appearance.floating_disabled_selected_background),
            active: appearance.active,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct OverlayRow {
    pub(super) fill: crate::appearance::Color,
    /// Foreground, secondary, icon and match colors, readable across the final backdrop endpoints.
    pub(super) content: [crate::appearance::Color; 4],
    pub(super) border: crate::appearance::Color,
}

impl OverlayRow {
    /// Resolves content against both admitted material endpoints and paints the smallest
    /// host-relative row state that can carry that content.
    #[cfg(test)]
    pub(super) fn resolve(
        (reference, reference_surface): (crate::appearance::Color, crate::appearance::Color),
        (paint, paint_surface): (crate::appearance::Color, crate::appearance::Color),
        content: [crate::appearance::Color; 4],
        border: crate::appearance::Color,
    ) -> Self {
        Self::resolve_with_floors(
            (reference, reference_surface),
            (paint, paint_surface),
            content,
            border,
            [4.5; 4],
            None,
            None,
        )
    }

    pub(super) fn resolve_with_floors(
        (reference, reference_surface): (Color, Color),
        (paint, paint_surface): (Color, Color),
        content: [Color; 4],
        border: Color,
        floors: [f64; 4],
        boundary: Option<f64>,
        selection: Option<f64>,
    ) -> Self {
        let minimum = floors.into_iter().fold(0.0, f64::max);
        let mut fill = row_fill((reference, reference_surface), (paint, paint_surface));
        if !paint_surface.is_opaque()
            && let Some(compressed) = readable_material_row_fill(fill, paint_surface, minimum)
        {
            fill = compressed;
        }
        let mut backgrounds = row_backgrounds(fill, paint_surface);
        if let Some(selection) = selection {
            fill = readable_selection_fill(
                fill,
                paint_surface,
                reference.source_over(reference_surface),
                minimum,
                selection,
            );
            backgrounds = row_backgrounds(fill, paint_surface);
        }
        if shared_neutral(backgrounds, minimum).is_none() {
            fill = super::chrome_state::contrast_host(
                reference.source_over(reference_surface),
                minimum,
            );
            backgrounds = [fill; 2];
        }
        Self {
            fill,
            content: std::array::from_fn(|index| {
                super::appearance::readable_on_backgrounds(
                    content[index],
                    backgrounds,
                    floors[index],
                )
            }),
            border: if let Some(minimum) = boundary.filter(|_| border.a > 0) {
                super::appearance::readable_on_backgrounds(border, backgrounds, minimum)
            } else if paint_surface == reference_surface || fill.is_opaque() {
                border
            } else {
                relative_edge(reference.source_over(reference_surface), border)
            },
        }
    }

    pub(super) fn paint(self) -> spaceterm_ui::ListRowPaint {
        let [foreground, secondary, icon, matched] = self.content;
        spaceterm_ui::ListRowPaint::new(
            gpui_color(self.fill),
            gpui_color(foreground),
            gpui_color(secondary),
            gpui_color(icon),
            gpui_color(matched),
            gpui_color(self.border),
        )
    }
}

fn row_backgrounds(
    fill: crate::appearance::Color,
    surface: crate::appearance::Color,
) -> [crate::appearance::Color; 2] {
    if surface.is_opaque() || fill.is_opaque() {
        return [fill.source_over(surface); 2];
    }
    [
        crate::appearance::Color::BLACK,
        crate::appearance::Color::WHITE,
    ]
    .map(|underlay| {
        let background = fill.source_over(surface.source_over(underlay));
        if fill.a == 0 {
            return background;
        }
        // The catalog folds tone and wash into one material. Rendering blends them separately,
        // so adding a row fill can move the endpoint one level beyond that folded estimate.
        // Idle rows keep the shell's already-resolved content and do not add this extra layer.
        let channel = |value: u8| {
            if underlay.r == 0 {
                value.saturating_sub(1)
            } else {
                value.saturating_add(1)
            }
        };
        crate::appearance::Color::from_rgb_components(
            channel(background.r),
            channel(background.g),
            channel(background.b),
        )
    })
}

/// Raises a selected overlay only as far as needed to retain both selection and readable text.
/// If transmission cannot satisfy both floors, select the nearest readable opaque fallback.
fn readable_selection_fill(
    fill: Color,
    surface: Color,
    reference: Color,
    text_floor: f64,
    selection_floor: f64,
) -> Color {
    let hosts = row_backgrounds(Color::rgba(0), surface);
    let acceptable = |candidate: Color| {
        let backgrounds = row_backgrounds(candidate, surface);
        shared_neutral(backgrounds, text_floor).is_some()
            && backgrounds
                .into_iter()
                .zip(hosts)
                .all(|(background, host)| background.contrast_ratio(host) >= selection_floor)
    };
    if acceptable(fill) {
        return fill;
    }
    for alpha in fill.a..=255 {
        let candidate = fill.with_alpha(alpha);
        if acceptable(candidate) {
            return candidate;
        }
    }
    let distance = |candidate: Color| {
        u32::from(candidate.r.abs_diff(reference.r))
            + u32::from(candidate.g.abs_diff(reference.g))
            + u32::from(candidate.b.abs_diff(reference.b))
    };
    let mut endpoints = [Color::BLACK, Color::WHITE];
    endpoints.sort_by_key(|endpoint| distance(*endpoint));
    for endpoint in endpoints {
        if let Some(candidate) = (1..u8::MAX)
            .map(|alpha| endpoint.with_alpha(alpha))
            .find(|candidate| acceptable(*candidate))
        {
            return candidate;
        }
    }
    endpoints
        .into_iter()
        .flat_map(|endpoint| {
            (0..=255).map(move |step| reference.mix(endpoint, f64::from(step) / 255.0))
        })
        .filter(|candidate| acceptable(*candidate))
        .min_by_key(|candidate| distance(*candidate))
        .unwrap_or(fill)
}

fn shared_neutral(
    backgrounds: [crate::appearance::Color; 2],
    minimum: f64,
) -> Option<crate::appearance::Color> {
    let score = |foreground: crate::appearance::Color| {
        backgrounds
            .into_iter()
            .map(|background| foreground.contrast_ratio(background))
            .fold(f64::INFINITY, f64::min)
    };
    let dark = crate::appearance::Color::BLACK;
    let light = crate::appearance::Color::WHITE;
    let (foreground, contrast) = if score(dark) >= score(light) {
        (dark, score(dark))
    } else {
        (light, score(light))
    };
    (contrast >= minimum).then_some(foreground)
}

/// Compresses one row-state overlay into the readable interval connected to transparent.
///
/// Fully opaque ink can become readable again after crossing an inaccessible middle interval;
/// stopping at the first failure keeps the result on the material side of that interval. The
/// compression curve preserves ordering among states that share the same ink and host instead of
/// flattening each of them onto one alpha ceiling.
fn readable_material_row_fill(
    fill: crate::appearance::Color,
    surface: crate::appearance::Color,
    minimum: f64,
) -> Option<crate::appearance::Color> {
    let foreground = shared_neutral(row_backgrounds(fill.with_alpha(0), surface), minimum)?;
    if fill.a == 0 {
        return Some(fill);
    }
    let ink = fill.with_alpha(255);
    let mut ceiling = 0_u8;
    for alpha in 1..=u8::MAX {
        let backgrounds = row_backgrounds(ink.with_alpha(alpha), surface);
        if backgrounds
            .into_iter()
            .all(|background| foreground.contrast_ratio(background) >= minimum)
        {
            ceiling = alpha;
        } else {
            break;
        }
    }
    if ceiling == 0 {
        return None;
    }
    let ceiling = f64::from(ceiling) / 255.0;
    let alpha = f64::from(fill.a) / 255.0;
    let compressed = ceiling * alpha * (1.0 + ceiling) / (alpha + ceiling);
    Some(fill.with_alpha((compressed.clamp(0.0, ceiling) * 255.0).round() as u8))
}

#[cfg(test)]
pub(super) fn replace_uniform_control_catalog(
    cx: &mut gpui::App,
    chrome: &super::appearance::ChromeAppearance,
) -> Result<spaceterm_ui::ControlThemeReplacement, spaceterm_ui::ControlThemeCatalogError> {
    let catalog = Box::new(catalog(chrome, spaceterm_ui::ControlMotion::Standard));
    spaceterm_ui::replace_control_theme_catalogs(
        cx,
        catalog.clone(),
        catalog.clone(),
        catalog.clone(),
        catalog,
    )
}

#[cfg(test)]
pub(super) fn overlay_list_rows(
    reference: &crate::appearance::ChromeColors,
    paint: &crate::appearance::ChromeColors,
) -> spaceterm_ui::ListRowPaints {
    overlay_list_rows_with_policy(reference, paint, OverlayRowPolicy::default())
}

pub(super) fn overlay_list_rows_with_policy(
    reference: &crate::appearance::ChromeColors,
    paint: &crate::appearance::ChromeColors,
    policy: OverlayRowPolicy,
) -> spaceterm_ui::ListRowPaints {
    use spaceterm_ui::ListRowPaints;
    let surfaces = (
        reference.elevated_surface_background,
        paint.elevated_surface_background,
    );
    let row =
        |selected: bool,
         pick: fn(&crate::appearance::ChromeColors) -> [crate::appearance::Color; 6]| {
            let [fill, foreground, secondary, icon, matched, border] = pick(reference);
            OverlayRow::resolve_with_floors(
                (fill, surfaces.0),
                (pick(paint)[0], surfaces.1),
                [foreground, secondary, icon, matched],
                border,
                [
                    policy.primary,
                    policy.secondary,
                    policy.primary,
                    policy.primary,
                ],
                policy.boundary,
                if selected { policy.selection } else { None },
            )
            .paint()
        };
    let disabled = OverlayRow::resolve_with_floors(
        (surfaces.0, surfaces.0),
        (surfaces.1, surfaces.1),
        [
            reference.text_disabled,
            reference.text_disabled,
            reference.icon_disabled,
            reference.text_disabled,
        ],
        reference.row_border,
        [policy.disabled; 4],
        policy.boundary,
        None,
    )
    .paint();
    let disabled_selected_fill = policy
        .disabled_selected_fill
        .unwrap_or(reference.row_selected_background);
    let disabled_selected = OverlayRow::resolve_with_floors(
        (disabled_selected_fill, surfaces.0),
        (
            policy
                .disabled_selected_fill
                .unwrap_or(paint.row_selected_background),
            surfaces.1,
        ),
        [
            reference.text_disabled,
            reference.text_disabled,
            reference.icon_disabled,
            reference.text_disabled,
        ],
        reference.row_selected_border,
        [policy.disabled; 4],
        policy.boundary,
        None,
    )
    .paint();
    let normal = row(false, |c| {
        [
            c.elevated_surface_background,
            c.row_foreground,
            c.row_secondary,
            c.row_icon,
            c.row_match,
            c.row_border,
        ]
    });
    let hovered = row(false, |c| {
        [
            c.row_hover_background,
            c.row_hover_foreground,
            c.row_hover_secondary,
            c.row_hover_icon,
            c.row_hover_match,
            c.row_hover_border,
        ]
    });
    let selected = row(true, |c| {
        [
            c.row_selected_background,
            c.row_selected_foreground,
            c.row_selected_secondary,
            c.row_selected_icon,
            c.row_selected_match,
            c.row_selected_border,
        ]
    });
    let selected_hovered = row(true, |c| {
        [
            c.row_selected_hover_background,
            c.row_selected_hover_foreground,
            c.row_selected_hover_secondary,
            c.row_selected_hover_icon,
            c.row_selected_hover_match,
            c.row_selected_hover_border,
        ]
    });
    ListRowPaints::new(
        normal,
        if policy.active { hovered } else { normal },
        selected,
        if policy.active {
            selected_hovered
        } else {
            selected
        },
        disabled,
        disabled_selected,
    )
}

/// What a row paints over the surface it rests on. Without a material it paints its authored
/// composite. With a material it paints the smallest host-relative overlay that reaches the same
/// state, and an idle row paints nothing, so the surface is never composited twice.
pub(super) fn row_fill(
    (reference, reference_surface): (crate::appearance::Color, crate::appearance::Color),
    (paint, paint_surface): (crate::appearance::Color, crate::appearance::Color),
) -> crate::appearance::Color {
    if paint == reference && paint_surface == reference_surface {
        reference.source_over(reference_surface)
    } else if reference == reference_surface {
        crate::appearance::Color::rgba(0)
    } else {
        let target = reference.source_over(reference_surface);
        let base = if paint_surface.is_opaque() {
            paint_surface
        } else {
            reference_surface
        };
        target.relative_overlay(base)
    }
}

/// Reconstructs an authored edge over its semantic host for a translucent row.
pub(super) fn relative_edge(
    host: crate::appearance::Color,
    edge: crate::appearance::Color,
) -> crate::appearance::Color {
    if edge.a == 0 {
        return edge;
    }
    edge.source_over(host).relative_overlay(host)
}

pub(super) fn readable_on(
    proposed: crate::appearance::Color,
    background: crate::appearance::Color,
    minimum_contrast: f64,
) -> crate::appearance::Color {
    super::appearance::readable_on_background(proposed, background, minimum_contrast)
}

#[cfg(test)]
mod modal_tests;
#[cfg(test)]
mod tests;
