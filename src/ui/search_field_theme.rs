use gpui::{Rgba, px, rgba};
use spaceterm_ui::{
    ButtonPaint, ButtonVariantStyle, FieldFrameTheme, SearchFieldMetrics, SearchFieldPaint,
    SearchFieldTheme,
};

use crate::appearance::{ChromeColors, Color};
use crate::ui::chrome_geometry::RadiusRole;
use crate::ui::chrome_icons::{ChromeIcons, IconRole};
use crate::ui::chrome_typography::{ChromeTypography, TextRole};

/// The height of one search field, matching the navigation entries a sidebar field sits above so
/// the column runs on one rhythm from its first row to its last.
const FIELD_HEIGHT: f32 = 28.0;
/// The ordinary control radius, which the sidebar chip this field shares its column with also
/// takes. It is named through the shared scale so the two cannot drift apart by one point.
const FIELD_RADIUS: f32 = RadiusRole::Control.points();
/// The clear mark: a compact disc struck through by a glyph that keeps a ring of fill around it
/// while staying heavy enough to read at a glance. The pointer target around the mark stays larger
/// and invisible, so a small affordance is still comfortable to hit, and the inset is measured to
/// that target rather than to the mark, which sits nearer the field's trailing edge than the
/// leading glyph sits to its own.
const CLEAR_MARK_SIZE: f32 = 12.0;
const CLEAR_GLYPH_SIZE: f32 = 7.0;
const CLEAR_TRAILING_INSET: f32 = 2.0;

pub(super) fn prepared(
    reference: &ChromeColors,
    colors: &ChromeColors,
    typography: &ChromeTypography,
    icons: &ChromeIcons,
) -> SearchFieldTheme {
    let body = typography.style(TextRole::Body);
    SearchFieldTheme::new(
        FieldFrameTheme::new(
            gpui_color(colors.input_background),
            gpui_color(colors.input_border),
            gpui_color(colors.input_focused_border),
            gpui_color(colors.input_invalid_border),
            gpui_color(colors.input_disabled_background),
            gpui_color(colors.input_disabled_border),
        )
        .focus_ring(gpui_color(colors.focus_ring))
        .corner_radius(px(FIELD_RADIUS)),
        SearchFieldPaint::new(
            // The glyph reads as part of the prompt the placeholder states, not as a control.
            gpui_color(colors.input_placeholder),
            clear_mark_fills(colors),
            gpui_color(clear_glyph(reference)),
        ),
        SearchFieldMetrics::new(px(FIELD_HEIGHT))
            .spacing(px(8.0), px(7.0), px(FIELD_RADIUS))
            .text_geometry(
                body.size,
                body.line_height,
                icons.metrics(IconRole::Row).glyph_size,
            )
            .icon_baseline_center(icons.metrics(IconRole::Row).baseline_center)
            .clear_mark(
                px(CLEAR_MARK_SIZE),
                px(CLEAR_GLYPH_SIZE),
                px(CLEAR_TRAILING_INSET),
            ),
    )
}

/// The disc fill for every state of the clear mark.
///
/// The mark rests at the neutral weight the placeholder and the search glyph already carry, so it
/// reads as part of the field rather than as a control competing with the value beside it. Pointing
/// at it and pressing it darken the disc toward the value's own weight. Nothing else paints: the
/// target around the mark is transparent in every state.
fn clear_mark_fills(colors: &ChromeColors) -> ButtonVariantStyle {
    let fill = |color: Color| ButtonPaint::new(rgba(0), gpui_color(color), rgba(0));
    ButtonVariantStyle::new(
        fill(colors.input_placeholder),
        fill(colors.input_placeholder.mix(colors.input_text, 0.5)),
        fill(colors.input_text),
        fill(colors.input_disabled_text),
    )
}

/// The glyph struck through the clear mark.
///
/// It reads as the field showing through the disc rather than as ink on it, so it starts from the
/// field's own fill and is then resolved to stay legible on the disc itself. Resolving against the
/// opaque presentation keeps one readable glyph while a translucent window moves the fills under
/// it, and the disc's own states only ever move further from the field's fill, so the glyph that
/// answers the resting disc answers the pointed-at and pressed ones too.
fn clear_glyph(reference: &ChromeColors) -> Color {
    let disc = reference.input_placeholder.source_over(
        reference
            .input_background
            .source_over(reference.panel_background),
    );
    super::control_theme_catalog::readable_on(reference.input_background, disc, 4.5)
}

fn gpui_color(color: Color) -> Rgba {
    rgba(color.rgba_hex())
}
