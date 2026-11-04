//! Compact Scrollback rows and one way to read cached and visible rows alike.
//!
//! A cached row stores its text once and each distinct cell style once, and it omits the blank
//! cells after its last written cell. Presenting it builds the desktop row again.
use crate::terminal::{
    CellSemanticSnapshot, CellSnapshot, CellText, RowSnapshot, TerminalColor,
    TerminalUnderlineSnapshot,
};
use std::sync::Arc;

/// The text of a cell that holds nothing, as the server presents it.
pub(super) const BLANK: &str = " ";

/// A cell that holds nothing, in the default style. It presents a row the cache does not hold yet.
pub(super) fn blank_cell() -> CellSnapshot {
    CellSnapshot {
        text: CellText::new_inline(BLANK),
        foreground_source: TerminalColor::Default,
        background_source: TerminalColor::Default,
        inverse: false,
        bold: false,
        faint: false,
        italic: false,
        blinking: false,
        invisible: false,
        underline: TerminalUnderlineSnapshot::None,
        underline_source: TerminalColor::Default,
        strikethrough: false,
        overline: false,
        selected: false,
        spacer_tail: false,
        semantic_content: CellSemanticSnapshot::Output,
        hyperlink: None,
    }
}

/// One Scrollback row as the client caches it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CachedRow {
    columns: u16,
    wrapped: bool,
    text: Box<str>,
    cells: Box<[Packed]>,
    /// Every field of a cell except its text, `selected` and `spacer_tail`.
    styles: Box<[CellSnapshot]>,
    /// The style of the blank cells after `cells`.
    fill: u16,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Packed {
    /// The end of this cell's text in `CachedRow::text`.
    end: u32,
    style: u16,
    spacer_tail: bool,
}

impl CachedRow {
    pub(crate) fn new(cells: &[CellSnapshot], wrapped: bool) -> Self {
        let template = |cell: &CellSnapshot| CellSnapshot {
            text: CellText::default(),
            selected: false,
            spacer_tail: false,
            ..cell.clone()
        };
        let fill_template = template(cells.last().unwrap_or(&blank_cell()));
        let kept = cells
            .iter()
            .rposition(|cell| {
                cell.spacer_tail || cell.text != BLANK || template(cell) != fill_template
            })
            .map_or(0, |last| last + 1);
        let mut styles: Vec<CellSnapshot> = Vec::new();
        let mut style_of = |template: CellSnapshot| -> u16 {
            let index = styles
                .iter()
                .position(|known| *known == template)
                .unwrap_or_else(|| {
                    styles.push(template);
                    styles.len() - 1
                });
            u16::try_from(index).unwrap_or(u16::MAX)
        };
        let fill = style_of(fill_template);
        let mut text = String::new();
        let mut packed = Vec::with_capacity(kept);
        for cell in &cells[..kept] {
            text.push_str(&cell.text);
            packed.push(Packed {
                end: u32::try_from(text.len()).unwrap_or(u32::MAX),
                style: style_of(template(cell)),
                spacer_tail: cell.spacer_tail,
            });
        }
        Self {
            columns: u16::try_from(cells.len()).unwrap_or(u16::MAX),
            wrapped,
            text: text.into_boxed_str(),
            cells: packed.into_boxed_slice(),
            styles: styles.into_boxed_slice(),
            fill,
        }
    }

    /// The desktop row this cached row presents.
    pub(crate) fn snapshot(&self) -> RowSnapshot {
        (0..self.columns)
            .map(|column| {
                let mut cell = self.style(column).clone();
                cell.text = self.text(column).into();
                cell.spacer_tail = self.spacer_tail(column);
                cell
            })
            .collect::<Vec<_>>()
            .into()
    }

    fn text(&self, column: u16) -> &str {
        let column = usize::from(column);
        let Some(cell) = self.cells.get(column) else {
            return BLANK;
        };
        let start = column
            .checked_sub(1)
            .and_then(|previous| self.cells.get(previous))
            .map_or(0, |previous| previous.end as usize);
        self.text.get(start..cell.end as usize).unwrap_or(BLANK)
    }

    fn spacer_tail(&self, column: u16) -> bool {
        self.cells
            .get(usize::from(column))
            .is_some_and(|cell| cell.spacer_tail)
    }

    fn style(&self, column: u16) -> &CellSnapshot {
        let style = self
            .cells
            .get(usize::from(column))
            .map_or(self.fill, |cell| cell.style);
        &self.styles[usize::from(style)]
    }
}

/// A cached or visible row, read the same way.
#[derive(Clone, Copy)]
pub(super) enum RowRef<'a> {
    Cached(&'a CachedRow),
    Visible(&'a RowSnapshot, bool),
}

impl<'a> RowRef<'a> {
    pub(super) fn columns(self) -> u16 {
        match self {
            Self::Cached(row) => row.columns,
            Self::Visible(cells, _) => u16::try_from(cells.len()).unwrap_or(u16::MAX),
        }
    }

    pub(super) fn wrapped(self) -> bool {
        match self {
            Self::Cached(row) => row.wrapped,
            Self::Visible(_, wrapped) => wrapped,
        }
    }

    pub(super) fn text(self, column: u16) -> &'a str {
        match self {
            Self::Cached(row) => row.text(column),
            Self::Visible(cells, _) => cells
                .get(usize::from(column))
                .map_or(BLANK, |cell| cell.text.as_str()),
        }
    }

    pub(super) fn spacer_tail(self, column: u16) -> bool {
        match self {
            Self::Cached(row) => row.spacer_tail(column),
            Self::Visible(cells, _) => cells
                .get(usize::from(column))
                .is_some_and(|cell| cell.spacer_tail),
        }
    }

    /// Whether the cell holds text. Blank cells and the spacers of wide characters hold none.
    pub(super) fn has_text(self, column: u16) -> bool {
        column < self.columns() && !self.spacer_tail(column) && self.text(column) != BLANK
    }

    /// The cell's colors and attributes. Its text is not meaningful.
    pub(super) fn style(self, column: u16) -> Option<&'a CellSnapshot> {
        match self {
            Self::Cached(row) => (column < row.columns).then(|| row.style(column)),
            Self::Visible(cells, _) => cells.get(usize::from(column)),
        }
    }

    /// Whether both present the same cells and soft wrap.
    pub(super) fn same(self, other: RowRef<'_>) -> bool {
        if self.wrapped() != other.wrapped() {
            return false;
        }
        match (self, other) {
            (Self::Visible(a, _), RowRef::Visible(b, _)) => Arc::ptr_eq(a, b) || a == b,
            (Self::Cached(a), RowRef::Cached(b)) => a == b,
            _ => self.snapshot() == other.snapshot(),
        }
    }

    pub(super) fn snapshot(self) -> RowSnapshot {
        match self {
            Self::Cached(row) => row.snapshot(),
            Self::Visible(cells, _) => Arc::clone(cells),
        }
    }
}
