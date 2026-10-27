//! Terminal presentation as row style runs against per-channel style and link tables.
//!
//! The server keeps the authoritative screen. A keyframe (`Frame::base` is `None`) carries
//! geometry, modes, colors and every row that is not blank, and resets the channel's style and
//! link tables. Any other update applies to the acknowledged frame `base`: first `geometry`, which
//! blanks every row when the grid's size changes, then `shift`, then the replacement rows.
use super::message::check;
use super::{Fatal, limits};
use crate::pane::{Color, effects::Hyperlink, graphics, reading};
use serde::{Deserialize, Serialize};

/// Bits of [`Style::flags`].
pub mod flags {
    pub const INVERSE: u16 = 1;
    pub const BOLD: u16 = 1 << 1;
    pub const FAINT: u16 = 1 << 2;
    pub const ITALIC: u16 = 1 << 3;
    pub const BLINKING: u16 = 1 << 4;
    pub const INVISIBLE: u16 = 1 << 5;
    pub const STRIKETHROUGH: u16 = 1 << 6;
    pub const OVERLINE: u16 = 1 << 7;
    /// The cell is in the server-side selection. Deleted by scrollback.
    pub const SELECTED: u16 = 1 << 8;
    pub const ALL: u16 = (1 << 9) - 1;
}

/// [`Style::underline`]: none, single, double, curly, dotted, dashed.
pub const MAX_UNDERLINE: u8 = 5;
/// [`Cursor::shape`]: block, bar, underline, hollow block.
pub const MAX_CURSOR_SHAPE: u8 = 3;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ScreenUpdate {
    pub cursor: Cursor,
    pub history: History,
    /// Present in keyframes and when the grid changes.
    pub geometry: Option<Geometry>,
    /// Present in keyframes and when a mode changes.
    pub modes: Option<Modes>,
    /// Present in keyframes and when a color changes.
    pub colors: Option<Colors>,
    /// Applied to the base frame's rows before `rows`.
    pub shift: Option<ScrollShift>,
    /// New definitions, applied before `rows`. A definition persists for the channel until the
    /// next keyframe, and replaces an earlier definition of its id. Ids are unique in one update.
    pub styles: Vec<StyleDefinition>,
    /// New definitions, with the same rules as `styles`.
    pub links: Vec<LinkDefinition>,
    /// Replacement visible rows, in increasing index order. Rows a keyframe omits are blank.
    pub rows: Vec<Row>,
    /// Present when the graphics manifest changes.
    pub graphics: Option<Result<graphics::Snapshot, graphics::Error>>,
    pub accessibility_revision: u64,
    /// Fences pointer and wheel coordinates. Increases when the visible grid's identity changes.
    pub generation: u64,
    /// Server-side reading state, present in keyframes and when it changes. Deleted by
    /// scrollback.
    pub reading: Option<reading::Snapshot>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Cursor {
    pub position: Option<CursorPosition>,
    pub visible: bool,
    pub blinking: bool,
    pub shape: u8,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct CursorPosition {
    pub column: u16,
    pub row: u16,
    /// 1, or 2 on a wide character.
    pub width: u8,
}

/// The primary screen's Scrollback above the visible rows.
///
/// Positions count every row that entered history in this `epoch`, so a position names the same
/// row while the server discards the oldest rows. Rows `oldest..total` are retained, and visible
/// row `r` is position `total + r`. Erased rows are never renumbered: clearing the Scrollback
/// moves `oldest` up to `total`. A new `epoch`, after the engine rewraps or resets history,
/// invalidates every cached row. The epoch belongs to the Terminal Session, so it survives a
/// reconnect. While the alternate screen is active, the primary screen's history stands.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct History {
    pub epoch: u64,
    pub oldest: u64,
    pub total: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Geometry {
    pub columns: u16,
    pub rows: u16,
    pub cell_pixels: Option<(u16, u16)>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Modes {
    pub alternate_screen: bool,
    pub mouse_tracking: bool,
    pub alternate_scroll: bool,
    pub reversed: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Colors {
    pub foreground: [u8; 3],
    pub background: [u8; 3],
    pub cursor: [u8; 3],
    pub cursor_text: [u8; 3],
    /// All 256 palette entries.
    pub palette: Vec<[u8; 3]>,
    pub foreground_overridden: bool,
    pub background_overridden: bool,
    pub cursor_overridden: bool,
    /// One bit per palette entry the program overrode, least significant bit first.
    pub palette_overrides: [u64; 4],
}

/// Rows `top..bottom` of the base frame move up by `lines`, or down when `lines` is negative.
/// Rows that move in from outside the region are blank until `rows` replaces them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScrollShift {
    pub top: u16,
    pub bottom: u16,
    pub lines: i16,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Style {
    pub foreground: Color,
    pub background: Color,
    pub underline_color: Color,
    pub flags: u16,
    pub underline: u8,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StyleDefinition {
    pub id: u16,
    pub style: Style,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LinkDefinition {
    pub id: u16,
    pub link: Hyperlink,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Row {
    pub index: u16,
    pub wrapped: bool,
    /// Runs cover the row from column 0. Columns after the last run are blank in the default
    /// style.
    pub runs: Vec<Run>,
}

/// Consecutive cells sharing one style and one link.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Run {
    pub style: StyleRef,
    pub link: Option<u16>,
    pub text: String,
    pub cells: Cells,
}

/// A run's style.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum StyleRef {
    Default,
    /// A definition in the channel's or the page's table. Ids start at 1.
    Id(u16),
    /// A style the full table cannot hold, so no cell loses its style.
    Inline(Style),
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Cells {
    /// Each `char` of `text` fills one single-width cell.
    Narrow,
    /// The byte length of each cell's text in order, at most [`limits::MAX_CELL_BYTES`]. A wide
    /// cell is followed by a zero-length spacer cell, which may start the next run when its
    /// style differs.
    Explicit(Vec<u16>),
}

/// Scrollback rows with page-local style and link tables, so a page never depends on the order in
/// which frames and pages are written.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Page {
    pub epoch: u64,
    pub start: u64,
    /// `Row::index` counts from `start`.
    pub rows: Vec<Row>,
    pub styles: Vec<StyleDefinition>,
    pub links: Vec<LinkDefinition>,
}

impl ScreenUpdate {
    /// Checks the semantic bounds postcard cannot express.
    pub fn validate(&self) -> Result<(), Fatal> {
        check(self.history.oldest <= self.history.total)?;
        if let Some(position) = self.cursor.position {
            check(
                position.row < crate::pane::MAX_ROWS
                    && position.column < crate::pane::MAX_COLUMNS
                    && (1..=2).contains(&position.width),
            )?;
        }
        check(self.cursor.shape <= MAX_CURSOR_SHAPE)?;
        if let Some(geometry) = self.geometry {
            check(
                (2..=crate::pane::MAX_COLUMNS).contains(&geometry.columns)
                    && (2..=crate::pane::MAX_ROWS).contains(&geometry.rows)
                    && geometry.cell_pixels.is_none_or(|(width, height)| {
                        (1..=256).contains(&width) && (1..=256).contains(&height)
                    }),
            )?;
        }
        if let Some(colors) = &self.colors {
            check(colors.palette.len() == 256)?;
        }
        if let Some(shift) = self.shift {
            check(
                shift.top < shift.bottom
                    && shift.bottom <= crate::pane::MAX_ROWS
                    && shift.lines != 0
                    && shift.lines.unsigned_abs() < shift.bottom - shift.top,
            )?;
        }
        check(self.rows.len() <= usize::from(crate::pane::MAX_ROWS))?;
        check(
            self.rows
                .windows(2)
                .all(|pair| pair[0].index < pair[1].index),
        )?;
        if let Some(Ok(graphics)) = &self.graphics {
            check(graphics.validate().is_ok())?;
        }
        tables(&self.styles, &self.links, &self.rows, crate::pane::MAX_ROWS)
    }
}

impl Page {
    pub fn validate(&self) -> Result<(), Fatal> {
        check(self.rows.len() <= usize::from(limits::MAX_PAGE_ROWS))?;
        check(
            self.rows
                .windows(2)
                .all(|pair| pair[0].index < pair[1].index),
        )?;
        tables(&self.styles, &self.links, &self.rows, limits::MAX_PAGE_ROWS)
    }
}

impl Style {
    pub const DEFAULT: Self = Self {
        foreground: Color::Default,
        background: Color::Default,
        underline_color: Color::Default,
        flags: 0,
        underline: 0,
    };

    fn valid(&self) -> bool {
        self.flags & !flags::ALL == 0 && self.underline <= MAX_UNDERLINE
    }
}

impl ScrollShift {
    /// Moves `rows[top..bottom]` by `lines` and fills the rows that move in with `blank`.
    /// `None` when the region does not fit `rows`.
    pub fn apply<T>(self, rows: &mut [T], mut blank: impl FnMut() -> T) -> Option<()> {
        let region = rows.get_mut(usize::from(self.top)..usize::from(self.bottom))?;
        let lines = usize::from(self.lines.unsigned_abs());
        if lines >= region.len() {
            return None;
        }
        let length = region.len();
        let vacated = if self.lines > 0 {
            region.rotate_left(lines);
            &mut region[length - lines..]
        } else {
            region.rotate_right(lines);
            &mut region[..lines]
        };
        for row in vacated {
            *row = blank();
        }
        Some(())
    }
}

impl Run {
    /// The cells of this run as byte ranges of `text`, in order. An empty range is a spacer
    /// cell. The ranges of a run that failed validation may fall outside `text`.
    pub fn cells(&self) -> impl Iterator<Item = std::ops::Range<usize>> + '_ {
        let narrow = match &self.cells {
            Cells::Narrow => Some(
                self.text
                    .char_indices()
                    .map(|(start, c)| start..start + c.len_utf8()),
            ),
            Cells::Explicit(_) => None,
        };
        let explicit = match &self.cells {
            Cells::Explicit(lengths) => {
                Some(lengths.iter().scan(0, |start: &mut usize, length| {
                    let cell = *start..*start + usize::from(*length);
                    *start = cell.end;
                    Some(cell)
                }))
            }
            Cells::Narrow => None,
        };
        narrow
            .into_iter()
            .flatten()
            .chain(explicit.into_iter().flatten())
    }

    /// The number of cells in this run.
    pub fn width(&self) -> usize {
        match &self.cells {
            Cells::Narrow => self.text.chars().count(),
            Cells::Explicit(lengths) => lengths.len(),
        }
    }

    /// Whether explicit lengths cover `text` exactly, on character boundaries and within
    /// [`limits::MAX_CELL_BYTES`]. Walks the lengths without allocating.
    fn covers_text(&self) -> bool {
        let Cells::Explicit(lengths) = &self.cells else {
            return true;
        };
        let mut end = 0;
        for length in lengths {
            end += usize::from(*length);
            if usize::from(*length) > limits::MAX_CELL_BYTES || !self.text.is_char_boundary(end) {
                return false;
            }
        }
        end == self.text.len()
    }
}

fn tables(
    styles: &[StyleDefinition],
    links: &[LinkDefinition],
    rows: &[Row],
    max_rows: u16,
) -> Result<(), Fatal> {
    check(styles.len() <= limits::MAX_STYLES && links.len() <= limits::MAX_LINKS)?;
    let mut defined = vec![false; limits::MAX_STYLES + 1];
    for definition in styles {
        let id = usize::from(definition.id);
        check((1..=limits::MAX_STYLES).contains(&id) && !defined[id])?;
        defined[id] = true;
        check(definition.style.valid())?;
    }
    let mut defined = vec![false; limits::MAX_LINKS];
    for definition in links {
        let id = usize::from(definition.id);
        check(id < limits::MAX_LINKS && !defined[id])?;
        defined[id] = true;
        let (Hyperlink::Url(target) | Hyperlink::File(target)) = &definition.link;
        check(target.len() <= limits::MAX_LINK_BYTES)?;
    }
    for row in rows {
        check(row.index < max_rows)?;
        check(row.runs.len() <= usize::from(crate::pane::MAX_COLUMNS))?;
        let mut columns = 0;
        for run in &row.runs {
            check(match run.style {
                StyleRef::Default => true,
                StyleRef::Id(id) => (1..=limits::MAX_STYLES).contains(&usize::from(id)),
                StyleRef::Inline(style) => style.valid(),
            })?;
            check(
                run.link
                    .is_none_or(|link| usize::from(link) < limits::MAX_LINKS),
            )?;
            // Bound the cell count before walking the lengths.
            columns += run.width();
            check(columns <= usize::from(crate::pane::MAX_COLUMNS) && run.covers_text())?;
        }
    }
    Ok(())
}
