//! Terminal Find over cached history and the visible screen.
//!
//! Matching follows the server's rules from before this cache: blank cells between text read as
//! spaces, trailing blanks of a row are dropped, soft-wrapped rows join, ASCII letters match in
//! either case and matches never overlap. A match never crosses a hard line break, so complete
//! history lines keep their matches until the cache drops one of their rows. A row that is not
//! cached yet ends a line; its matches appear once its page arrives.
use super::{Lines, Point, row::RowRef};
use crate::terminal::{
    FindDirection, FindHighlightSpan, FindQueryGeneration, TerminalFindSnapshot,
};
use std::collections::BTreeMap;
use std::sync::Arc;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct Match {
    pub(super) start: Point,
    pub(super) end: Point,
    /// Cells the last character covers.
    pub(super) end_width: u8,
}

#[derive(Default)]
pub(super) struct LocalFind {
    query: Option<(FindQueryGeneration, String)>,
    /// Matches of complete history lines, by each line's first line number, with its row count.
    lines: BTreeMap<u64, (u64, Vec<Match>)>,
    current: Option<Match>,
}

impl LocalFind {
    pub(super) fn active(&self) -> bool {
        self.query.is_some()
    }

    pub(super) fn set_query(&mut self, generation: FindQueryGeneration, query: String) {
        self.query = Some((generation, query));
        self.lines.clear();
        self.current = None;
    }

    /// Ends Find unless a newer query replaced the one `generation` names.
    pub(super) fn end(&mut self, generation: FindQueryGeneration) -> bool {
        if self
            .query
            .as_ref()
            .is_none_or(|(current, _)| generation < *current)
        {
            return false;
        }
        self.query = None;
        self.lines.clear();
        self.current = None;
        true
    }

    /// Forgets matches of a history epoch that ended.
    pub(super) fn invalidate(&mut self) {
        self.lines.clear();
        self.current = None;
    }

    /// Selects the next or previous match and returns the view's new top line, which shows it.
    pub(super) fn navigate(
        &mut self,
        generation: FindQueryGeneration,
        direction: FindDirection,
        lines: Lines<'_>,
        top: u64,
        rows: u16,
    ) -> Option<u64> {
        if self
            .query
            .as_ref()
            .is_none_or(|(current, _)| *current != generation)
        {
            return None;
        }
        let matches = self.matches(lines);
        if matches.is_empty() {
            return None;
        }
        let visible = u64::from(rows.max(1));
        let bottom = top + visible - 1;
        let current = self
            .current
            .and_then(|current| matches.iter().position(|found| *found == current));
        let index = match (current, direction) {
            (Some(index), FindDirection::Next) => (index + 1) % matches.len(),
            (Some(index), FindDirection::Previous) => {
                index.checked_sub(1).unwrap_or(matches.len() - 1)
            }
            (None, FindDirection::Next) => matches
                .iter()
                .position(|found| found.end.line >= top)
                .unwrap_or(0),
            (None, FindDirection::Previous) => matches
                .iter()
                .rposition(|found| found.start.line <= bottom)
                .unwrap_or(matches.len() - 1),
        };
        let selected = matches[index];
        self.current = Some(selected);
        Some(if selected.start.line < top {
            selected.start.line
        } else if selected.end.line > bottom {
            if selected.end.line - selected.start.line >= visible {
                selected.start.line
            } else {
                selected.end.line + 1 - visible
            }
        } else {
            top
        })
    }

    pub(super) fn snapshot(
        &mut self,
        lines: Lines<'_>,
        top: u64,
        rows: u16,
    ) -> Option<Arc<TerminalFindSnapshot>> {
        let generation = self.query.as_ref()?.0;
        let matches = self.matches(lines);
        let current = self
            .current
            .and_then(|current| matches.iter().position(|found| *found == current));
        if current.is_none() {
            self.current = None;
        }
        let bottom = top + u64::from(rows.max(1)) - 1;
        let mut spans = Vec::new();
        for (index, found) in matches.iter().enumerate() {
            if found.end.line < top || found.start.line > bottom {
                continue;
            }
            for line in found.start.line.max(top)..=found.end.line.min(bottom) {
                let columns = lines.get(line).map_or(1, RowRef::columns);
                spans.push(FindHighlightSpan {
                    row: u16::try_from(line - top).unwrap_or(u16::MAX),
                    start_column: if line == found.start.line {
                        found.start.column
                    } else {
                        0
                    },
                    end_column: if line == found.end.line {
                        found
                            .end
                            .column
                            .saturating_add(u16::from(found.end_width.saturating_sub(1)))
                    } else {
                        columns.saturating_sub(1)
                    },
                    current: current == Some(index),
                });
            }
        }
        Some(Arc::new(TerminalFindSnapshot {
            generation,
            total_matches: matches.len(),
            current_match: current.map(|index| index + 1),
            visible_spans: spans.into(),
        }))
    }

    /// Every match in readable lines, in order.
    fn matches(&mut self, lines: Lines<'_>) -> Vec<Match> {
        let Some((_, query)) = &self.query else {
            return Vec::new();
        };
        let needle = query.as_bytes();
        let range = lines.range();
        let total = lines.total();
        // Complete history lines end before the screen; the line the screen starts in may begin
        // in history, so it is matched with the screen.
        let mut boundary = total.max(range.start);
        while boundary > range.start && lines.get(boundary - 1).is_some_and(|row| row.wrapped()) {
            boundary -= 1;
        }
        // A kept line is still whole: every row cached, and no row above it continues into it.
        self.lines.retain(|start, (rows, _)| {
            *start >= range.start
                && *start + *rows <= boundary
                && (*start..*start + *rows).all(|line| lines.get(line).is_some())
                && start
                    .checked_sub(1)
                    .and_then(|above| lines.get(above))
                    .is_none_or(|above| !above.wrapped())
        });
        let mut matches = Vec::new();
        let mut line = range.start;
        while line < boundary {
            if let Some((rows, found)) = self.lines.get(&line) {
                matches.extend_from_slice(found);
                line += rows;
                continue;
            }
            if lines.get(line).is_none() {
                line += 1;
                continue;
            }
            let end = logical_end(lines, line, boundary);
            let found = Corpus::new(lines, line..end).matches(needle);
            matches.extend_from_slice(&found);
            // A line cut short by a missing row may continue once that row arrives.
            if lines.get(end - 1).is_some_and(|row| !row.wrapped()) {
                self.lines.insert(line, (end - line, found));
            }
            line = end;
        }
        let mut line = boundary;
        while line < range.end {
            if lines.get(line).is_none() {
                line += 1;
                continue;
            }
            let end = logical_end(lines, line, range.end);
            matches.extend(Corpus::new(lines, line..end).matches(needle));
            line = end;
        }
        matches
    }
}

/// The line after the soft-wrapped run that starts at `line`, stopping at a missing row.
fn logical_end(lines: Lines<'_>, mut line: u64, limit: u64) -> u64 {
    while line < limit {
        let Some(row) = lines.get(line) else {
            return line;
        };
        line += 1;
        if !row.wrapped() {
            break;
        }
    }
    line
}

/// The text of one logical line, with the cell of every byte.
struct Corpus {
    bytes: Vec<u8>,
    cells: Vec<(Point, u8)>,
}

impl Corpus {
    fn new(lines: Lines<'_>, range: std::ops::Range<u64>) -> Self {
        let mut corpus = Self {
            bytes: Vec::new(),
            cells: Vec::new(),
        };
        for line in range {
            let Some(row) = lines.get(line) else {
                break;
            };
            let mut pending = Vec::new();
            for column in 0..row.columns() {
                if row.spacer_tail(column) {
                    continue;
                }
                let point = Point { line, column };
                if !row.has_text(column) {
                    pending.push(point);
                    continue;
                }
                for blank in pending.drain(..) {
                    corpus.push(" ", blank, 1);
                }
                let width = if row.spacer_tail(column + 1) { 2 } else { 1 };
                corpus.push(row.text(column), point, width);
            }
        }
        corpus
    }

    fn push(&mut self, text: &str, point: Point, width: u8) {
        self.bytes.extend_from_slice(text.as_bytes());
        self.cells
            .extend(std::iter::repeat_n((point, width), text.len()));
    }

    fn matches(&self, needle: &[u8]) -> Vec<Match> {
        if needle.is_empty() || needle.contains(&b'\n') || needle.len() > self.bytes.len() {
            return Vec::new();
        }
        let mut matches = Vec::new();
        let mut offset = 0;
        while offset + needle.len() <= self.bytes.len() {
            let end = offset + needle.len();
            if self.bytes[offset..end].eq_ignore_ascii_case(needle) {
                let (start, _) = self.cells[offset];
                let (end_point, end_width) = self.cells[end - 1];
                matches.push(Match {
                    start,
                    end: end_point,
                    end_width,
                });
                offset = end;
            } else {
                offset += 1;
            }
        }
        matches
    }
}
