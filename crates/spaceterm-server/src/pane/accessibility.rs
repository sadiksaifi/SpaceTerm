//! Demand-driven terminal semantics. These values grant no native or Pane authority.
//!
//! While a client requests semantics for a channel, the server pushes each revision as one or
//! more [`Update`] messages, relative to the revision the client acknowledged last. A revision's
//! messages carry its topology splices first, then its changed rows, and its last message carries
//! the [`Tail`]. [ADR 0062](../../../../docs/adr/0062-push-terminal-semantics-as-binary-updates.md)
//! owns the exchange.
use serde::{Deserialize, Serialize};
use std::ops::Range;

pub const CHUNK_BYTES: usize = 32 * 1024;
pub const MAX_BYTES: usize = 128 * 1024 * 1024;

/// The size a server fills one [`Update`] to before it starts the revision's next message. A row
/// larger than what is left starts the next message, and a row larger than this travels alone.
pub const UPDATE_TARGET_BYTES: usize = 32 * 1024;
/// The most text one row carries: every column at the largest cell text. Servers shorten a longer
/// cell at a character boundary, as screen frames do.
pub const MAX_ROW_TEXT_BYTES: usize =
    super::MAX_COLUMNS as usize * crate::protocol::limits::MAX_CELL_BYTES;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum Screen {
    Primary,
    Alternate,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct RowId {
    pub screen: Screen,
    pub screen_generation: u64,
    pub node_serial: u64,
    pub page_row: u16,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Cell {
    pub text: String,
    pub column: u16,
    pub width_cells: u16,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CellRef {
    pub row: RowId,
    pub row_revision: u64,
    pub column: u16,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SelectionRefs {
    pub start: CellRef,
    pub end: CellRef,
    pub rectangle: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Row {
    pub id: RowId,
    pub revision: u64,
    pub soft_wrapped: bool,
    pub cells: Vec<Cell>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Snapshot {
    pub revision: u64,
    pub screen: Screen,
    pub screen_generation: u64,
    pub topology: Vec<RowId>,
    pub rows: Vec<Row>,
    pub visible_lines: Range<usize>,
    pub cursor: Option<CellRef>,
    pub selection: Option<SelectionRefs>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Chunk {
    pub revision: u64,
    pub offset: usize,
    pub total: usize,
    pub bytes: Vec<u8>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum Error {
    Pending,
    Stale,
    TooLarge,
}

/// One message of one semantic revision.
///
/// The messages of a revision arrive consecutively on their channel and repeat its `revision`,
/// `base`, `screen` and `screen_generation`. Every splice of the revision precedes every row, and
/// only its last message carries `tail`. The client applies the revision when the tail arrives.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Update {
    /// The server's semantic revision. It increases with every revision on a channel.
    pub revision: u64,
    /// The acknowledged revision this one applies to. `None` is a complete document, which
    /// replaces whatever the client holds.
    pub base: Option<u64>,
    /// Every row of the document belongs to this screen and screen generation.
    pub screen: Screen,
    pub screen_generation: u64,
    /// Edits to the row topology, applied in order to the base topology, or to an empty one for a
    /// complete document.
    pub splices: Vec<Splice>,
    /// Rows that are new or changed since the base, in increasing line order. A row the base
    /// holds at the same revision is not sent.
    pub rows: Vec<RowUpdate>,
    /// Present on the revision's last message only.
    pub tail: Option<Tail>,
}

/// Removes `remove` rows at line `at` of the topology, then inserts the rows of `insert` there.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Splice {
    pub at: u32,
    pub remove: u32,
    pub insert: Vec<Run>,
}

/// `count` consecutive page rows of one page node, starting at `page_row`, in the update's screen
/// and screen generation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Run {
    pub node_serial: u64,
    pub page_row: u16,
    pub count: u16,
}

/// The content of the row at `line` of the revision's topology.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RowUpdate {
    pub line: u32,
    pub revision: u64,
    pub soft_wrapped: bool,
    pub text: String,
    /// `None` when the row has one cell per character, each one column wide, from column 0.
    pub cells: Option<Vec<CellSpan>>,
}

/// One cell of a row: its columns and the next `bytes` bytes of the row's text.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CellSpan {
    pub column: u16,
    pub width: u16,
    pub bytes: u16,
}

/// What the client presents with the revision. Lines index the revision's topology.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tail {
    pub visible_lines: Range<u32>,
    pub cursor: Option<Point>,
    pub selection: Option<Selection>,
}

/// A cell named by its line in the topology and its column. The client derives the cell
/// reference from the row at that line and its revision.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Point {
    pub line: u32,
    pub column: u16,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Selection {
    pub start: Point,
    pub end: Point,
    pub rectangle: bool,
}

impl Update {
    /// Checks the bounds one message can show on its own. The client checks lines against its
    /// topology and the order of messages within a revision.
    pub fn validate(&self) -> Result<(), crate::protocol::Fatal> {
        use crate::protocol::message::check;
        check(self.base.is_none_or(|base| base < self.revision))?;
        for splice in &self.splices {
            check(!splice.insert.is_empty() || splice.remove > 0)?;
            for run in &splice.insert {
                check(run.count > 0 && run.page_row.checked_add(run.count - 1).is_some())?;
            }
        }
        for pair in self.rows.windows(2) {
            check(pair[0].line < pair[1].line)?;
        }
        for row in &self.rows {
            check(row.valid())?;
        }
        if let Some(tail) = &self.tail {
            check(tail.visible_lines.start <= tail.visible_lines.end)?;
        }
        Ok(())
    }
}

impl RowUpdate {
    /// A row whose cells are `cells`, in column order, each naming its columns and the length of
    /// its text in `text`. The cells cover `text` exactly. Keeps the spans only when the row is not
    /// one column per character.
    pub fn new(
        line: u32,
        revision: u64,
        soft_wrapped: bool,
        text: String,
        cells: impl IntoIterator<Item = CellSpan>,
    ) -> Self {
        let cells = spans(&text, cells);
        Self {
            line,
            revision,
            soft_wrapped,
            text,
            cells,
        }
    }

    /// Each cell's columns and its byte range in `text`.
    pub fn cells(&self) -> impl Iterator<Item = (Range<u16>, Range<usize>)> + '_ {
        let simple = self
            .cells
            .is_none()
            .then(|| self.text.char_indices().zip(0_u16..))
            .into_iter()
            .flatten()
            .map(|((start, character), column)| {
                (column..column + 1, start..start + character.len_utf8())
            });
        let mut offset = 0;
        let spans = self.cells.iter().flatten().map(move |span| {
            let start = offset;
            offset += usize::from(span.bytes);
            (span.column..span.column + span.width, start..offset)
        });
        simple.chain(spans)
    }

    fn valid(&self) -> bool {
        if self.text.len() > MAX_ROW_TEXT_BYTES {
            return false;
        }
        let Some(spans) = &self.cells else {
            return self.text.chars().count() <= usize::from(super::MAX_COLUMNS);
        };
        let mut offset = 0_usize;
        let mut next_column = 0_u16;
        for span in spans {
            let end = span.column.checked_add(span.width);
            if span.width == 0
                || span.column < next_column
                || end.is_none_or(|end| end > super::MAX_COLUMNS)
            {
                return false;
            }
            offset += usize::from(span.bytes);
            if !self.text.is_char_boundary(offset) {
                return false;
            }
            next_column = end.unwrap_or(u16::MAX);
        }
        offset == self.text.len()
    }
}

/// `None` when `cells` are one column per character of `text`, from column 0. Allocates only for
/// a row that needs its spans.
fn spans(text: &str, cells: impl IntoIterator<Item = CellSpan>) -> Option<Vec<CellSpan>> {
    let simple_prefix = |count: u32| {
        text.chars()
            .zip(0..count)
            .map(|(character, column)| CellSpan {
                column: column as u16,
                width: 1,
                bytes: character.len_utf8() as u16,
            })
            .collect::<Vec<_>>()
    };
    let mut characters = text.chars();
    let mut count = 0_u32;
    let mut cells = cells.into_iter();
    while let Some(cell) = cells.next() {
        let simple = characters.next().is_some_and(|character| {
            u32::from(cell.column) == count
                && cell.width == 1
                && usize::from(cell.bytes) == character.len_utf8()
        });
        if !simple {
            let mut kept = simple_prefix(count);
            kept.push(cell);
            kept.extend(cells);
            return Some(kept);
        }
        count += 1;
    }
    // Text the cells leave uncovered keeps its spans, so validation refuses the row instead of
    // the client inventing cells for it.
    characters.next().map(|_| simple_prefix(count))
}

/// Splits one revision into [`Update`] messages near [`UPDATE_TARGET_BYTES`], in wire order:
/// splices, then rows, then the tail.
///
/// [`Packer::splice`] and [`Packer::row`] return the message so far when the item they add does
/// not fit in it; that item starts the next message. [`Packer::finish`] returns the last message.
/// A server pulls messages from it one at a time, so it never holds an encoded document.
pub struct Packer {
    current: Update,
    bytes: usize,
    empty: usize,
}

impl Packer {
    pub fn new(revision: u64, base: Option<u64>, screen: Screen, screen_generation: u64) -> Self {
        let current = Update {
            revision,
            base,
            screen,
            screen_generation,
            splices: Vec::new(),
            rows: Vec::new(),
            tail: None,
        };
        // Room for the sequence lengths to grow from one byte to the largest varint.
        let empty = encoded_len(&current) + 2 * 4;
        Self {
            current,
            bytes: empty,
            empty,
        }
    }

    /// Adds a splice. Splices go before every row of the revision.
    pub fn splice(&mut self, splice: Splice) -> Option<Update> {
        assert!(
            self.current.rows.is_empty(),
            "a revision's splices precede its rows"
        );
        let full = self.make_room(encoded_len(&splice));
        self.current.splices.push(splice);
        full
    }

    /// Adds a row. Rows go in increasing line order.
    pub fn row(&mut self, row: RowUpdate) -> Option<Update> {
        let full = self.make_room(encoded_len(&row));
        self.current.rows.push(row);
        full
    }

    pub fn finish(mut self, tail: Tail) -> Update {
        self.current.tail = Some(tail);
        self.current
    }

    fn make_room(&mut self, bytes: usize) -> Option<Update> {
        let started = self.bytes > self.empty;
        let full = (started && self.bytes + bytes > UPDATE_TARGET_BYTES).then(|| {
            self.bytes = self.empty;
            Update {
                splices: std::mem::take(&mut self.current.splices),
                rows: std::mem::take(&mut self.current.rows),
                ..self.current.clone()
            }
        });
        self.bytes += bytes;
        full
    }
}

fn encoded_len(value: &impl Serialize) -> usize {
    postcard::serialize_with_flavor(value, postcard::ser_flavors::Size::default())
        .unwrap_or(usize::MAX / 2)
}

#[cfg(test)]
mod tests;
