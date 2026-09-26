//! The reusable search field: a standard leading search glyph, one editor, and a clear action.
use gpui::prelude::*;
use gpui::{App, ElementId, Entity, Pixels, Rgba, SharedString, Window, div, px, rgba};

use crate::{
    ButtonSize, ButtonVariantStyle, FieldFrameTheme, FieldState, Icon, IconButton, IconName,
    TextInput,
};

/// The logical accessibility name of the trailing clear action.
const CLEAR_ACCESSIBILITY_NAME: &str = "Clear search";

/// Application-owned colors for the parts a search field adds to its editor.
///
/// The editor keeps its own text, placeholder, selection, and caret paints, and the surrounding
/// surface keeps the shared field frame, so this covers only the leading glyph and the trailing
/// clear action.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SearchFieldPaint {
    icon: Rgba,
    clear: ButtonVariantStyle,
    clear_glyph: Rgba,
}

impl SearchFieldPaint {
    /// Creates the complete bounded search-field paint catalog.
    ///
    /// The clear mark is a filled disc struck through by a glyph, and the pointer target around it
    /// stays invisible, so `clear` supplies the disc fill for every interactive state through each
    /// state's icon foreground rather than a target surface. `clear_glyph` is the contrasting glyph
    /// struck through that disc.
    pub fn new(icon: Rgba, clear: ButtonVariantStyle, clear_glyph: Rgba) -> Self {
        Self {
            icon,
            clear,
            clear_glyph,
        }
    }
}

/// Bounded dimensions shared by every search field.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SearchFieldMetrics {
    height: Pixels,
    horizontal_padding: Pixels,
    gap: Pixels,
    corner_radius: Pixels,
    label_size: Pixels,
    line_height: Pixels,
    icon_size: Pixels,
    icon_baseline_center: Pixels,
    clear_mark_size: Pixels,
    clear_glyph_size: Pixels,
    clear_target_size: Pixels,
    clear_trailing_inset: Pixels,
}

impl SearchFieldMetrics {
    /// Creates compact native defaults around the field height.
    pub fn new(height: Pixels) -> Self {
        Self {
            height,
            horizontal_padding: px(8.0),
            gap: px(7.0),
            corner_radius: px(6.0),
            label_size: px(12.0),
            line_height: px(16.0),
            icon_size: px(13.0),
            icon_baseline_center: px(4.0),
            clear_mark_size: height / 2.0,
            clear_glyph_size: px(8.0),
            clear_target_size: px(20.0),
            clear_trailing_inset: px(4.0),
        }
    }

    /// Sets content padding, the gap between the glyph, the editor, and the clear action, and the
    /// field's corner radius.
    pub fn spacing(
        mut self,
        horizontal_padding: Pixels,
        gap: Pixels,
        corner_radius: Pixels,
    ) -> Self {
        self.horizontal_padding = horizontal_padding;
        self.gap = gap;
        self.corner_radius = corner_radius;
        self
    }

    /// Sets the editor's text size and line box and the leading glyph size.
    pub fn text_geometry(
        mut self,
        label_size: Pixels,
        line_height: Pixels,
        icon_size: Pixels,
    ) -> Self {
        self.label_size = label_size;
        self.line_height = line_height;
        self.icon_size = icon_size;
        self
    }

    /// Sets the center-above-baseline metric for the search glyph paired with the editor text.
    pub fn icon_baseline_center(mut self, center: Pixels) -> Self {
        self.icon_baseline_center = center;
        self
    }

    /// Sets the visible clear mark: the disc diameter, the glyph struck through it, and the space
    /// the field keeps between its own trailing edge and the mark's pointer target.
    ///
    /// The mark stays smaller than the pointer target it is centered in, so the field keeps a
    /// comfortable target for a small affordance. The trailing inset therefore replaces the
    /// field's own trailing padding while the mark is present: the target already carries the air
    /// around the mark, and counting both would push the mark away from the edge it belongs to.
    pub fn clear_mark(
        mut self,
        diameter: Pixels,
        glyph_size: Pixels,
        target_size: Pixels,
        trailing_inset: Pixels,
    ) -> Self {
        self.clear_mark_size = diameter;
        self.clear_glyph_size = glyph_size;
        self.clear_target_size = target_size;
        self.clear_trailing_inset = trailing_inset;
        self
    }

    fn scaled(self, text_scale: f32, spacing_scale: f32) -> Self {
        Self {
            height: crate::appearance::scale_line_box(
                self.height,
                self.line_height,
                text_scale,
                spacing_scale,
            ),
            horizontal_padding: crate::appearance::scale_metric(
                self.horizontal_padding,
                spacing_scale,
            ),
            gap: crate::appearance::scale_metric(self.gap, spacing_scale),
            corner_radius: self.corner_radius,
            label_size: crate::appearance::scale_metric(self.label_size, text_scale),
            line_height: crate::appearance::scale_metric(self.line_height, text_scale),
            icon_size: crate::appearance::scale_metric(self.icon_size, text_scale),
            icon_baseline_center: crate::appearance::scale_metric(
                self.icon_baseline_center,
                text_scale,
            ),
            clear_mark_size: crate::appearance::scale_metric(self.clear_mark_size, text_scale),
            clear_glyph_size: crate::appearance::scale_metric(self.clear_glyph_size, text_scale),
            clear_target_size: self.clear_target_size,
            clear_trailing_inset: crate::appearance::scale_metric(
                self.clear_trailing_inset,
                spacing_scale,
            ),
        }
    }
}

/// Application-installed presentation for every [`SearchField`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SearchFieldTheme {
    frame: FieldFrameTheme,
    paint: SearchFieldPaint,
    metrics: SearchFieldMetrics,
}

impl SearchFieldTheme {
    /// Creates a complete search-field theme from the application-owned field surface, the
    /// search-specific colors, and bounded metrics.
    pub fn new(
        frame: FieldFrameTheme,
        paint: SearchFieldPaint,
        metrics: SearchFieldMetrics,
    ) -> Self {
        Self {
            frame,
            paint,
            metrics,
        }
    }

    pub(crate) fn focus_ring_width(mut self, width: Pixels) -> Self {
        self.frame = self.frame.focus_ring_width(width);
        self
    }

    pub(crate) fn scaled_metrics(self, text_scale: f32, spacing_scale: f32) -> Self {
        Self {
            metrics: self.metrics.scaled(text_scale, spacing_scale),
            ..self
        }
    }
}

impl gpui::Global for SearchFieldTheme {}

/// A standard search field: a leading search glyph, one editor, and a trailing clear action.
///
/// The field owns presentation and the clear interaction. Its consumer owns the query and every
/// search semantic: it creates and retains the [`TextInput`], subscribes to
/// [`TextInputEvent::ValueChanged`](crate::TextInputEvent::ValueChanged), and updates results from
/// the editor's current value as that value changes. The field never reports query contents of its
/// own, so an editor built with
/// [`emit_programmatic_changes(true)`](TextInput::emit_programmatic_changes) reports the clear
/// through the same event as typing, and one subscription answers both.
///
/// The trailing action appears only while the value is nonempty, clears the editor through its
/// public Interface, and returns keyboard focus to it, so clearing leaves the reader where they
/// were typing. Configure the editor itself for search: a
/// [`Bare`](crate::TextInputVariant::Bare) variant, because the field paints the surrounding
/// surface, and a placeholder.
#[derive(IntoElement)]
pub struct SearchField {
    id: ElementId,
    input: Entity<TextInput>,
    frame_selector: Option<SharedString>,
    clear_selector: Option<SharedString>,
}

impl SearchField {
    /// Creates a search field over a caller-owned editor.
    pub fn new(id: impl Into<ElementId>, input: Entity<TextInput>) -> Self {
        Self {
            id: id.into(),
            input,
            frame_selector: None,
            clear_selector: None,
        }
    }

    /// Overrides the stable selectors used by GPUI interaction tests for the field surface and its
    /// clear action, which otherwise derive from the field's own identifier.
    pub fn debug_selectors(
        mut self,
        frame: impl Into<SharedString>,
        clear: impl Into<SharedString>,
    ) -> Self {
        self.frame_selector = Some(frame.into());
        self.clear_selector = Some(clear.into());
        self
    }
}

impl RenderOnce for SearchField {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = *crate::floating_surface::hosted_search_field_theme(cx);
        let metrics = theme.metrics;
        let icon_offset = crate::icon::text_alignment_offset(
            crate::control_typography(cx).regular(),
            metrics.label_size,
            metrics.line_height,
            metrics.icon_baseline_center,
            window,
        );
        let editor = self.input.read(cx);
        let focus = editor.focus_handle();
        let empty = editor.value().is_empty();
        let frame_selector = self
            .frame_selector
            .unwrap_or_else(|| SharedString::from(self.id.to_string()));
        let clear_selector = self
            .clear_selector
            .unwrap_or_else(|| SharedString::from(format!("{}-clear", self.id)));
        let clear_id = ElementId::NamedChild(std::sync::Arc::new(self.id.clone()), "clear".into());
        let input = self.input.clone();
        let clear_focus = focus.clone();
        crate::field_frame::themed_field_frame(theme.frame, self.id, &focus, FieldState::default())
            .debug_selector(move || frame_selector.to_string())
            .flex()
            .flex_row()
            .items_center()
            .w_full()
            .h(metrics.height)
            .pl(metrics.horizontal_padding)
            .pr(if empty {
                metrics.horizontal_padding
            } else {
                metrics.clear_trailing_inset
            })
            .gap(metrics.gap)
            .rounded(metrics.corner_radius)
            .text_size(metrics.label_size)
            .line_height(metrics.line_height)
            .child(
                div()
                    .flex_none()
                    .relative()
                    .top(icon_offset)
                    .child(Icon::new(
                        IconName::Search,
                        metrics.icon_size,
                        theme.paint.icon,
                    )),
            )
            .child(div().min_w_0().flex_1().child(self.input))
            .when(!empty, |field| {
                let glyph = theme.paint.clear_glyph;
                field.child(
                    IconButton::new(clear_id, CLEAR_ACCESSIBILITY_NAME, move |fill| {
                        clear_mark(metrics, fill, glyph)
                    })
                    // Compact keeps the target comfortably larger than the mark it centers.
                    .size(ButtonSize::Compact)
                    .target_size(metrics.clear_target_size)
                    // The target itself stays invisible and carries only the pointer interaction,
                    // so the theme paints every state into the mark inside it and the mark never
                    // takes a ring of its own.
                    .contextual_style(theme.paint.clear, rgba(0))
                    // Clearing belongs to the editor the mark sits in. Keyboard readers reach it
                    // through the host's own dismissal policy rather than a second traversal stop.
                    .tab_stop(false)
                    .debug_selector(clear_selector.to_string())
                    .on_activate(move |_, window, cx| {
                        input.update(cx, |input, cx| {
                            input.clear(cx);
                        });
                        clear_focus.focus(window, cx);
                    }),
                )
            })
    }
}

/// Paints the clear mark centered inside its invisible pointer target.
fn clear_mark(metrics: SearchFieldMetrics, fill: Rgba, glyph: Rgba) -> gpui::AnyElement {
    div()
        .w(metrics.clear_mark_size)
        .h(metrics.clear_mark_size)
        .flex()
        .items_center()
        .justify_center()
        // A quad clamps its radii to half its own shorter side, so one radius at the diameter
        // keeps the mark a disc at every metric scale.
        .rounded(metrics.clear_mark_size)
        .bg(fill)
        .child(Icon::new(IconName::X, metrics.clear_glyph_size, glyph))
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn density_scales_field_bounds_but_not_radius() {
        let metrics = SearchFieldMetrics::new(px(28.0)).spacing(px(8.0), px(7.0), px(6.0));
        let comfortable = metrics.scaled(1.0, 1.25);

        assert!(comfortable.height > metrics.height);
        assert_eq!(comfortable.corner_radius, metrics.corner_radius);
    }

    #[test]
    fn clear_mark_target_is_semantic_and_does_not_scale_twice() {
        let metrics =
            SearchFieldMetrics::new(px(28.0)).clear_mark(px(13.0), px(8.0), px(28.0), px(2.0));

        assert_eq!(metrics.scaled(1.5, 1.25).clear_target_size, px(28.0));
    }
}
