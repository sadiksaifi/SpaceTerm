//! Plans terminal drawing in grid coordinates from decoded frame cells and client view state.
//!
//! A row input contains one entry per column, including blank cells and wide-cell spacer tails.
//! Painters shape the returned text with ligatures disabled and anchor it to the specified columns.

mod color;
mod symbol;

pub use color::{Color, ColorSource, Colors, HighlightColors, effective_colors};
pub use symbol::{TerminalSymbol, terminal_symbol};

use std::ops::RangeInclusive;
use unicode_bidi::{BidiClass, bidi_class};

/// A decoded cell's drawing attributes. The color sources survive resolution against the theme.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct CellStyle {
    pub foreground: ColorSource,
    pub background: ColorSource,
    pub underline_color: ColorSource,
    pub inverse: bool,
    pub bold: bool,
    pub faint: bool,
    pub italic: bool,
    pub blinking: bool,
    pub invisible: bool,
    pub underline: Underline,
    pub strikethrough: bool,
    pub overline: bool,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum Underline {
    #[default]
    None,
    Single,
    Double,
    Curly,
    Dotted,
    Dashed,
}

/// One column of a decoded row, in frame order.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Cell<'a> {
    pub text: &'a str,
    pub style: CellStyle,
    pub spacer_tail: bool,
    pub selected: bool,
    /// Output cells may extend their backgrounds into the terminal's padding.
    pub output: bool,
    pub hyperlink: Option<u64>,
}

/// A Find match in a displayed row. The last column is inclusive.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FindHighlight {
    pub row: u16,
    pub start_column: u16,
    pub end_column: u16,
    pub current: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BackgroundSpan {
    pub start: usize,
    pub len: usize,
    pub color: Color,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DecorationKind {
    Underline(Underline),
    Overline,
    Strikethrough,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DecorationSpan {
    pub start: usize,
    pub len: usize,
    pub kind: DecorationKind,
    pub color: Color,
    pub blinking: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SymbolSpan {
    pub start: usize,
    pub width_cells: u8,
    pub color: Color,
    pub blinking: bool,
    pub symbol: TerminalSymbol,
}

/// Style for a UTF-8 byte range of a text fragment.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TextStyle {
    pub color: Color,
    pub bold: bool,
    pub italic: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TextStyleRun {
    /// Exclusive byte offset within the fragment. The first run starts at zero.
    pub end: usize,
    pub style: TextStyle,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TextFragment {
    pub start: usize,
    pub width_cells: usize,
    pub text: String,
    pub styles: Vec<TextStyleRun>,
    /// Each scalar is anchored to its own column. Otherwise the entire fragment is one cell's
    /// text and the painter preserves its native shaping offsets from the head column.
    pub simple_cells: bool,
    pub blinking: bool,
}

impl TextFragment {
    fn from_cell(
        column: usize,
        width_cells: usize,
        cell: Cell<'_>,
        color: Color,
        simple_cells: bool,
    ) -> Self {
        Self {
            start: column,
            width_cells,
            text: cell.text.to_owned(),
            styles: vec![TextStyleRun {
                end: cell.text.len(),
                style: TextStyle {
                    color,
                    bold: cell.style.bold,
                    italic: cell.style.italic,
                },
            }],
            simple_cells,
            blinking: cell.style.blinking,
        }
    }

    fn push(&mut self, cell: Cell<'_>, color: Color) {
        self.width_cells += 1;
        self.text.push_str(cell.text);
        let style = TextStyle {
            color,
            bold: cell.style.bold,
            italic: cell.style.italic,
        };
        let end = self.text.len();
        if let Some(previous) = self.styles.last_mut()
            && previous.style == style
        {
            previous.end = end;
        } else {
            self.styles.push(TextStyleRun { end, style });
        }
    }
}

/// Background colors to extend into the space around the grid.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct RowPadding {
    pub leading: Option<Color>,
    pub trailing: Option<Color>,
    pub extends_below: bool,
}

/// Draw data for one row. Paint the terminal backgrounds, Find backgrounds and selections in that
/// order before painting foreground content. Underlines and overlines go under text, and
/// strikethroughs go over it.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Row {
    pub backgrounds: Vec<BackgroundSpan>,
    pub selections: Vec<BackgroundSpan>,
    pub fragments: Vec<TextFragment>,
    pub symbols: Vec<SymbolSpan>,
    pub under_text_decorations: Vec<DecorationSpan>,
    pub over_text_decorations: Vec<DecorationSpan>,
    pub padding: RowPadding,
}

/// Plans the stable drawing for a row. Find backgrounds can be drawn from [`find_backgrounds`].
pub fn plan_row<'a>(
    cells: impl IntoIterator<Item = Cell<'a>>,
    colors: &Colors<'_, impl Copy + Into<Color>>,
    row_index: usize,
    find_spans: &[FindHighlight],
) -> Row {
    let mut row = Row::default();
    let mut cells = cells.into_iter().enumerate().peekable();
    let mut overlines = Vec::new();
    let mut extends_below = cells.peek().is_some();
    let default_background = colors.effective_background();
    let mut columns = 0;

    while let Some((column, cell)) = cells.next() {
        columns = column + 1;
        let style = cell.style;
        let placeholder = is_kitty_placeholder(cell.text);
        let (foreground, background) = effective_colors(style, colors);
        extends_below &=
            background != default_background && cell.output && !is_perfect_fit_powerline(cell.text);
        // Explicit colors and inverse video can paint over a translucent surface even when the
        // resolved color equals the default.
        if style.inverse
            || (!colors.reversed && style.background != ColorSource::Default)
            || (colors.reversed && style.foreground != ColorSource::Default)
            || background != default_background
        {
            push_background(&mut row.backgrounds, column, background);
        }

        if cell.selected {
            push_background(&mut row.selections, column, colors.selection.background);
        }

        if style.invisible || placeholder {
            continue;
        }
        let foreground = if cell.selected {
            colors.selection.foreground
        } else {
            color::find_foreground(row_index, column, find_spans, colors)
        }
        .unwrap_or(foreground);

        // Each column owns its background and decorations, including spacer tails with
        // different styles from the head cell.
        if style.underline != Underline::None {
            push_decoration(
                &mut row.under_text_decorations,
                DecorationSpan {
                    start: column,
                    len: 1,
                    kind: DecorationKind::Underline(style.underline),
                    color: color::underline_color(style, colors, foreground),
                    blinking: style.blinking,
                },
            );
        }
        if style.overline {
            push_decoration(
                &mut overlines,
                DecorationSpan {
                    start: column,
                    len: 1,
                    kind: DecorationKind::Overline,
                    color: foreground,
                    blinking: style.blinking,
                },
            );
        }
        if style.strikethrough {
            push_decoration(
                &mut row.over_text_decorations,
                DecorationSpan {
                    start: column,
                    len: 1,
                    kind: DecorationKind::Strikethrough,
                    color: foreground,
                    blinking: style.blinking,
                },
            );
        }

        if cell.spacer_tail || cell.text.is_empty() {
            continue;
        }
        let width_cells = 1 + u8::from(cells.peek().is_some_and(|(_, next)| next.spacer_tail));
        if let Some(symbol) = terminal_symbol(cell.text) {
            row.symbols.push(SymbolSpan {
                start: column,
                width_cells,
                color: foreground,
                blinking: style.blinking,
                symbol,
            });
            continue;
        }

        let simple_cells = width_cells == 1 && is_simple_cell(cell.text);
        if simple_cells
            && let Some(previous) = row.fragments.last_mut()
            && previous.simple_cells
            && previous.blinking == style.blinking
            && previous.start + previous.width_cells == column
        {
            previous.push(cell, foreground);
        } else {
            row.fragments.push(TextFragment::from_cell(
                column,
                usize::from(width_cells),
                cell,
                foreground,
                simple_cells,
            ));
        }
    }

    row.under_text_decorations.extend(overlines);
    row.padding = RowPadding {
        leading: row
            .backgrounds
            .first()
            .filter(|span| span.start == 0)
            .map(|span| span.color),
        trailing: row
            .backgrounds
            .last()
            .filter(|span| span.start + span.len == columns)
            .map(|span| span.color),
        extends_below,
    };
    row
}

/// Find backgrounds for a row in paint order, with the current match on top of other matches.
pub fn find_backgrounds(
    row_index: usize,
    spans: &[FindHighlight],
    find_match: HighlightColors,
    active_match: HighlightColors,
) -> Vec<BackgroundSpan> {
    [false, true]
        .into_iter()
        .flat_map(|current| {
            spans
                .iter()
                .filter(move |span| usize::from(span.row) == row_index && span.current == current)
                .map(move |span| BackgroundSpan {
                    start: usize::from(span.start_column),
                    len: usize::from(
                        span.end_column
                            .saturating_sub(span.start_column)
                            .saturating_add(1),
                    ),
                    color: if current {
                        active_match.background
                    } else {
                        find_match.background
                    },
                })
        })
        .collect()
}

/// Draws a hover underline across the supplied occurrence, skipping cells with their own underline.
pub fn hyperlink_hover<'a>(
    cells: impl IntoIterator<Item = Cell<'a>>,
    columns: RangeInclusive<usize>,
    identity: u64,
    color: Color,
) -> Vec<DecorationSpan> {
    let mut spans = Vec::new();
    for (column, cell) in cells
        .into_iter()
        .enumerate()
        .skip(*columns.start())
        .take_while(|(column, _)| column <= columns.end())
    {
        if cell.style.invisible
            || is_kitty_placeholder(cell.text)
            || cell.style.underline != Underline::None
            || cell.hyperlink != Some(identity)
        {
            continue;
        }
        push_decoration(
            &mut spans,
            DecorationSpan {
                start: column,
                len: 1,
                kind: DecorationKind::Underline(Underline::Single),
                color,
                blinking: false,
            },
        );
    }
    spans
}

fn push_background(spans: &mut Vec<BackgroundSpan>, column: usize, color: Color) {
    if let Some(previous) = spans.last_mut()
        && previous.start + previous.len == column
        && previous.color == color
    {
        previous.len += 1;
    } else {
        spans.push(BackgroundSpan {
            start: column,
            len: 1,
            color,
        });
    }
}

fn push_decoration(spans: &mut Vec<DecorationSpan>, span: DecorationSpan) {
    if let Some(previous) = spans.last_mut()
        && previous.start + previous.len == span.start
        && previous.kind == span.kind
        && previous.color == span.color
        && previous.blinking == span.blinking
    {
        previous.len += span.len;
    } else {
        spans.push(span);
    }
}

fn is_kitty_placeholder(text: &str) -> bool {
    // The whole grapheme, including diacritics, encodes image placement.
    text.starts_with('\u{10eeee}')
}

fn is_perfect_fit_powerline(text: &str) -> bool {
    text.chars().next().is_some_and(|character| {
        matches!(u32::from(character), 0xe0b0..=0xe0c8 | 0xe0ca | 0xe0cc..=0xe0d2 | 0xe0d4)
    })
}

fn is_simple_cell(text: &str) -> bool {
    let mut chars = text.chars();
    let Some(character) = chars.next() else {
        return false;
    };
    chars.next().is_none()
        && !matches!(
            bidi_class(character),
            BidiClass::R
                | BidiClass::AL
                | BidiClass::AN
                | BidiClass::NSM
                | BidiClass::RLE
                | BidiClass::RLO
                | BidiClass::RLI
                | BidiClass::LRE
                | BidiClass::LRO
                | BidiClass::LRI
                | BidiClass::FSI
                | BidiClass::PDI
                | BidiClass::PDF
        )
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum CursorShape {
    #[default]
    Block,
    Bar,
    Underline,
    BlockHollow,
}

/// A visible cursor with its frame-supplied position and width in cells.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Cursor {
    pub row: u16,
    pub column: u16,
    pub width_cells: u8,
    pub shape: CursorShape,
    pub blinking: bool,
    pub color: Color,
    pub text_color: Color,
}

impl Cursor {
    /// A visible cursor is hollow and steady when the terminal lacks input focus.
    pub fn with_input_focus(self, focused: bool) -> Self {
        if focused {
            self
        } else {
            Self {
                shape: CursorShape::BlockHollow,
                blinking: false,
                ..self
            }
        }
    }

    pub fn recolors_text(self) -> bool {
        self.shape == CursorShape::Block
    }
}

pub fn visible_in_phase(blinking: bool, blink_phase_visible: bool) -> bool {
    !blinking || blink_phase_visible
}

#[cfg(test)]
mod tests;
