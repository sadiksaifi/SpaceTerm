use gpui::px;
use spaceterm_ui::{ControlShadow, ControlShadowLayer, ControlThemeCatalog};

use crate::appearance::{Appearance, Color};

use super::chrome_typography::{ChromeTypography, TextRole};

use super::{
    button_theme, combo_box_theme, command_palette_theme, menu_theme, modal_theme, progress_theme,
    resize_handle_theme, scrollbar_theme, search_field_theme, segmented_control_theme,
    text_input_theme, toggle_theme, tooltip_theme,
};

pub(super) fn catalog(
    appearance: &super::appearance::ChromeAppearance,
    progress_motion: spaceterm_ui::ProgressMotion,
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
        appearance.floating_surface(appearance.colors.elevated_surface_background);
    let mut unfocused_popup = appearance
        .unfocused_selection_colors(spaceterm_ui::ControlHost::Floating)
        .clone();
    unfocused_popup.elevated_surface_background = popup.elevated_surface_background;
    let row_reference = host.clone();
    let row_policy = OverlayRowPolicy::prepared(appearance);
    let title_bar_controls = surface_control_themes(
        &appearance.title_bar_controls,
        &row_reference,
        &popup,
        progress_motion,
        appearance,
    );
    let panel_controls = surface_control_themes(
        &appearance.panel_controls,
        &row_reference,
        &popup,
        progress_motion,
        appearance,
    );
    let card_controls = surface_control_themes(
        &appearance.card_controls,
        &row_reference,
        &popup,
        progress_motion,
        appearance,
    );
    let catalog = ControlThemeCatalog::new(
        button_theme::prepared(
            colors,
            &appearance.typography,
            &appearance.icons,
            appearance.capabilities.show_borders,
        ),
        toggle_theme::prepared(colors, &appearance.typography),
        progress_theme::theme(colors, progress_motion),
        scrollbar_theme::theme(colors),
        resize_handle_theme::theme(colors),
        segmented_control_theme::prepared(
            segmented,
            &appearance.typography,
            appearance.capabilities.show_borders,
        ),
        search_field_theme::prepared(reference, colors, &appearance.typography, &appearance.icons),
        menu_theme::prepared_with_rows(
            &row_reference,
            colors,
            &popup,
            Some(&unfocused_popup),
            &appearance.typography,
            &appearance.icons,
            row_policy,
        ),
        command_palette_theme::prepared(
            &row_reference,
            &popup,
            Some(&unfocused_popup),
            &appearance.typography,
            &appearance.icons,
            row_policy,
        ),
        combo_box_theme::prepared_with_rows(
            &row_reference,
            colors,
            &popup,
            Some(&unfocused_popup),
            &appearance.typography,
            &appearance.icons,
            row_policy,
        ),
        text_input_theme::theme(colors),
        tooltip_theme::prepared(host, &appearance.typography),
        modal_theme::theme(floating_controls),
    )
    .title_bar_controls(title_bar_controls)
    .resting_controls(panel_controls, card_controls)
    .floating(
        appearance.floating_surfaces(),
        spaceterm_ui::SurfaceControlThemes::new(
            button_theme::prepared(
                floating_controls,
                &appearance.typography,
                &appearance.icons,
                appearance.capabilities.show_borders,
            ),
            toggle_theme::prepared(floating_controls, &appearance.typography),
            progress_theme::theme(floating_controls, progress_motion),
            segmented_control_theme::prepared(
                floating_segmented,
                &appearance.typography,
                appearance.capabilities.show_borders,
            ),
            search_field_theme::prepared(
                field_reference,
                field,
                &appearance.typography,
                &appearance.icons,
            ),
            text_input_theme::themed(field, host),
        )
        .triggers(
            menu_theme::prepared_with_rows(
                &row_reference,
                floating_controls,
                &popup,
                Some(&unfocused_popup),
                &appearance.typography,
                &appearance.icons,
                row_policy,
            ),
            combo_box_theme::prepared_with_rows(
                &row_reference,
                floating_controls,
                &popup,
                Some(&unfocused_popup),
                &appearance.typography,
                &appearance.icons,
                row_policy,
            ),
        ),
    )
    .typography(prepared_control_typography(&appearance.typography))
    // Role sizes already include the additive base-size and density policy. Only structural
    // spacing still uses the legacy scaling seam while the reusable catalog migrates family by
    // family.
    .scale_metrics(1.0, appearance.spacing_scale)
    .focus_ring_width(px(if appearance.capabilities.increase_contrast {
        2.0
    } else {
        1.0
    }));

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
    let border =
        (light && !preserve_accessibility_border).then_some(gpui_color(Color::rgba(0x00000026)));
    let catalog = catalog.toggle_segmented_elevation(
        if light { shadow } else { ControlShadow::none() },
        border,
        shadow,
        light.then_some(gpui_color(Color::rgba(0x0000001f))),
    );

    if !light {
        return catalog;
    }

    catalog.ordinary_control_elevation(shadow, border)
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
    progress_motion: spaceterm_ui::ProgressMotion,
    appearance: &super::appearance::ChromeAppearance,
) -> spaceterm_ui::SurfaceControlThemes {
    let typography = &appearance.typography;
    let icons = &appearance.icons;
    let show_borders = appearance.capabilities.show_borders;
    let row_policy = OverlayRowPolicy::prepared(appearance);
    let mut unfocused_popup = appearance
        .unfocused_selection_colors(spaceterm_ui::ControlHost::Floating)
        .clone();
    unfocused_popup.elevated_surface_background = popup.elevated_surface_background;
    spaceterm_ui::SurfaceControlThemes::new(
        button_theme::prepared(&host.colors, typography, icons, show_borders),
        toggle_theme::prepared(&host.colors, typography),
        progress_theme::theme(&host.colors, progress_motion),
        segmented_control_theme::prepared(&host.segmented, typography, show_borders),
        search_field_theme::prepared(&host.reference, &host.colors, typography, icons),
        text_input_theme::theme(&host.colors),
    )
    .triggers(
        menu_theme::prepared_with_rows(
            row_reference,
            &host.colors,
            popup,
            Some(&unfocused_popup),
            typography,
            icons,
            row_policy,
        ),
        combo_box_theme::prepared_with_rows(
            row_reference,
            &host.colors,
            popup,
            Some(&unfocused_popup),
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
        Self {
            primary: if increased { 7.0 } else { 4.5 },
            secondary: if increased { 4.5 } else { 3.0 },
            disabled: if increased { 4.5 } else { 3.0 },
            selection: Some(if increased {
                1.40
            } else if appearance.active {
                1.25
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
        if shared_neutral(backgrounds, minimum).is_none() {
            fill = super::chrome_state::contrast_host(
                reference.source_over(reference_surface),
                minimum,
            );
            backgrounds = [fill; 2];
        }
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
        crate::appearance::Color::rgb(0x000000),
        crate::appearance::Color::rgb(0xffffff),
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
    [Color::rgb(0), Color::rgb(0xffffff)]
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
    let dark = crate::appearance::Color::rgb(0x000000);
    let light = crate::appearance::Color::rgb(0xffffff);
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
    )
    .disabled_selected(disabled_selected)
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

fn gpui_color(color: crate::appearance::Color) -> gpui::Rgba {
    gpui::rgba(color.rgba_hex())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::appearance::{
        ChromeColors, ChromeDensity, Color, FontStyle, ResolvedChromeTypography,
        ResolvedFontDescriptor,
    };

    struct ControlEdgeFixture(spaceterm_ui::ControlHost);

    impl gpui::Render for ControlEdgeFixture {
        fn render(
            &mut self,
            _: &mut gpui::Window,
            _: &mut gpui::Context<Self>,
        ) -> impl gpui::IntoElement {
            use gpui::{IntoElement as _, ParentElement as _, Styled as _, div};
            use spaceterm_ui::{
                Button, ButtonVariant, Checkbox, CheckboxState, ComboBox, ComboBoxItem, Icon,
                IconButton, IconName, Menu, MenuEntry, Picker, PickerOption, SegmentedControl,
                SegmentedOption, Switch,
            };

            self.0.mount(
                div()
                    .flex()
                    .flex_col()
                    .items_start()
                    .p(px(20.0))
                    .gap(px(8.0))
                    .child(
                        Button::new("edge-button", "Action")
                            .variant(ButtonVariant::Secondary)
                            .debug_selector("edge-button")
                            .on_activate(|_, _, _| {}),
                    )
                    .child(
                        IconButton::new("edge-icon", "Workspace", |color| {
                            Icon::new(IconName::Plus, px(14.0), color).into_any_element()
                        })
                        .variant(ButtonVariant::Secondary)
                        .debug_selector("edge-icon")
                        .on_activate(|_, _, _| {}),
                    )
                    .child(
                        ComboBox::new(
                            "edge-combo",
                            "Scheme",
                            Some(1),
                            "Choose",
                            vec![ComboBoxItem::new(1, "Scheme")],
                        )
                        .debug_selector("edge-combo")
                        .on_accept(|_, _, _| {}),
                    )
                    .child(
                        ComboBox::new(
                            "edge-icon-combo",
                            "Workspace",
                            Some(1),
                            "Choose",
                            vec![ComboBoxItem::new(1, "Workspace")],
                        )
                        .icon_trigger(|color, size| {
                            Icon::new(IconName::Plus, size, color).into_any_element()
                        })
                        .debug_selector("edge-icon-combo")
                        .on_accept(|_, _, _| {}),
                    )
                    .child(
                        Menu::new("edge-menu", "Actions", vec![MenuEntry::action("Open", ())])
                            .debug_selector("edge-menu")
                            .on_activate(|_, _, _| {}),
                    )
                    .child(
                        Picker::new(
                            "edge-picker",
                            "Scheme",
                            1,
                            vec![PickerOption::new(1, "Scheme")],
                        )
                        .unwrap()
                        .debug_selector("edge-picker")
                        .on_change(|_, _, _| {}),
                    )
                    .child(
                        SegmentedControl::new(
                            "edge-segmented",
                            "Density",
                            &false,
                            vec![
                                SegmentedOption::new(false, "Compact"),
                                SegmentedOption::new(true, "Comfortable"),
                            ],
                        )
                        .unwrap()
                        .debug_selector("edge-segmented")
                        .on_change(|_, _, _| {}),
                    )
                    .child(
                        Checkbox::new("edge-checkbox", "Choice", CheckboxState::Unchecked)
                            .debug_selector("edge-checkbox")
                            .on_change(|_, _, _| {}),
                    )
                    .child(
                        Switch::new("edge-switch", "Enabled", false)
                            .debug_selector("edge-switch")
                            .on_change(|_, _, _| {}),
                    ),
            )
        }
    }

    #[gpui::test]
    fn control_surfaces_do_not_paint_detached_bottom_hairlines(cx: &mut gpui::TestAppContext) {
        use crate::appearance::{
            AppearanceGeneration, AppearanceMode, AppearancePreferences, AvailableFonts,
            CompositionCapabilities, SchemeCatalog, SystemAppearance,
        };
        use crate::ui::appearance::ChromeAppearance;
        use spaceterm_ui::{ControlHost, ProgressMotion, replace_control_theme_catalog};

        cx.update(crate::ui::init).unwrap();
        let mut violations = Vec::new();
        for appearance in [Appearance::Light, Appearance::Dark] {
            for (increase_contrast, show_borders) in [(false, false), (true, false), (false, true)]
            {
                let resolved = SchemeCatalog::default()
                    .resolve(
                        AppearanceGeneration::INITIAL,
                        &AppearancePreferences {
                            mode: match appearance {
                                Appearance::Light => AppearanceMode::Light,
                                Appearance::Dark => AppearanceMode::Dark,
                            },
                            ..AppearancePreferences::default()
                        },
                        SystemAppearance::available(appearance).with_composition(
                            CompositionCapabilities {
                                increase_contrast,
                                show_borders,
                                ..CompositionCapabilities::new(true, true)
                            },
                        ),
                        &AvailableFonts::default(),
                    )
                    .unwrap();
                for active in [true, false] {
                    let prepared = ChromeAppearance::prepare_for_activity(&resolved.chrome, active);
                    cx.update(|cx| {
                        replace_control_theme_catalog(
                            cx,
                            catalog(&prepared, ProgressMotion::Standard),
                        )
                        .unwrap()
                    });
                    for host in [
                        ControlHost::Window,
                        ControlHost::TitleBar,
                        ControlHost::Panel,
                        ControlHost::Card,
                        ControlHost::Floating,
                    ] {
                        let (_, view) = cx.add_window_view(|_, _| ControlEdgeFixture(host));
                        view.simulate_resize(gpui::size(px(640.0), px(640.0)));
                        view.run_until_parked();
                        for selector in [
                            "edge-button",
                            "edge-icon",
                            "edge-combo",
                            "edge-icon-combo",
                            "edge-menu",
                            "edge-picker",
                            "edge-segmented",
                            "edge-checkbox-indicator",
                            "edge-switch-indicator",
                        ] {
                            let bounds = view
                                .debug_bounds(selector)
                                .unwrap_or_else(|| panic!("missing {selector}"));
                            view.update(|window, _| {
                                let bounds = bounds.scale(window.scale_factor());
                                let hairline = px(1.0).scale(window.scale_factor());
                                let quads = window.painted_quads_for_test();
                                if quads.iter().any(|quad| {
                                    let line = quad.visible_bounds;
                                    line.size.height == hairline && line.size.width > hairline * 2.0
                                        && line.left() > bounds.left() && line.right() < bounds.right()
                                        && line.bottom() <= bounds.bottom()
                                        && line.top() >= bounds.bottom() - hairline * 3.0
                                }) {
                                    violations.push(format!("{appearance:?}/{host:?}/{selector}/active={active}/contrast={increase_contrast}/borders={show_borders}"));
                                }
                            });
                        }
                    }
                }
            }
        }
        assert!(
            violations.is_empty(),
            "detached bottom hairlines: {violations:?}"
        );
    }

    #[test]
    fn control_catalog_preserves_exact_semantic_font_metadata() {
        fn descriptor(
            family: &str,
            fallback: &str,
            style: FontStyle,
            feature: &str,
        ) -> ResolvedFontDescriptor {
            ResolvedFontDescriptor {
                primary_family: family.to_owned(),
                fallback_families: vec![fallback.to_owned()],
                size: 13.0,
                line_height: 13.0,
                weight: 375,
                style,
                features: vec![feature.to_owned()],
                resolution_identity: family.to_owned(),
            }
        }

        let body = descriptor("Body Family", "Body Fallback", FontStyle::Italic, "+ss01");
        let caption = descriptor(
            "Caption Family",
            "Caption Fallback",
            FontStyle::Normal,
            "+ss02",
        );
        let heading = descriptor(
            "Heading Family",
            "Heading Fallback",
            FontStyle::Italic,
            "+ss03",
        );
        let resolved = ResolvedChromeTypography {
            body: body.clone(),
            small: caption.clone(),
            control: body.clone(),
            navigation: body.clone(),
            caption,
            heading,
            shortcut: body,
        };
        let typography = ChromeTypography::prepare(&resolved, ChromeDensity::Compact);
        let controls = prepared_control_typography(&typography);

        for (actual, role) in [
            (controls.shortcut(), TextRole::Shortcut),
            (controls.caption(), TextRole::Caption),
            (controls.badge(), TextRole::Badge),
        ] {
            assert_eq!(actual, &typography.style(role).font);
        }

        let shortcut = controls.shortcut();
        assert_eq!(shortcut.family.as_ref(), "Body Family");
        assert_eq!(shortcut.style, gpui::FontStyle::Italic);
        assert_eq!(shortcut.weight, gpui::FontWeight(375.0));
        assert_eq!(
            shortcut
                .fallbacks
                .as_ref()
                .expect("shortcut fallbacks")
                .0
                .as_ref(),
            &["Body Fallback".to_owned()]
        );
        assert!(
            shortcut
                .features
                .tag_value_list()
                .contains(&("ss01".to_owned(), 1))
        );
        assert!(
            shortcut
                .features
                .tag_value_list()
                .contains(&("tnum".to_owned(), 1))
        );

        let caption = controls.caption();
        assert_eq!(caption.family.as_ref(), "Caption Family");
        assert_eq!(caption.style, gpui::FontStyle::Normal);
        assert_eq!(caption.weight, gpui::FontWeight(375.0));
        assert_eq!(
            caption
                .fallbacks
                .as_ref()
                .expect("caption fallbacks")
                .0
                .as_ref(),
            &["Caption Fallback".to_owned()]
        );
        assert_eq!(caption.features.tag_value_list(), &[("ss02".to_owned(), 1)]);

        let badge = controls.badge();
        assert_eq!(badge.family.as_ref(), "Caption Family");
        assert_eq!(badge.style, gpui::FontStyle::Normal);
        assert_eq!(badge.weight, gpui::FontWeight(375.0));
        assert!(
            badge
                .features
                .tag_value_list()
                .contains(&("ss02".to_owned(), 1))
        );
        assert!(
            badge
                .features
                .tag_value_list()
                .contains(&("tnum".to_owned(), 1))
        );
    }

    #[test]
    fn selected_rows_keep_their_fill_floor_after_material_compression() {
        for (surface, state) in [
            (Color::rgba(0x202020c0), Color::rgb(0x323232)),
            (Color::rgba(0xf0f0f0c0), Color::rgb(0xdfdfdf)),
        ] {
            for (text_floor, selection_floor) in [(4.5, 1.25), (7.0, 1.40)] {
                let fill = readable_selection_fill(
                    state.relative_overlay(surface.with_alpha(255)),
                    surface,
                    state,
                    text_floor,
                    selection_floor,
                );
                let backgrounds = row_backgrounds(fill, surface);
                assert!(shared_neutral(backgrounds, text_floor).is_some());
                for (background, host) in backgrounds
                    .into_iter()
                    .zip(row_backgrounds(Color::rgba(0), surface))
                {
                    assert!(
                        background.contrast_ratio(host) >= selection_floor,
                        "{state:?}: {background:?} on {host:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn increased_contrast_row_content_meets_floors_on_final_material_endpoints() {
        for (surface, material) in [
            (Color::rgb(0x202020), Color::rgba(0x202020f3)),
            (Color::rgb(0xf0f0f0), Color::rgba(0xf0f0f0f3)),
        ] {
            for state in [surface, Color::rgb(0x606060), Color::rgb(0xa0a0a0)] {
                let floors = [7.0, 4.5, 7.0, 7.0];
                let row = OverlayRow::resolve_with_floors(
                    (state, surface),
                    (state, material),
                    [Color::rgb(0x777777); 4],
                    Color::rgb(0x777777),
                    floors,
                    Some(3.0),
                    None,
                );
                for background in row_backgrounds(row.fill, material) {
                    for (foreground, minimum) in row.content.into_iter().zip(floors) {
                        assert!(
                            foreground
                                .source_over(background)
                                .contrast_ratio(background)
                                >= minimum,
                            "{state:?} over {background:?}"
                        );
                    }
                    assert!(
                        row.border
                            .source_over(background)
                            .contrast_ratio(background)
                            >= 3.0
                    );
                }
            }
        }
    }

    #[test]
    fn prepared_row_state_policy_keeps_disabled_selection_identical_across_activity() {
        use crate::appearance::{
            Appearance, AppearanceGeneration, AppearanceMode, AppearancePreferences,
            AvailableFonts, CompositionCapabilities, SchemeCatalog, SystemAppearance,
        };
        use crate::ui::appearance::ChromeAppearance;
        for appearance in [Appearance::Light, Appearance::Dark] {
            for increase_contrast in [false, true] {
                let resolved = SchemeCatalog::default()
                    .resolve(
                        AppearanceGeneration::INITIAL,
                        &AppearancePreferences {
                            mode: if appearance == Appearance::Light {
                                AppearanceMode::Light
                            } else {
                                AppearanceMode::Dark
                            },
                            ..AppearancePreferences::default()
                        },
                        SystemAppearance::available(appearance).with_composition(
                            CompositionCapabilities {
                                increase_contrast,
                                ..CompositionCapabilities::new(true, true)
                            },
                        ),
                        &AvailableFonts::default(),
                    )
                    .unwrap();
                let rows = |active| {
                    let prepared = ChromeAppearance::prepare_for_activity(&resolved.chrome, active);
                    overlay_list_rows_with_policy(
                        &prepared.floating_colors,
                        &prepared.floating_colors,
                        OverlayRowPolicy::prepared(&prepared),
                    )
                };
                assert_eq!(
                    rows(true).resolve(false, true, false),
                    rows(false).resolve(false, true, true),
                    "{appearance:?}, increase_contrast={increase_contrast}"
                );
            }
        }
    }

    #[test]
    fn material_row_fill_reconstructs_the_authored_state_on_an_opaque_host() {
        let surface = Color::rgb(0x202020);
        let state = Color::rgb(0x262626);
        let actual_host = Color::rgb(0x181818);
        let fill = row_fill((state, surface), (state, actual_host));
        let rendered = fill.source_over(actual_host);

        assert_eq!(rendered, state);
        assert!(
            fill.a < 32,
            "a small elevation step needs only a thin overlay"
        );
    }

    #[test]
    fn material_row_fill_preserves_a_custom_chromatic_state_as_a_relative_overlay() {
        let surface = Color::rgb(0x202020);
        let state = Color::rgb(0x603028);
        let row = OverlayRow::resolve(
            (state, surface),
            (state, Color::rgba(0x202020b3)),
            [Color::rgb(0xffffff); 4],
            Color::rgba(0),
        );
        let fill = row.fill;

        assert!(fill.a > 0 && fill.a < 128);
        assert!(
            fill.r > fill.g && fill.r > fill.b,
            "the red authored direction must survive host-relative reconstruction: {fill:?}"
        );
    }

    #[test]
    fn dark_selected_row_lifts_from_its_semantic_host_even_when_material_rgb_is_lighter() {
        let surface = Color::rgb(0x202020);
        let selected = Color::rgb(0x262626);
        let combined_material = Color::rgba(0x363636b9);
        let fill = row_fill((selected, surface), (selected, combined_material));

        assert!(fill.a > 0 && fill.a < 32);
        assert!(
            fill.r >= 250 && fill.g >= 250 && fill.b >= 250,
            "selected must remain a lightening step over the native-backed host: {fill:?}"
        );
    }

    #[test]
    fn row_with_no_shared_readable_foreground_falls_back_once_to_opaque_state() {
        let middle = Color::rgb(0x808080);
        let row = OverlayRow::resolve(
            (middle, middle),
            (Color::rgba(0), Color::rgba(0)),
            [Color::rgb(0x777777); 4],
            Color::rgba(0),
        );

        assert_eq!(row.fill, middle);
        assert!(
            row.content
                .into_iter()
                .all(|content| content.contrast_ratio(middle) >= 4.5)
        );
    }

    #[test]
    fn nonzero_row_state_uses_fallback_when_no_nonzero_readable_alpha_exists() {
        let surface = Color::rgba(0x00000089);
        let fill = Color::rgba(0xffffff10);

        assert!(shared_neutral(row_backgrounds(fill.with_alpha(0), surface), 4.5).is_some());
        assert!(
            readable_material_row_fill(fill, surface, 4.5).is_none(),
            "a requested state must not disappear when its readable interval has zero width"
        );
    }

    /// Built-in row content stays authored when possible and remains readable across materials.
    #[test]
    fn overlay_row_content_stays_readable_across_transparency_in_both_appearances() {
        use crate::appearance::{
            Appearance, AppearancePreferences, ResolvedWindowComposition, builtin_chrome_base,
        };
        for appearance in [Appearance::Light, Appearance::Dark] {
            let reference = builtin_chrome_base(appearance).opaque_presentation();
            let rows_at = |transparency: f32| {
                let mut preferences = AppearancePreferences::default();
                preferences.background.transparency = transparency;
                let materials = ResolvedWindowComposition::resolve(
                    &preferences.background,
                    crate::appearance::CompositionCapabilities::new(true, true),
                )
                .materials;
                let paint = reference.material_presentation(materials);
                (overlay_list_rows(&reference, &paint), paint)
            };
            type RowColors = fn(&ChromeColors) -> [Color; 6];
            let states: [(bool, bool, RowColors); 3] = [
                (false, false, |c| {
                    [
                        c.elevated_surface_background,
                        c.row_foreground,
                        c.row_secondary,
                        c.row_icon,
                        c.row_match,
                        c.row_border,
                    ]
                }),
                (false, true, |c| {
                    [
                        c.row_hover_background,
                        c.row_hover_foreground,
                        c.row_hover_secondary,
                        c.row_hover_icon,
                        c.row_hover_match,
                        c.row_hover_border,
                    ]
                }),
                (true, false, |c| {
                    [
                        c.row_selected_background,
                        c.row_selected_foreground,
                        c.row_selected_secondary,
                        c.row_selected_icon,
                        c.row_selected_match,
                        c.row_selected_border,
                    ]
                }),
            ];
            let (opaque_rows, _) = rows_at(0.0);
            for transparency in [0.15, 1.0] {
                let (rows, paint) = rows_at(transparency);
                for (selected, hovered, pick) in states {
                    let [fill, foreground, secondary, icon, matched, border] = pick(&reference);
                    let opaque = OverlayRow::resolve(
                        (fill, reference.elevated_surface_background),
                        (fill, reference.elevated_surface_background),
                        [foreground, secondary, icon, matched],
                        border,
                    );
                    assert_eq!(
                        opaque_rows.resolve(true, selected, hovered),
                        opaque.paint(),
                        "{appearance:?} opaque rows keep their composite paint"
                    );
                    let expected = OverlayRow::resolve(
                        (fill, reference.elevated_surface_background),
                        (pick(&paint)[0], paint.elevated_surface_background),
                        [foreground, secondary, icon, matched],
                        border,
                    );
                    assert_eq!(
                        rows.resolve(true, selected, hovered),
                        expected.paint(),
                        "{appearance:?} at {transparency}: rows use the final material endpoints"
                    );
                    let backgrounds = if paint.elevated_surface_background.is_opaque() {
                        [expected.fill.source_over(paint.elevated_surface_background); 2]
                    } else {
                        [Color::rgb(0x000000), Color::rgb(0xffffff)].map(|underlay| {
                            expected.fill.source_over(
                                paint.elevated_surface_background.source_over(underlay),
                            )
                        })
                    };
                    for (authored, resolved) in [foreground, secondary, icon, matched]
                        .into_iter()
                        .zip(expected.content)
                    {
                        assert!(
                            backgrounds.into_iter().all(|background| resolved
                                .source_over(background)
                                .contrast_ratio(background)
                                >= 4.5),
                            "{appearance:?} at {transparency}: {resolved:?} must read over {backgrounds:?}"
                        );
                        if backgrounds.into_iter().all(|background| {
                            authored.source_over(background).contrast_ratio(background) >= 4.5
                        }) {
                            assert_eq!(
                                resolved, authored,
                                "{appearance:?} at {transparency}: readable authored content stays exact"
                            );
                        }
                    }
                    if transparency == 1.0 {
                        // The maximum setting keeps a faint fill rather than none, so a row
                        // that states a state still states it. A row that matches the surface
                        // it rests on still paints nothing at all.
                        assert!(
                            expected.fill.a < 128,
                            "{appearance:?}: a resting row must still transmit most of its backing: {:?}",
                            expected.fill
                        );
                        assert_eq!(
                            selected || hovered,
                            expected.fill.a > 0,
                            "{appearance:?}: {:?}",
                            expected.fill
                        );
                    }
                }
            }
            if appearance == Appearance::Light {
                let [_, foreground, ..] = (states[0].2)(&reference);
                let readable = readable_on(foreground, reference.elevated_surface_background, 4.5);
                assert!(
                    readable.contrast_ratio(Color::rgb(0x000000))
                        < readable.contrast_ratio(Color::rgb(0xffffff)),
                    "Light overlay text stays dark at every transparency"
                );
            }
        }
    }

    #[test]
    fn built_in_floating_row_states_remain_distinct_translucent_and_readable_at_maximum_glass() {
        use crate::appearance::{
            Appearance, AppearanceGeneration, AppearanceMode, AppearancePreferences,
            AvailableFonts, CompositionCapabilities, SchemeCatalog, SystemAppearance,
        };
        use crate::ui::appearance::ChromeAppearance;

        for appearance in [Appearance::Light, Appearance::Dark] {
            let mut preferences = AppearancePreferences {
                mode: match appearance {
                    Appearance::Light => AppearanceMode::Light,
                    Appearance::Dark => AppearanceMode::Dark,
                },
                ..AppearancePreferences::default()
            };
            preferences.background.transparency = 1.0;
            let resolved = SchemeCatalog::default()
                .resolve(
                    AppearanceGeneration::INITIAL,
                    &preferences,
                    SystemAppearance::available(appearance)
                        .with_composition(CompositionCapabilities::new(true, true)),
                    &AvailableFonts::default(),
                )
                .expect("built-in appearance should resolve");
            let prepared = ChromeAppearance::prepare(&resolved.chrome);
            let reference = &prepared.floating_colors;
            let material = prepared.floating_surface(prepared.colors.elevated_surface_background);
            let mut paint = reference.clone();
            paint.elevated_surface_background = material;
            type RowColors = fn(&ChromeColors) -> [Color; 6];
            let states: [RowColors; 3] = [
                |c| {
                    [
                        c.row_hover_background,
                        c.row_hover_foreground,
                        c.row_hover_secondary,
                        c.row_hover_icon,
                        c.row_hover_match,
                        c.row_hover_border,
                    ]
                },
                |c| {
                    [
                        c.row_selected_background,
                        c.row_selected_foreground,
                        c.row_selected_secondary,
                        c.row_selected_icon,
                        c.row_selected_match,
                        c.row_selected_border,
                    ]
                },
                |c| {
                    [
                        c.row_selected_hover_background,
                        c.row_selected_hover_foreground,
                        c.row_selected_hover_secondary,
                        c.row_selected_hover_icon,
                        c.row_selected_hover_match,
                        c.row_selected_hover_border,
                    ]
                },
            ];
            let rows = states.map(|pick| {
                let [fill, foreground, secondary, icon, matched, border] = pick(reference);
                OverlayRow::resolve(
                    (fill, reference.elevated_surface_background),
                    (pick(&paint)[0], paint.elevated_surface_background),
                    [foreground, secondary, icon, matched],
                    border,
                )
            });

            for row in rows {
                assert!(
                    row.fill.a > 0 && row.fill.a < 255,
                    "{appearance:?} row state must remain a translucent material: {:?}",
                    row.fill
                );
                for underlay in [Color::rgb(0x000000), Color::rgb(0xffffff)] {
                    let background = row.fill.source_over(material.source_over(underlay));
                    assert!(
                        row.content
                            .into_iter()
                            .all(|content| content.contrast_ratio(background) >= 4.5),
                        "{appearance:?} row content must read over {background:?}"
                    );
                }
            }
            let alphas = rows.map(|row| row.fill.a);
            assert!(
                alphas.windows(2).all(|pair| pair[0] < pair[1]),
                "{appearance:?} row states must retain their authored order: {alphas:?}"
            );
        }
    }

    #[test]
    fn overlay_rows_preserve_interaction_states_but_inherit_the_elevated_idle_surface() {
        use gpui::rgba;
        use spaceterm_ui::ListRowPaint;
        let colors = ChromeColors {
            row_background: Color::rgba(0x10203040),
            row_foreground: Color::rgba(0x14233142),
            row_secondary: Color::rgba(0x18263244),
            row_icon: Color::rgba(0x1c293346),
            row_match: Color::rgba(0x202c3448),
            row_border: Color::rgba(0x242f354a),
            row_hover_background: Color::rgba(0x2832364c),
            row_hover_foreground: Color::rgba(0x2c35374e),
            row_hover_secondary: Color::rgba(0x30383850),
            row_hover_icon: Color::rgba(0x343b3952),
            row_hover_match: Color::rgba(0x383e3a54),
            row_hover_border: Color::rgba(0x3c413b56),
            row_selected_background: Color::rgba(0x40443c58),
            row_selected_foreground: Color::rgba(0x44473d5a),
            row_selected_secondary: Color::rgba(0x484a3e5c),
            row_selected_icon: Color::rgba(0x4c4d3f5e),
            row_selected_match: Color::rgba(0x50504060),
            row_selected_border: Color::rgba(0x54534162),
            row_selected_hover_background: Color::rgba(0x58564264),
            row_selected_hover_foreground: Color::rgba(0x5c594366),
            row_selected_hover_secondary: Color::rgba(0x605c4468),
            row_selected_hover_icon: Color::rgba(0x645f456a),
            row_selected_hover_match: Color::rgba(0x6862466c),
            row_selected_hover_border: Color::rgba(0x6c65476e),
            ..ChromeColors::default()
        };
        let rows = overlay_list_rows(&colors, &colors);
        let expected = |background: Color,
                        foreground: Color,
                        secondary: Color,
                        icon: Color,
                        matched: Color,
                        border: Color| {
            let background = background.source_over(colors.elevated_surface_background);
            ListRowPaint::new(
                rgba(background.rgba_hex()),
                rgba(
                    crate::ui::appearance::readable_on_backgrounds(
                        foreground,
                        [background; 2],
                        4.5,
                    )
                    .rgba_hex(),
                ),
                rgba(
                    crate::ui::appearance::readable_on_backgrounds(secondary, [background; 2], 4.5)
                        .rgba_hex(),
                ),
                rgba(
                    crate::ui::appearance::readable_on_backgrounds(icon, [background; 2], 4.5)
                        .rgba_hex(),
                ),
                rgba(
                    crate::ui::appearance::readable_on_backgrounds(matched, [background; 2], 4.5)
                        .rgba_hex(),
                ),
                rgba(border.rgba_hex()),
            )
        };
        assert_eq!(
            rows.resolve(true, false, false),
            expected(
                colors.elevated_surface_background,
                colors.row_foreground,
                colors.row_secondary,
                colors.row_icon,
                colors.row_match,
                colors.row_border,
            )
        );
        assert_eq!(
            rows.resolve(true, false, true),
            expected(
                colors.row_hover_background,
                colors.row_hover_foreground,
                colors.row_hover_secondary,
                colors.row_hover_icon,
                colors.row_hover_match,
                colors.row_hover_border,
            )
        );
        assert_eq!(
            rows.resolve(true, true, false),
            expected(
                colors.row_selected_background,
                colors.row_selected_foreground,
                colors.row_selected_secondary,
                colors.row_selected_icon,
                colors.row_selected_match,
                colors.row_selected_border,
            )
        );
        assert_eq!(
            rows.resolve(true, true, true),
            expected(
                colors.row_selected_hover_background,
                colors.row_selected_hover_foreground,
                colors.row_selected_hover_secondary,
                colors.row_selected_hover_icon,
                colors.row_selected_hover_match,
                colors.row_selected_hover_border,
            )
        );
        assert_eq!(
            rows.resolve(false, true, true),
            expected(
                colors.row_selected_background,
                colors.text_disabled,
                colors.text_disabled,
                colors.icon_disabled,
                colors.text_disabled,
                colors.row_selected_border,
            )
        );
    }

    #[test]
    fn list_themes_should_consume_hover_and_selection_independently() {
        let base = ChromeColors::default();
        let hovered = ChromeColors {
            row_hover_background: Color::rgb(0x123456),
            ..base.clone()
        };
        let selected = ChromeColors {
            row_selected_background: Color::rgb(0xabcdef),
            ..base.clone()
        };
        let hover_foreground = ChromeColors {
            row_hover_foreground: Color::rgb(0x123456),
            ..base.clone()
        };
        let selected_foreground = ChromeColors {
            row_selected_foreground: Color::rgb(0xabcdef),
            ..base.clone()
        };
        for changed in [hovered, selected, hover_foreground, selected_foreground] {
            assert_ne!(menu_theme::theme(&base), menu_theme::theme(&changed));
            assert_ne!(
                combo_box_theme::theme(&base),
                combo_box_theme::theme(&changed)
            );
            assert_ne!(
                command_palette_theme::theme(&base),
                command_palette_theme::theme(&changed)
            );
        }
    }

    #[test]
    fn overlay_controls_keep_idle_rows_on_their_raised_panel() {
        let base = ChromeColors::default().opaque_presentation();
        let changed_shell_row = ChromeColors {
            row_background: Color::rgb(0xff00ff),
            ..base.clone()
        };
        let changed_overlay = ChromeColors {
            elevated_surface_background: Color::rgb(0x004488),
            ..base.clone()
        };

        assert_eq!(
            menu_theme::theme(&base),
            menu_theme::theme(&changed_shell_row)
        );
        assert_eq!(
            combo_box_theme::theme(&base),
            combo_box_theme::theme(&changed_shell_row)
        );
        assert_eq!(
            command_palette_theme::theme(&base),
            command_palette_theme::theme(&changed_shell_row)
        );
        assert_ne!(
            menu_theme::theme(&base),
            menu_theme::theme(&changed_overlay)
        );
        assert_ne!(
            combo_box_theme::theme(&base),
            combo_box_theme::theme(&changed_overlay)
        );
        assert_ne!(
            command_palette_theme::theme(&base),
            command_palette_theme::theme(&changed_overlay)
        );
    }
}
