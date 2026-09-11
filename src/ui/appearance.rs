//! Prepared chrome presentation shared by app-owned composites and reusable controls.

use std::sync::Arc;

use gpui::{App, Font, FontWeight, Global, Pixels, TextRun, Window, font, px, rgba};

use crate::appearance::{
    ChromeColors, ChromeDensity, ResolvedChromeAppearance, ResolvedFontDescriptor,
};

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
    pub(crate) colors: ChromeColors,
    pub(crate) regular: Font,
    pub(crate) emphasis: Font,
    pub(crate) heading: Font,
    pub(crate) text_scale: f32,
    pub(crate) spacing_scale: f32,
}

impl Default for ChromeAppearance {
    fn default() -> Self {
        let regular = font(".SystemUIFont");
        let mut emphasis = regular.clone();
        emphasis.weight = FontWeight::SEMIBOLD;
        Self {
            colors: ChromeColors::default(),
            regular,
            heading: emphasis.clone(),
            emphasis,
            text_scale: 1.0,
            spacing_scale: 1.0,
        }
    }
}

impl ChromeAppearance {
    pub(crate) fn shadow(&self) -> Vec<gpui::BoxShadow> {
        vec![gpui::BoxShadow {
            color: rgba(self.colors.shadow.rgba_hex()).into(),
            offset: gpui::point(px(0.0), px(4.0)),
            blur_radius: px(6.0),
            spread_radius: px(-1.0),
        }]
    }
    pub(crate) fn prepare(resolved: &ResolvedChromeAppearance) -> Self {
        Self {
            colors: resolved.colors.clone(),
            regular: prepared_font(&resolved.typography.body),
            emphasis: prepared_font(&resolved.typography.navigation),
            heading: prepared_font(&resolved.typography.heading),
            text_scale: resolved.typography.body.size / 13.0,
            spacing_scale: match resolved.density {
                ChromeDensity::Compact => 1.0,
                ChromeDensity::Comfortable => 1.25,
            },
        }
    }
    pub(crate) fn text_size(&self, baseline: f32) -> Pixels {
        px(baseline * self.text_scale)
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

    pub(crate) fn measure(&self, value: &str, baseline: f32, window: &Window) -> Pixels {
        self.measure_font(value, baseline, &self.regular, window)
    }

    pub(crate) fn measure_emphasis(&self, value: &str, baseline: f32, window: &Window) -> Pixels {
        self.measure_font(value, baseline, &self.emphasis, window)
    }

    fn measure_font(&self, value: &str, baseline: f32, font: &Font, window: &Window) -> Pixels {
        window
            .text_system()
            .shape_line(
                value.to_owned().into(),
                self.text_size(baseline),
                &[TextRun {
                    len: value.len(),
                    font: font.clone(),
                    color: rgba(self.colors.text.rgba_hex()).into(),
                    background_color: None,
                    underline: None,
                    strikethrough: None,
                }],
                None,
            )
            .width
    }
}

pub(crate) fn control_shadow(colors: &ChromeColors, large: bool) -> spaceterm_ui::ControlShadow {
    use spaceterm_ui::{ControlShadow, ControlShadowLayer};
    let color = rgba(colors.shadow.rgba_hex()).into();
    let (offset, blur, spread) = if large {
        (10.0, 15.0, -3.0)
    } else {
        (4.0, 6.0, -1.0)
    };
    ControlShadow::double(
        ControlShadowLayer::new(color, px(0.0), px(offset), px(blur), px(spread)),
        ControlShadowLayer::new(color, px(0.0), px(2.0), px(4.0), px(-2.0)),
    )
}

#[derive(Clone)]
pub(crate) struct InstalledChrome(pub(crate) Arc<ChromeAppearance>);
impl Global for InstalledChrome {}

pub(crate) fn chrome(cx: &App) -> &ChromeAppearance {
    &cx.global::<InstalledChrome>().0
}

pub(crate) fn initialize(cx: &mut App) {
    if !cx.has_global::<InstalledChrome>() {
        cx.set_global(InstalledChrome(Arc::new(ChromeAppearance::default())));
    }
}
