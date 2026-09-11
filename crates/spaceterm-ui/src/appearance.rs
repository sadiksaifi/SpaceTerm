use gpui::{BoxShadow, Font, FontWeight, Hsla, Pixels, font, hsla, point, px};

const SYSTEM_UI_FONT_FAMILY: &str = ".SystemUIFont";

/// The resolved fonts used by reusable chrome controls.
///
/// Sizes and line heights remain semantic metrics of each control family. The three fonts carry
/// the complete effective family, fallback, feature, weight, and style choices used for shaping.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ControlTypography {
    regular: Font,
    emphasis: Font,
    heading: Font,
}

impl ControlTypography {
    /// Creates a complete resolved typography catalog.
    pub fn new(regular: Font, emphasis: Font, heading: Font) -> Self {
        Self {
            regular,
            emphasis,
            heading,
        }
    }

    /// Returns the font used for ordinary labels, values, and editable text.
    pub fn regular(&self) -> &Font {
        &self.regular
    }

    /// Returns the font used for section labels and other emphasized chrome text.
    pub fn emphasis(&self) -> &Font {
        &self.emphasis
    }

    /// Returns the font used for modal and major chrome headings.
    pub fn heading(&self) -> &Font {
        &self.heading
    }
}

impl Default for ControlTypography {
    fn default() -> Self {
        let regular = font(SYSTEM_UI_FONT_FAMILY);
        let mut emphasis = regular.clone();
        emphasis.weight = FontWeight::SEMIBOLD;
        let heading = emphasis.clone();
        Self::new(regular, emphasis, heading)
    }
}

/// One bounded layer of a semantic chrome shadow.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ControlShadowLayer {
    color: Hsla,
    offset_x: Pixels,
    offset_y: Pixels,
    blur_radius: Pixels,
    spread_radius: Pixels,
}

impl ControlShadowLayer {
    /// Creates one resolved shadow layer.
    pub fn new(
        color: Hsla,
        offset_x: Pixels,
        offset_y: Pixels,
        blur_radius: Pixels,
        spread_radius: Pixels,
    ) -> Self {
        Self {
            color,
            offset_x,
            offset_y,
            blur_radius,
            spread_radius,
        }
    }

    fn into_box_shadow(self) -> BoxShadow {
        BoxShadow {
            color: self.color,
            offset: point(self.offset_x, self.offset_y),
            blur_radius: self.blur_radius,
            spread_radius: self.spread_radius,
        }
    }
}

/// A bounded semantic shadow with at most two layers.
///
/// The application resolves the colors and geometry. Controls only select the semantic shadow
/// attached to their family and cannot introduce per-call paint overrides.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ControlShadow {
    primary: Option<ControlShadowLayer>,
    secondary: Option<ControlShadowLayer>,
}

impl ControlShadow {
    /// Creates a shadow with one layer.
    pub fn single(primary: ControlShadowLayer) -> Self {
        Self {
            primary: Some(primary),
            secondary: None,
        }
    }

    /// Creates a shadow with two ordered layers.
    pub fn double(primary: ControlShadowLayer, secondary: ControlShadowLayer) -> Self {
        Self {
            primary: Some(primary),
            secondary: Some(secondary),
        }
    }

    /// Creates an explicitly shadowless presentation.
    pub fn none() -> Self {
        Self::default()
    }

    pub(crate) fn layers(self) -> Vec<BoxShadow> {
        [self.primary, self.secondary]
            .into_iter()
            .flatten()
            .map(ControlShadowLayer::into_box_shadow)
            .collect()
    }

    pub(crate) fn medium_default() -> Self {
        let color = hsla(0.0, 0.0, 0.0, 0.1);
        Self::double(
            ControlShadowLayer::new(color, px(0.0), px(4.0), px(6.0), px(-1.0)),
            ControlShadowLayer::new(color, px(0.0), px(2.0), px(4.0), px(-2.0)),
        )
    }

    pub(crate) fn large_default() -> Self {
        let color = hsla(0.0, 0.0, 0.0, 0.1);
        Self::double(
            ControlShadowLayer::new(color, px(0.0), px(10.0), px(15.0), px(-3.0)),
            ControlShadowLayer::new(color, px(0.0), px(4.0), px(6.0), px(-4.0)),
        )
    }
}

pub(crate) fn normalized_scale(scale: f32) -> f32 {
    if scale.is_finite() {
        scale.clamp(0.5, 2.0)
    } else {
        1.0
    }
}

pub(crate) fn scale_metric(value: Pixels, scale: f32) -> Pixels {
    value * normalized_scale(scale)
}

pub(crate) fn scale_line_box(
    extent: Pixels,
    baseline_text: Pixels,
    text_scale: f32,
    spacing_scale: f32,
) -> Pixels {
    let content = baseline_text * normalized_scale(text_scale);
    let padding = (extent - baseline_text).max(px(0.0)) * normalized_scale(spacing_scale);
    content + padding
}
