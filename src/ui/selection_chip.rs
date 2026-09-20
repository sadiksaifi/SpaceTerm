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
use crate::ui::chrome_geometry::HAIRLINE;

/// Where a chip sits inside the item that owns it.
///
/// The item keeps its own bounds, so hit target, hover region, and keyboard target are unchanged
/// by the inset: only the paint moves inward. The two horizontal insets are separate because a
/// strip can meet a window edge on one side and a floating surface on the other, where equal
/// insets would read as unequal air.
#[derive(Clone, Copy)]
pub(crate) struct ChipShape {
    pub(crate) inset_leading: Pixels,
    pub(crate) inset_trailing: Pixels,
    pub(crate) inset_y: Pixels,
    pub(crate) radius: Pixels,
}

impl ChipShape {
    /// A chip with the same air on both sides, for a strip whose neighbours match.
    pub(crate) const fn symmetric(inset_x: Pixels, inset_y: Pixels, radius: Pixels) -> Self {
        Self {
            inset_leading: inset_x,
            inset_trailing: inset_x,
            inset_y,
            radius,
        }
    }
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

impl ChipPaint {
    /// Ties a chip to a translucent window without giving up what it describes.
    ///
    /// Each fill is relative to its host. Custom rims are relative to their state fill, so their
    /// contrast direction survives changes in the native backing.
    pub(crate) fn raised_on(
        self,
        appearance: &crate::ui::appearance::ChromeAppearance,
        semantic_host: Color,
    ) -> Self {
        let material = |color: Option<Color>| {
            color.map(|color| {
                appearance.materials.paint(
                    crate::appearance::SurfaceRole::Surface,
                    semantic_host,
                    color,
                )
            })
        };
        let edge = |fill: Option<Color>, color: Option<Color>| {
            let host = fill.map_or(semantic_host, |fill| fill.source_over(semantic_host));
            color.map(|color| appearance.materials.edge(host, color))
        };
        Self {
            fill: material(self.fill),
            hover_fill: material(self.hover_fill),
            rim: edge(self.fill, self.rim),
            hover_rim: edge(self.hover_fill.or(self.fill), self.hover_rim),
        }
    }
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

    fn body(shape: ChipShape) -> gpui::Div {
        div()
            .absolute()
            .top(shape.inset_y)
            .bottom(shape.inset_y)
            .left(shape.inset_leading)
            .right(shape.inset_trailing)
            .rounded(shape.radius)
    }

    /// The chip itself, painted under the item's own content.
    ///
    /// Hover is carried by the owning item's group rather than by the chip, so pointing anywhere in
    /// the item lights the one shape that item presents.
    pub(crate) fn render(self, selector: String, group: &str) -> AnyElement {
        let hover_fill = self.paint.hover_fill;
        // The chip has no content, so omitting transparent rims does not alter layout.
        let hover_rim = self.paint.hover_rim.filter(|rim| rim.a != 0);
        let rim = self.paint.rim.filter(|rim| rim.a != 0);
        Self::body(self.shape)
            .debug_selector(move || selector.clone())
            .when_some(self.paint.fill, |chip, fill| {
                chip.bg(rgba(fill.rgba_hex())).when_some(rim, |chip, rim| {
                    chip.border(px(HAIRLINE)).border_color(rgba(rim.rgba_hex()))
                })
            })
            .group_hover(group.to_owned(), move |style| {
                let style = match hover_fill {
                    Some(fill) => style.bg(rgba(fill.rgba_hex())),
                    None => style,
                };
                match hover_rim {
                    Some(rim) => style
                        .border(px(HAIRLINE))
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
        Self::body(ChipShape {
            inset_leading: self.shape.inset_leading - gap,
            inset_trailing: self.shape.inset_trailing - gap,
            inset_y: self.shape.inset_y - gap,
            radius: self.shape.radius + gap,
        })
        .border(px(HAIRLINE))
        .border_color(rgba(color.rgba_hex()))
        .debug_selector(move || selector.to_owned())
        .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::appearance::{
        AppearanceGeneration, AppearancePreferences, AvailableFonts, CompositionCapabilities,
        SchemeCatalog, SystemAppearance,
    };

    fn appearance(transparency: f32) -> crate::ui::appearance::ChromeAppearance {
        let mut preferences = AppearancePreferences::default();
        preferences.background.transparency = transparency;
        let resolved = SchemeCatalog::default()
            .resolve(
                AppearanceGeneration::INITIAL,
                &preferences,
                SystemAppearance::unavailable()
                    .with_composition(CompositionCapabilities::new(true, true)),
                &AvailableFonts::default(),
            )
            .expect("built-in appearance should resolve");
        crate::ui::appearance::ChromeAppearance::prepare(&resolved.chrome)
    }

    fn selected(colors: &crate::appearance::ChromeColors) -> ChipPaint {
        ChipPaint {
            fill: Some(colors.row_selected_background),
            rim: Some(colors.row_selected_border),
            hover_fill: Some(colors.row_selected_hover_background),
            hover_rim: Some(colors.row_selected_hover_border),
        }
    }

    #[test]
    fn raised_chip_materializes_each_state_against_its_actual_host() {
        let appearance = appearance(1.0);
        let colors = &appearance.colors;
        let host = colors.panel_background;
        let paint = selected(colors).raised_on(&appearance, host);
        let expected_fill = appearance.materials.paint(
            crate::appearance::SurfaceRole::Surface,
            host,
            colors.row_selected_background,
        );
        let expected_hover = appearance.materials.paint(
            crate::appearance::SurfaceRole::Surface,
            host,
            colors.row_selected_hover_background,
        );

        assert_eq!(paint.fill, Some(expected_fill));
        assert_eq!(paint.hover_fill, Some(expected_hover));
        assert_ne!(paint.fill, paint.hover_fill);
    }

    #[test]
    fn opaque_chip_keeps_its_authored_states() {
        let appearance = appearance(0.0);
        let colors = &appearance.colors;
        let authored = selected(colors);
        let paint = authored.raised_on(&appearance, colors.panel_background);

        assert_eq!(paint.fill, authored.fill);
        assert_eq!(paint.hover_fill, authored.hover_fill);
    }

    #[test]
    fn custom_chip_edges_follow_each_state_and_keep_opaque_authored_paints() {
        let authored = ChipPaint {
            fill: Some(Color::rgb(0x303030)),
            rim: Some(Color::rgba(0x48484880)),
            hover_fill: Some(Color::rgb(0x404040)),
            hover_rim: Some(Color::rgba(0x606060a0)),
        };
        let opaque = appearance(0.0);
        let glass = appearance(1.0);
        let host = Color::rgb(0x202020);
        let opaque_paint = authored.raised_on(&opaque, host);
        assert_eq!(
            (opaque_paint.rim, opaque_paint.hover_rim),
            (authored.rim, authored.hover_rim)
        );

        let paint = authored.raised_on(&glass, host);
        for (fill, edge, actual) in [
            (
                authored.fill.unwrap(),
                authored.rim.unwrap(),
                paint.rim.unwrap(),
            ),
            (
                authored.hover_fill.unwrap(),
                authored.hover_rim.unwrap(),
                paint.hover_rim.unwrap(),
            ),
        ] {
            assert!(actual.a < edge.a);
            let actual = actual.source_over(fill);
            let expected = edge.source_over(fill);
            assert!(actual.r.abs_diff(expected.r) <= 1);
            assert!(actual.g.abs_diff(expected.g) <= 1);
            assert!(actual.b.abs_diff(expected.b) <= 1);
        }
    }
}
