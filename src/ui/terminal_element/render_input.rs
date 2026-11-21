use spaceterm_terminal_client::render::{
    self, Cell, CellStyle, ColorSource, Colors, Cursor, CursorShape, FindHighlight,
    HighlightColors, Underline,
};

use crate::appearance::Color;
use crate::terminal::{
    CellSemanticSnapshot, CellSnapshot, CursorPositionSnapshot, CursorShapeSnapshot,
    CursorSnapshot, FindHighlightSpan, TerminalColor, TerminalColorsSnapshot,
    TerminalDefaultColorSource, TerminalUnderlineSnapshot,
};

impl From<Color> for render::Color {
    fn from(Color { r, g, b, a }: Color) -> Self {
        Self { r, g, b, a }
    }
}

impl From<render::Color> for Color {
    fn from(render::Color { r, g, b, a }: render::Color) -> Self {
        Self { r, g, b, a }
    }
}

fn color_source(source: TerminalColor) -> ColorSource {
    match source {
        TerminalColor::Default => ColorSource::Default,
        TerminalColor::Palette(index) => ColorSource::Palette(index),
        TerminalColor::Rgb(color) => ColorSource::Rgb(color.into()),
    }
}

fn underline(underline: TerminalUnderlineSnapshot) -> Underline {
    match underline {
        TerminalUnderlineSnapshot::None => Underline::None,
        TerminalUnderlineSnapshot::Single => Underline::Single,
        TerminalUnderlineSnapshot::Double => Underline::Double,
        TerminalUnderlineSnapshot::Dotted => Underline::Dotted,
        TerminalUnderlineSnapshot::Dashed => Underline::Dashed,
        TerminalUnderlineSnapshot::Curly => Underline::Curly,
    }
}

pub(super) fn cell_style(cell: &CellSnapshot) -> CellStyle {
    CellStyle {
        foreground: color_source(cell.foreground_source),
        background: color_source(cell.background_source),
        underline_color: color_source(cell.underline_source),
        inverse: cell.inverse,
        bold: cell.bold,
        faint: cell.faint,
        italic: cell.italic,
        blinking: cell.blinking,
        invisible: cell.invisible,
        underline: underline(cell.underline),
        strikethrough: cell.strikethrough,
        overline: cell.overline,
    }
}

pub(super) fn cell(cell: &CellSnapshot) -> Cell<'_> {
    Cell {
        text: &cell.text,
        style: cell_style(cell),
        selected: cell.selected,
        spacer_tail: cell.spacer_tail,
        output: cell.semantic_content == CellSemanticSnapshot::Output,
        hyperlink: cell.hyperlink.as_ref().map(|link| link.identity),
    }
}

pub(super) fn colors(colors: &TerminalColorsSnapshot) -> Colors<'_, Color> {
    let configured = &colors.configured;
    Colors {
        foreground: colors.foreground.into(),
        background: colors.background.into(),
        foreground_overridden: colors.foreground_source
            == TerminalDefaultColorSource::ProgramOverride,
        palette: &colors.palette,
        palette_overrides: &colors.palette_overrides,
        reversed: colors.reversed,
        bold_as_bright: colors.bold_as_bright,
        bright_foreground: configured.bright_foreground.into(),
        dim_foreground: configured.dim_foreground.into(),
        dim: configured.dim.map(Into::into),
        selection: highlight(
            configured.selection_background,
            configured.selection_foreground,
        ),
        find_match: highlight(
            configured.find_match_background,
            configured.find_match_foreground,
        ),
        active_match: highlight(
            configured.find_active_match_background,
            configured.find_active_match_foreground,
        ),
    }
}

fn highlight(background: Color, foreground: Option<Color>) -> HighlightColors {
    HighlightColors {
        background: background.into(),
        foreground: foreground.map(Into::into),
    }
}

pub(super) fn find_spans(spans: &[FindHighlightSpan]) -> Vec<FindHighlight> {
    spans
        .iter()
        .map(|span| FindHighlight {
            row: span.row,
            start_column: span.start_column,
            end_column: span.end_column,
            current: span.current,
        })
        .collect()
}

pub(super) fn cursor(position: CursorPositionSnapshot, style: CursorSnapshot) -> Option<Cursor> {
    style.visible.then(|| Cursor {
        row: position.row,
        column: position.column,
        width_cells: position.width_cells.max(1),
        shape: match style.shape {
            CursorShapeSnapshot::Block => CursorShape::Block,
            CursorShapeSnapshot::Bar => CursorShape::Bar,
            CursorShapeSnapshot::Underline => CursorShape::Underline,
            CursorShapeSnapshot::BlockHollow => CursorShape::BlockHollow,
        },
        blinking: style.blinking,
        color: style.color.into(),
        text_color: style.text_color.into(),
    })
}
