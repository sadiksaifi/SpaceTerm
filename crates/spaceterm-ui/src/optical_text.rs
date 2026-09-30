//! Single-line text laid out by its ink rather than its advance.
use gpui::{
    App, Bounds, Element, ElementId, GlobalElementId, Hsla, InspectorElementId, IntoElement,
    LayoutId, Pixels, ShapedLine, SharedString, Style, TextAlign, Window, point, px,
};

/// Shows one line of text in the inherited text style.
///
/// The box spans the glyphs' ink, without the outer side bearings, so spacing around the text
/// measures to what the reader sees. Every glyph keeps its shaped advance.
pub(crate) struct OpticalText {
    text: SharedString,
    centers_fallback_glyphs: bool,
}

impl OpticalText {
    pub(crate) fn new(text: impl Into<SharedString>) -> Self {
        Self {
            text: text.into(),
            centers_fallback_glyphs: false,
        }
    }

    /// Paints each glyph the font lacks so its ink center meets the font's cap-height center.
    ///
    /// A fallback font has its own vertical design, such as Return from Lucida Grande beside
    /// Command from SF. Glyphs the font provides keep their designed position, so a raised Control
    /// caret stays raised.
    pub(crate) fn centering_fallback_glyphs(mut self) -> Self {
        self.centers_fallback_glyphs = true;
        self
    }
}

impl IntoElement for OpticalText {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

/// The shaped text and the vertical offset of each glyph, in run and glyph order.
pub(crate) struct OpticalLayout {
    line: ShapedLine,
    line_height: Pixels,
    color: Hsla,
    offsets: Vec<Pixels>,
    /// The first glyph's leading side bearing, which the box omits.
    ink_left: Pixels,
}

impl Element for OpticalText {
    type RequestLayoutState = OpticalLayout;
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let style = window.text_style();
        let font_size = style.font_size.to_pixels(window.rem_size());
        let line_height = style.line_height_in_pixels(window.rem_size());
        let text_system = window.text_system();
        let font_id = text_system.resolve_font(&style.font());
        let cap_center = text_system.cap_height(font_id, font_size) / 2.0;
        let line = text_system.shape_line(
            self.text.clone(),
            font_size,
            &[style.to_run(self.text.len())],
            None,
        );
        let ink = |glyph_font_id, glyph: &gpui::ShapedGlyph| {
            let character = self.text[glyph.index..].chars().next()?;
            text_system
                .typographic_bounds(glyph_font_id, font_size, character)
                .ok()
        };
        let glyphs = || {
            line.runs
                .iter()
                .flat_map(|run| run.glyphs.iter().map(move |glyph| (run.font_id, glyph)))
        };
        let offsets = glyphs()
            .map(|(glyph_font_id, glyph)| {
                if !self.centers_fallback_glyphs || glyph_font_id == font_id || glyph.is_emoji {
                    return px(0.0);
                }
                // Typographic bounds rise from the baseline, so a positive offset moves ink down.
                ink(glyph_font_id, glyph).map_or(px(0.0), |ink| {
                    ink.origin.y + ink.size.height / 2.0 - cap_center
                })
            })
            .collect();
        let ink_left = glyphs()
            .next()
            .and_then(|(glyph_font_id, glyph)| {
                ink(glyph_font_id, glyph).map(|ink| glyph.position.x + ink.origin.x)
            })
            .unwrap_or(px(0.0));
        let ink_right = glyphs()
            .last()
            .and_then(|(glyph_font_id, glyph)| {
                ink(glyph_font_id, glyph).map(|ink| glyph.position.x + ink.right())
            })
            .unwrap_or(line.width);
        let mut layout_style = Style::default();
        layout_style.size.width = (ink_right - ink_left).max(px(0.0)).into();
        layout_style.size.height = line_height.into();
        layout_style.flex_shrink = 0.0;
        let layout_id = window.request_layout(layout_style, [], cx);
        (
            layout_id,
            OpticalLayout {
                line,
                line_height,
                color: style.color,
                offsets,
                ink_left,
            },
        )
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        _: &mut Self::RequestLayoutState,
        _: &mut Window,
        _: &mut App,
    ) {
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        layout: &mut Self::RequestLayoutState,
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        let origin = bounds.origin - point(layout.ink_left, px(0.0));
        if layout.offsets.iter().all(|offset| *offset == px(0.0)) {
            let _ = layout.line.paint(
                origin,
                layout.line_height,
                TextAlign::Left,
                None,
                window,
                cx,
            );
            return;
        }
        let line = &layout.line;
        let baseline = (layout.line_height - line.ascent - line.descent) / 2.0 + line.ascent;
        let glyphs = line
            .runs
            .iter()
            .flat_map(|run| run.glyphs.iter().map(move |glyph| (run.font_id, glyph)));
        for ((font_id, glyph), offset) in glyphs.zip(&layout.offsets) {
            let origin = origin + point(glyph.position.x, baseline + glyph.position.y + *offset);
            let _ = if glyph.is_emoji {
                window.paint_emoji(origin, font_id, glyph.id, line.font_size)
            } else {
                window.paint_glyph(origin, font_id, glyph.id, line.font_size, layout.color)
            };
        }
    }
}
