use gpui::{BoxShadow, Font, FontWeight, Hsla, Pixels, font, point, px};

const SYSTEM_UI_FONT_FAMILY: &str = ".SystemUIFont";

/// The resolved fonts used by reusable chrome controls.
///
/// Sizes and line heights remain semantic metrics of each control family. Each font carries the
/// complete effective family, fallback, feature, weight, and style choices used for shaping.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ControlTypography {
    regular: Font,
    emphasis: Font,
    section: Font,
    heading: Font,
    shortcut: Font,
    caption: Font,
    badge: Font,
}

impl ControlTypography {
    /// Creates a complete resolved typography catalog.
    pub fn new(regular: Font, emphasis: Font, heading: Font) -> Self {
        let shortcut = regular.clone();
        let caption = regular.clone();
        let badge = emphasis.clone();
        Self {
            regular,
            section: emphasis.clone(),
            emphasis,
            heading,
            shortcut,
            caption,
            badge,
        }
    }

    /// Sets the distinct font used by group headings in command and menu surfaces.
    pub fn section(mut self, section: Font) -> Self {
        self.section = section;
        self
    }

    /// Supplies the fonts used by compact semantic text roles.
    pub fn semantic_fonts(mut self, shortcut: Font, caption: Font, badge: Font) -> Self {
        self.shortcut = shortcut;
        self.caption = caption;
        self.badge = badge;
        self
    }

    /// Returns the font used for ordinary labels, values, and editable text.
    pub fn regular(&self) -> &Font {
        &self.regular
    }

    /// Returns the font used for section labels and other emphasized chrome text.
    pub fn emphasis(&self) -> &Font {
        &self.emphasis
    }

    /// Returns the font used for group headings in command and menu surfaces.
    pub fn section_font(&self) -> &Font {
        &self.section
    }

    /// Returns the font used for modal and major chrome headings.
    pub fn heading(&self) -> &Font {
        &self.heading
    }

    /// Returns the font used for keyboard equivalents.
    pub fn shortcut(&self) -> &Font {
        &self.shortcut
    }

    /// Returns the font used for compact explanatory text.
    pub fn caption(&self) -> &Font {
        &self.caption
    }

    /// Returns the font used for compact tabular labels.
    pub fn badge(&self) -> &Font {
        &self.badge
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
            inset: false,
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
