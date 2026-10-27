//! Terminal presentation as row style runs against per-channel style and link tables.
//!
//! The server keeps the authoritative screen. A keyframe (`Frame::base` is `None`) carries every
//! row, geometry, modes and colors, and resets the channel's style and link tables. Any other
//! update applies to the acknowledged frame `base`: first `shift`, then the replacement rows.
use super::message::check;
use super::{Fatal, limits};
use crate::pane::{Color, effects::Hyperlink, graphics, reading};
use serde::{Deserialize, Serialize};

/// Style id 0 is the default style and is never defined.
pub const DEFAULT_STYLE: u16 = 0;

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
    /// New definitions. They persist for the channel until the next keyframe.
    pub styles: Vec<StyleDefinition>,
    /// New definitions. They persist for the channel until the next keyframe.
    pub links: Vec<LinkDefinition>,
    /// Replacement visible rows, in increasing index order.
    pub rows: Vec<Row>,
    /// Present when the graphics manifest changes.
    pub graphics: Option<Result<graphics::Snapshot, graphics::Error>>,
    pub accessibility_revision: u64,
    /// Fences pointer and wheel coordinates. Increases when the visible grid's identity changes.
    pub generation: u64,
    /// Server-side reading state. Deleted by scrollback.
    pub reading: Option<reading::Snapshot>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Cursor {
    pub position: Option<CursorPosition>,
    pub visible: bool,
    pub blinking: bool,
    pub shape: u8,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CursorPosition {
    pub column: u16,
    pub row: u16,
    pub width: u8,
}

/// The Scrollback above the visible rows. A new `epoch` invalidates every cached row.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct History {
    pub epoch: u64,
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

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Row {
    pub index: u16,
    pub wrapped: bool,
    /// Runs cover the row from column 0. Columns after the last run are blank in the default
    /// style.
    pub runs: Vec<Run>,
}

/// Consecutive cells sharing one style and one link.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Run {
    pub style: u16,
    pub link: Option<u16>,
    pub text: String,
    pub cells: Cells,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Cells {
    /// Each `char` of `text` fills one single-width cell.
    Narrow,
    /// The byte length of each cell's text in order. A wide cell is followed by a zero-length
    /// spacer cell.
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
        if let Some(geometry) = self.geometry {
            check(
                (2..=crate::pane::MAX_COLUMNS).contains(&geometry.columns)
                    && (2..=crate::pane::MAX_ROWS).contains(&geometry.rows),
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
        tables(&self.styles, &self.links, &self.rows, limits::MAX_PAGE_ROWS)
    }
}

fn tables(
    styles: &[StyleDefinition],
    links: &[LinkDefinition],
    rows: &[Row],
    max_rows: u16,
) -> Result<(), Fatal> {
    check(styles.len() <= limits::MAX_STYLES && links.len() <= limits::MAX_LINKS)?;
    check(
        styles
            .iter()
            .all(|style| style.id != DEFAULT_STYLE && usize::from(style.id) <= limits::MAX_STYLES),
    )?;
    check(
        links
            .iter()
            .all(|link| usize::from(link.id) < limits::MAX_LINKS),
    )?;
    for row in rows {
        check(row.index < max_rows)?;
        check(row.runs.len() <= usize::from(crate::pane::MAX_COLUMNS))?;
        for run in &row.runs {
            check(usize::from(run.style) <= limits::MAX_STYLES)?;
            check(
                run.link
                    .is_none_or(|link| usize::from(link) < limits::MAX_LINKS),
            )?;
            if let Cells::Explicit(cells) = &run.cells {
                check(cells.len() <= usize::from(crate::pane::MAX_COLUMNS))?;
                check(cells.iter().map(|len| usize::from(*len)).sum::<usize>() == run.text.len())?;
            }
        }
    }
    Ok(())
}
