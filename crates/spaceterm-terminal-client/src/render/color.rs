use super::{CellStyle, FindHighlight};

/// Unpremultiplied RGBA.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Color {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ColorSource {
    #[default]
    Default,
    Palette(u8),
    Rgb(Color),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct HighlightColors {
    pub background: Color,
    /// Keep the resolved cell foreground when absent.
    pub foreground: Option<Color>,
}

/// Frame colors and the configured theme roles used to paint them. The palette can borrow the
/// client's own color representation.
pub struct Colors<'a, PaletteColor = Color> {
    pub foreground: Color,
    pub background: Color,
    pub foreground_overridden: bool,
    pub palette: &'a [PaletteColor; 256],
    pub palette_overrides: &'a [bool; 256],
    pub reversed: bool,
    pub bold_as_bright: bool,
    pub bright_foreground: Color,
    pub dim_foreground: Color,
    pub dim: [Color; 8],
    pub selection: HighlightColors,
    pub find_match: HighlightColors,
    pub active_match: HighlightColors,
}

impl<PaletteColor> Colors<'_, PaletteColor> {
    pub fn effective_background(&self) -> Color {
        if self.reversed {
            self.foreground
        } else {
            self.background
        }
    }
}

/// Resolves a cell's foreground and background before Selection and Find overlays.
pub fn effective_colors(
    style: CellStyle,
    colors: &Colors<'_, impl Copy + Into<Color>>,
) -> (Color, Color) {
    let foreground_source = match (style.foreground, style.bold && colors.bold_as_bright) {
        (ColorSource::Palette(index @ 0..=7), true) => ColorSource::Palette(index + 8),
        (source, _) => source,
    };
    let mut foreground = if colors.bold_as_bright
        && style.bold
        && !style.faint
        && foreground_source == ColorSource::Default
        && !colors.foreground_overridden
    {
        colors.bright_foreground
    } else {
        resolve_color(foreground_source, colors, colors.foreground)
    };
    let mut background = resolve_color(style.background, colors, colors.background);
    let reversed = style.inverse ^ colors.reversed;
    if reversed {
        std::mem::swap(&mut foreground, &mut background);
    }
    if style.faint {
        let source = if reversed {
            style.background
        } else {
            foreground_source
        };
        foreground = dim_color(foreground, source, colors, !reversed);
    }
    (foreground, background)
}

pub(super) fn underline_color(
    style: CellStyle,
    colors: &Colors<'_, impl Copy + Into<Color>>,
    effective_foreground: Color,
) -> Color {
    let mut color = resolve_color(style.underline_color, colors, effective_foreground);
    if style.faint && style.underline_color != ColorSource::Default {
        color.a = color.a.div_ceil(2);
    }
    color
}

pub(super) fn find_foreground(
    row: usize,
    column: usize,
    spans: &[FindHighlight],
    colors: &Colors<'_, impl Copy + Into<Color>>,
) -> Option<Color> {
    let mut matches = spans.iter().filter(|span| {
        usize::from(span.row) == row
            && (usize::from(span.start_column)..=usize::from(span.end_column)).contains(&column)
    });
    if matches.clone().any(|span| span.current) {
        colors.active_match.foreground
    } else if matches.next().is_some() {
        colors.find_match.foreground
    } else {
        None
    }
}

fn dim_color(
    color: Color,
    source: ColorSource,
    colors: &Colors<'_, impl Copy + Into<Color>>,
    default_is_foreground: bool,
) -> Color {
    match source {
        ColorSource::Default if default_is_foreground && !colors.foreground_overridden => {
            colors.dim_foreground
        }
        ColorSource::Palette(index @ 0..=15) if !colors.palette_overrides[usize::from(index)] => {
            colors.dim[usize::from(index % 8)]
        }
        _ => Color {
            a: color.a.div_ceil(2),
            ..color
        },
    }
}

fn resolve_color(
    source: ColorSource,
    colors: &Colors<'_, impl Copy + Into<Color>>,
    default: Color,
) -> Color {
    match source {
        ColorSource::Default => default,
        ColorSource::Palette(index) => colors.palette[usize::from(index)].into(),
        ColorSource::Rgb(color) => color,
    }
}
