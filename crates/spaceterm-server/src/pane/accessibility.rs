//! Demand-driven terminal semantics. These values grant no native or Pane authority.
//!
//! While a client requests semantics for a channel, the server pushes each revision as one or
//! more [`Update`] messages, relative to the revision the client acknowledged last. A revision's
//! messages carry its topology splices first, then its changed rows, and its last message carries
//! the [`Tail`]. [ADR 0062](../../../../docs/adr/0062-push-terminal-semantics-as-binary-updates.md)
//! owns the exchange.
use serde::{Deserialize, Serialize};
use std::ops::Range;

/// The size a server fills one [`Update`] to. [`Packer`] never exceeds it: it splits a splice or
/// a row that does not fit, so the item continues in the revision's next message.
pub const UPDATE_TARGET_BYTES: usize = 32 * 1024;
/// The most text one cell carries: a base codepoint and Ghostty's 64 grapheme suffix codepoints
/// (`grapheme_max_len` in `page.zig`), each at most 4 bytes of UTF-8. Semantic text is never
/// shortened, unlike the cell text of screen frames.
pub const MAX_CELL_TEXT_BYTES: usize = (1 + 64) * 4;
/// The most text one row carries across its fragments: every column at the largest cell text.
pub const MAX_ROW_TEXT_BYTES: usize = super::MAX_COLUMNS as usize * MAX_CELL_TEXT_BYTES;

/// Starts or ends a client's semantic demand for one channel.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum Demand {
    /// Starts a demand generation. `generation` exceeds every earlier start on the channel.
    Start {
        generation: u64,
    },
    End,
}

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

/// One message of one semantic revision.
///
/// The messages of a revision arrive consecutively on their channel and repeat its `header`.
/// Every splice of the revision precedes every row, and only its last message carries `tail`. The
/// client applies the revision when the tail arrives and never publishes a part of it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Update {
    pub header: Header,
    /// Edits to the row topology, applied in order to the base topology, or to an empty one for a
    /// complete document.
    pub splices: Vec<Splice>,
    /// Rows that are new or changed since the base, in increasing line order. A row the base
    /// holds at the same revision is not sent. A row can travel as several fragments.
    pub rows: Vec<RowUpdate>,
    /// Present on the revision's last message only.
    pub tail: Option<Tail>,
}

/// What every message of one revision repeats.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Header {
    /// The [`Demand::Start`] generation this revision answers. The client discards a message of
    /// any other generation without acknowledging it.
    pub demand_generation: u64,
    /// The server's semantic revision. It increases with every revision within a demand
    /// generation.
    pub revision: u64,
    /// The acknowledged revision of the same demand generation this one applies to. `None` is a
    /// complete document, which replaces whatever the client holds.
    pub base: Option<u64>,
    /// Every row of the document belongs to this screen and screen generation.
    pub screen: Screen,
    pub screen_generation: u64,
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

/// The content of the row at `line` of the revision's topology, or one fragment of it.
///
/// A row whose text does not fit a message travels as consecutive fragments, which can cross
/// messages. Each fragment carries whole cells and their spans. Every fragment but the last has
/// `continues`, and the next row update names the same line, revision and soft-wrap flag. The
/// client joins the fragments, so a row reaches the document whole or not at all.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RowUpdate {
    pub line: u32,
    pub revision: u64,
    pub soft_wrapped: bool,
    pub text: String,
    /// `None` when the row has one cell per character, each one column wide, from column 0. A
    /// fragment always has spans.
    pub cells: Option<Vec<CellSpan>>,
    pub continues: bool,
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
    /// Checks the bounds one message can show on its own. The client checks the header against
    /// its demand generation and applied revision, lines against its topology, fragments across
    /// messages, and the order of messages within a revision.
    pub fn validate(&self) -> Result<(), crate::protocol::Fatal> {
        use crate::protocol::message::check;
        check(
            self.header
                .base
                .is_none_or(|base| base < self.header.revision),
        )?;
        for splice in &self.splices {
            check(!splice.insert.is_empty() || splice.remove > 0)?;
            for run in &splice.insert {
                check(run.count > 0 && run.page_row.checked_add(run.count - 1).is_some())?;
            }
        }
        for pair in self.rows.windows(2) {
            check(if pair[0].continues {
                pair[0].line == pair[1].line
            } else {
                pair[0].line < pair[1].line
            })?;
        }
        for row in &self.rows {
            check(row.valid())?;
        }
        if let Some(tail) = &self.tail {
            check(self.rows.last().is_none_or(|row| !row.continues))?;
            check(tail.visible_lines.start <= tail.visible_lines.end)?;
        }
        Ok(())
    }
}

impl RowUpdate {
    /// A whole row whose cells are `cells`, in column order, each naming its columns and the
    /// length of its text in `text`. The cells cover `text` exactly. Keeps the spans only when the
    /// row is not one column per character.
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
            continues: false,
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

    /// The row's text is bounded by its cells: at most one span per column, each at most
    /// [`MAX_CELL_TEXT_BYTES`].
    fn valid(&self) -> bool {
        let Some(spans) = &self.cells else {
            return !self.continues && self.text.chars().count() <= usize::from(super::MAX_COLUMNS);
        };
        let mut offset = 0_usize;
        let mut next_column = 0_u16;
        for span in spans {
            let end = span.column.checked_add(span.width);
            if span.width == 0
                || usize::from(span.bytes) > MAX_CELL_TEXT_BYTES
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

/// Splits one revision into [`Update`] messages whose message bodies, channel included, take at
/// most [`UPDATE_TARGET_BYTES`], in wire order: splices, then rows, then the tail.
///
/// [`Packer::splice`] and [`Packer::row`] return the messages the item they add completes, which
/// is usually none. A splice that does not fit continues as further splices, and a row that does
/// not fit continues as further fragments, in the next message. [`Packer::finish`] returns the
/// last message. A server pulls messages from it one item at a time, so it never holds an
/// encoded document.
pub struct Packer {
    current: Update,
    bytes: usize,
    empty: usize,
}

impl Packer {
    pub fn new(header: Header) -> Self {
        let current = Update {
            header,
            splices: Vec::new(),
            rows: Vec::new(),
            tail: None,
        };
        let widest = Point {
            line: u32::MAX,
            column: u16::MAX,
        };
        let tail = Tail {
            visible_lines: u32::MAX..u32::MAX,
            cursor: Some(widest),
            selection: Some(Selection {
                start: widest,
                end: widest,
                rectangle: true,
            }),
        };
        // Room for the channel the message adds, for the sequence lengths to grow to the largest
        // varint a message needs, and for the largest tail in place of its absent marker.
        let empty = encoded_len(&crate::protocol::Channel(u32::MAX))
            + encoded_len(&current)
            + 2 * VARINT_GROWTH
            + encoded_len(&tail);
        Self {
            current,
            bytes: empty,
            empty,
        }
    }

    /// Adds the splice that removes `remove` rows at line `at` and inserts `insert` there.
    /// Splices go before every row of the revision. When the runs do not fit, the rest of them
    /// continue as splices that remove nothing, at the line after the runs placed so far.
    pub fn splice(
        &mut self,
        at: u32,
        remove: u32,
        insert: impl IntoIterator<Item = Run>,
    ) -> Vec<Update> {
        assert!(
            self.current.rows.is_empty(),
            "a revision's splices precede its rows"
        );
        let mut full = Vec::new();
        let mut piece = Splice {
            at,
            remove,
            insert: Vec::new(),
        };
        let mut bytes = encoded_len(&piece) + VARINT_GROWTH;
        for run in insert {
            let cost = encoded_len(&run);
            if !self.fits(bytes + cost) {
                let next = piece.at + inserted(&piece);
                // A removal that does not fit with no runs waits for the next message.
                if !piece.insert.is_empty() || (piece.remove > 0 && self.fits(bytes)) {
                    self.bytes += bytes;
                    self.current.splices.push(piece);
                    piece = Splice {
                        at: next,
                        remove: 0,
                        insert: Vec::new(),
                    };
                    bytes = encoded_len(&piece) + VARINT_GROWTH;
                }
                full.extend(self.flush());
            }
            bytes += cost;
            piece.insert.push(run);
        }
        if !piece.insert.is_empty() || piece.remove > 0 {
            if !self.fits(bytes) {
                full.extend(self.flush());
            }
            self.bytes += bytes;
            self.current.splices.push(piece);
        }
        full
    }

    /// Adds a whole row. Rows go in increasing line order. A row that does not fit what is left
    /// of the message starts the next one, and a row that does not fit an empty message travels
    /// as fragments that fill each message.
    pub fn row(&mut self, row: RowUpdate) -> Vec<Update> {
        assert!(!row.continues, "a server adds whole rows");
        let mut full = Vec::new();
        let cost = encoded_len(&row);
        if !self.fits(cost) && cost <= UPDATE_TARGET_BYTES - self.empty {
            full.extend(self.flush());
        }
        if self.fits(cost) {
            self.bytes += cost;
            self.current.rows.push(row);
            return full;
        }
        // Only a row with spans can exceed a message: a simple row is at most one 4-byte character
        // per column.
        let RowUpdate {
            line,
            revision,
            soft_wrapped,
            text,
            cells,
            ..
        } = row;
        let spans = cells.expect("a simple row fits an empty message");
        // Spans that do not cover the text leave it empty, so validation refuses the fragment.
        let fragment = |text: Option<&str>, cells: &[CellSpan], continues| RowUpdate {
            line,
            revision,
            soft_wrapped,
            text: text.unwrap_or_default().to_owned(),
            cells: Some(cells.to_vec()),
            continues,
        };
        let overhead = encoded_len(&fragment(None, &[], true)) + 2 * VARINT_GROWTH;
        let (mut first, mut start, mut end, mut bytes) = (0, 0, 0, overhead);
        for (index, span) in spans.iter().enumerate() {
            let cost = encoded_len(span) + usize::from(span.bytes);
            if !self.fits(bytes + cost) {
                if index > first {
                    self.bytes += bytes;
                    self.current.rows.push(fragment(
                        text.get(start..end),
                        &spans[first..index],
                        true,
                    ));
                }
                full.extend(self.flush());
                (first, start, bytes) = (index, end, overhead);
            }
            end += usize::from(span.bytes);
            bytes += cost;
        }
        self.bytes += bytes;
        self.current
            .rows
            .push(fragment(text.get(start..), &spans[first..], false));
        full
    }

    pub fn finish(mut self, tail: Tail) -> Update {
        self.current.tail = Some(tail);
        self.current
    }

    fn fits(&self, bytes: usize) -> bool {
        self.bytes + bytes <= UPDATE_TARGET_BYTES
    }

    /// The message so far, if it holds anything, and an empty one in its place.
    fn flush(&mut self) -> Option<Update> {
        let started = !self.current.splices.is_empty() || !self.current.rows.is_empty();
        started.then(|| {
            self.bytes = self.empty;
            Update {
                splices: std::mem::take(&mut self.current.splices),
                rows: std::mem::take(&mut self.current.rows),
                ..self.current.clone()
            }
        })
    }
}

/// How far a sequence or string length's varint can grow within one message: from one byte to
/// three, which covers lengths below 2^21.
const VARINT_GROWTH: usize = 2;

fn inserted(splice: &Splice) -> u32 {
    splice.insert.iter().map(|run| u32::from(run.count)).sum()
}

fn encoded_len(value: &impl Serialize) -> usize {
    postcard::serialize_with_flavor(value, postcard::ser_flavors::Size::default())
        .unwrap_or(usize::MAX / 2)
}

#[cfg(test)]
mod tests;
