//! The inset rounded chip SpaceTerm paints behind a hovered or selected navigation item.
//!
//! A navigation item that fills its strip edge to edge reads as a band laid over the surface, and
//! a band has no front or back. Pulling the paint into a chip with a small inset and a modest
//! radius gives the current item a shape of its own, and gives hover somewhere to land that does
//! not contradict it.
//!
//! The Workspace sidebar, the Tab bar, and the Settings navigation all carry the same idea, so the
//! geometry, the resting and hovered paints, and the keyboard focus ring live here rather than
//! three times over. Each surface still chooses its own metrics and its own roles: what this
//! Module owns is the shape those choices are expressed in.

use gpui::prelude::*;
use gpui::{AnyElement, Pixels, div, px, rgba};

use crate::appearance::Color;

/// The hairline every chip and focus ring is drawn with.
const CHIP_HAIRLINE: f32 = 1.0;

/// The corner radius of every selection chip, before density scaling.
///
/// The Workspace sidebar, the Settings navigation, and the Tab bar present one selection shape, so
/// the radius has one owner rather than three constants that merely happen to agree.
pub(crate) const CHIP_RADIUS: f32 = 6.0;

/// Where a chip sits inside the item that owns it.
///
/// The item keeps its own bounds, so hit target, hover region, and keyboard target are unchanged
/// by the inset: only the paint moves inward.
#[derive(Clone, Copy)]
pub(crate) struct ChipShape {
    pub(crate) inset_x: Pixels,
    pub(crate) inset_y: Pixels,
    pub(crate) radius: Pixels,
}

/// What a chip paints at rest and under the pointer.
///
/// Every paint is optional. A rim earns its place only where the fill alone is too close to the
/// surface under it to describe an edge, and an item that cannot be chosen leaves the hover paints
/// out entirely rather than lighting under a pointer that can do nothing with it.
#[derive(Clone, Copy)]
pub(crate) struct ChipPaint {
    pub(crate) fill: Option<Color>,
    pub(crate) rim: Option<Color>,
    pub(crate) hover_fill: Option<Color>,
    pub(crate) hover_rim: Option<Color>,
}

#[derive(Clone, Copy)]
pub(crate) struct SelectionChip {
    shape: ChipShape,
    paint: ChipPaint,
}

impl SelectionChip {
    pub(crate) fn new(shape: ChipShape, paint: ChipPaint) -> Self {
        Self { shape, paint }
    }

    fn body(inset_x: Pixels, inset_y: Pixels, radius: Pixels) -> gpui::Div {
        div()
            .absolute()
            .top(inset_y)
            .bottom(inset_y)
            .left(inset_x)
            .right(inset_x)
            .rounded(radius)
    }

    /// The chip itself, painted under the item's own content.
    ///
    /// Hover is carried by the owning item's group rather than by the chip, so pointing anywhere in
    /// the item lights the one shape that item presents.
    pub(crate) fn render(self, selector: String, group: &str) -> AnyElement {
        let hover_fill = self.paint.hover_fill;
        let hover_rim = self.paint.hover_rim;
        Self::body(self.shape.inset_x, self.shape.inset_y, self.shape.radius)
            .debug_selector(move || selector.clone())
            .when_some(self.paint.fill, |chip, fill| {
                chip.bg(rgba(fill.rgba_hex()))
                    .when_some(self.paint.rim, |chip, rim| {
                        // A rim is authored to sit darker than its fill in light Chrome and lighter
                        // in dark, so one role describes a lit edge in both rather than an outline
                        // drawn around a box.
                        chip.border(px(CHIP_HAIRLINE))
                            .border_color(rgba(rim.rgba_hex()))
                    })
            })
            .group_hover(group.to_owned(), move |style| {
                let style = match hover_fill {
                    Some(fill) => style.bg(rgba(fill.rgba_hex())),
                    None => style,
                };
                match hover_rim {
                    Some(rim) => style
                        .border(px(CHIP_HAIRLINE))
                        .border_color(rgba(rim.rgba_hex())),
                    None => style,
                }
            })
            .into_any_element()
    }

    /// Keyboard focus, riding just outside the chip it belongs to.
    ///
    /// A ring drawn on the item's own edges would box the whole strip and say nothing about which
    /// shape the keyboard is pointing at. The gap is the hairline of surface left visible between
    /// the two, so the ring reads as something around the chip rather than as a thicker chip.
    pub(crate) fn ring(self, gap: Pixels, color: Color, selector: &'static str) -> AnyElement {
        Self::body(
            self.shape.inset_x - gap,
            self.shape.inset_y - gap,
            self.shape.radius + gap,
        )
        .border(px(CHIP_HAIRLINE))
        .border_color(rgba(color.rgba_hex()))
        .debug_selector(move || selector.to_owned())
        .into_any_element()
    }
}
