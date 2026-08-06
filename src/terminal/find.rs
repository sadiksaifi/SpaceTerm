use std::sync::Arc;

use libghostty_vt::screen::{CellWide, Screen, TrackedGridRef};
use libghostty_vt::terminal::{Point, PointCoordinate, PointSpace, ScrollViewport};
use libghostty_vt::{Error, Terminal};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum FindDirection {
    Next,
    Previous,
}

#[derive(Clone, Copy, Debug, Default, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) struct FindQueryGeneration(u64);

impl FindQueryGeneration {
    pub(crate) fn next(self) -> Self {
        Self(self.0.saturating_add(1))
    }

    #[cfg(test)]
    pub(crate) const fn test(value: u64) -> Self {
        Self(value)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct FindHighlightSpan {
    pub(crate) row: u16,
    pub(crate) start_column: u16,
    pub(crate) end_column: u16,
    pub(crate) current: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct TerminalFindSnapshot {
    pub(crate) generation: FindQueryGeneration,
    pub(crate) total_matches: usize,
    pub(crate) current_match: Option<usize>,
    pub(crate) visible_spans: Arc<[FindHighlightSpan]>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct FindMatch {
    start: PointCoordinate,
    end: PointCoordinate,
    end_width: u8,
}

struct CurrentMatch {
    start: TrackedGridRef,
    end: TrackedGridRef,
}

#[derive(Default)]
pub(super) struct TerminalFindState {
    query: Option<(FindQueryGeneration, String)>,
    matches: Vec<FindMatch>,
    current: Option<CurrentMatch>,
    current_index: Option<usize>,
    active_screen: Option<Screen>,
    dirty: bool,
    changed: bool,
}

impl TerminalFindState {
    pub(super) fn set_query(&mut self, generation: FindQueryGeneration, query: String) {
        self.query = Some((generation, query));
        self.current = None;
        self.current_index = None;
        self.dirty = true;
        self.changed = true;
    }

    pub(super) fn end(&mut self, generation: FindQueryGeneration) {
        if self
            .query
            .as_ref()
            .is_some_and(|(current, _)| generation < *current)
        {
            return;
        }
        self.query = None;
        self.matches.clear();
        self.current = None;
        self.current_index = None;
        self.active_screen = None;
        self.dirty = false;
        self.changed = true;
    }

    pub(super) fn invalidate(&mut self) {
        if self.query.is_some() {
            self.dirty = true;
        }
    }

    pub(super) fn is_changed(&self) -> bool {
        self.changed
    }

    pub(super) fn mark_published(&mut self) {
        self.changed = false;
    }

    pub(super) fn refresh(&mut self, terminal: &Terminal<'_, '_>, cols: u16) -> Result<(), Error> {
        if self.query.is_none() || !self.dirty {
            return Ok(());
        }

        let active_screen = terminal.active_screen()?;
        let preserved = if self.active_screen == Some(active_screen) {
            self.current_points()?
        } else {
            None
        };
        self.current = None;
        let query = self
            .query
            .as_ref()
            .map(|(_, query)| query.as_str())
            .unwrap_or_default();
        let matches = if query.is_empty() {
            Vec::new()
        } else {
            SearchCorpus::from_terminal(terminal, cols)?.literal_matches(query)
        };
        let current_index = preserved
            .filter(|_| !query.is_empty())
            .and_then(|range| matches.iter().position(|candidate| *candidate == range));

        self.current = current_index
            .and_then(|index| matches.get(index))
            .map(|found| {
                Ok(CurrentMatch {
                    start: terminal.track_grid_ref(Point::Screen(found.start))?,
                    end: terminal.track_grid_ref(Point::Screen(found.end))?,
                })
            })
            .transpose()?;
        self.current_index = current_index;
        self.matches = matches;
        self.active_screen = Some(active_screen);
        self.dirty = false;
        self.changed = true;
        Ok(())
    }

    pub(super) fn navigate(
        &mut self,
        terminal: &mut Terminal<'_, '_>,
        cols: u16,
        generation: FindQueryGeneration,
        direction: FindDirection,
    ) -> Result<bool, Error> {
        if self
            .query
            .as_ref()
            .is_none_or(|(current, _)| *current != generation)
        {
            return Ok(false);
        }
        self.refresh(terminal, cols)?;
        if self.matches.is_empty() {
            return Ok(false);
        }

        let scrollbar = terminal.scrollbar()?;
        let viewport_top = u32::try_from(scrollbar.offset).unwrap_or(u32::MAX);
        let viewport_bottom = viewport_top
            .saturating_add(u32::try_from(scrollbar.len).unwrap_or(u32::MAX))
            .saturating_sub(1);
        let index = match (self.current_index, direction) {
            (Some(index), FindDirection::Next) => (index + 1) % self.matches.len(),
            (Some(index), FindDirection::Previous) => {
                index.checked_sub(1).unwrap_or(self.matches.len() - 1)
            }
            (None, FindDirection::Next) => self
                .matches
                .iter()
                .position(|candidate| candidate.end.y >= viewport_top)
                .unwrap_or(0),
            (None, FindDirection::Previous) => self
                .matches
                .iter()
                .rposition(|candidate| candidate.start.y <= viewport_bottom)
                .unwrap_or(self.matches.len() - 1),
        };
        let selected = self.matches[index];
        self.current = Some(CurrentMatch {
            start: terminal.track_grid_ref(Point::Screen(selected.start))?,
            end: terminal.track_grid_ref(Point::Screen(selected.end))?,
        });
        self.current_index = Some(index);

        let visible_rows = u32::try_from(scrollbar.len).unwrap_or(u32::MAX).max(1);
        let target_top = if selected.start.y < viewport_top {
            selected.start.y
        } else if selected.end.y > viewport_bottom {
            if selected.end.y.saturating_sub(selected.start.y) >= visible_rows {
                selected.start.y
            } else {
                selected
                    .end
                    .y
                    .saturating_add(1)
                    .saturating_sub(visible_rows)
            }
        } else {
            viewport_top
        };
        if target_top != viewport_top {
            terminal.scroll_viewport(ScrollViewport::Row(
                usize::try_from(target_top).unwrap_or(usize::MAX),
            ));
        }
        self.changed = true;
        Ok(true)
    }

    pub(super) fn snapshot(
        &self,
        cols: u16,
        viewport_offset: u64,
        visible_rows: u64,
    ) -> Option<Arc<TerminalFindSnapshot>> {
        let (generation, _) = self.query.as_ref()?;
        let top = u32::try_from(viewport_offset).unwrap_or(u32::MAX);
        let bottom = top
            .saturating_add(u32::try_from(visible_rows).unwrap_or(u32::MAX))
            .saturating_sub(1);
        let mut spans = Vec::new();

        for (index, found) in self.matches.iter().enumerate() {
            if found.end.y < top || found.start.y > bottom {
                continue;
            }
            let first_row = found.start.y.max(top);
            let last_row = found.end.y.min(bottom);
            for screen_row in first_row..=last_row {
                let start_column = if screen_row == found.start.y {
                    found.start.x
                } else {
                    0
                };
                let end_column = if screen_row == found.end.y {
                    found
                        .end
                        .x
                        .saturating_add(u16::from(found.end_width.saturating_sub(1)))
                } else {
                    cols.saturating_sub(1)
                };
                spans.push(FindHighlightSpan {
                    row: u16::try_from(screen_row.saturating_sub(top)).unwrap_or(u16::MAX),
                    start_column,
                    end_column,
                    current: self.current_index == Some(index),
                });
            }
        }

        Some(Arc::new(TerminalFindSnapshot {
            generation: *generation,
            total_matches: self.matches.len(),
            current_match: self.current_index.map(|index| index + 1),
            visible_spans: Arc::from(spans),
        }))
    }

    fn current_points(&self) -> Result<Option<FindMatch>, Error> {
        let Some(current) = &self.current else {
            return Ok(None);
        };
        let Some(start) = current.start.point(PointSpace::Screen)? else {
            return Ok(None);
        };
        let Some(end) = current.end.point(PointSpace::Screen)? else {
            return Ok(None);
        };
        Ok(Some(FindMatch {
            start,
            end,
            end_width: self
                .current_index
                .and_then(|index| self.matches.get(index))
                .map_or(1, |found| found.end_width),
        }))
    }
}

#[derive(Default)]
struct SearchCorpus {
    bytes: Vec<u8>,
    cells: Vec<Option<CellMapping>>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct CellMapping {
    point: PointCoordinate,
    width: u8,
}

impl SearchCorpus {
    fn from_terminal(terminal: &Terminal<'_, '_>, cols: u16) -> Result<Self, Error> {
        let scrollbar = terminal.scrollbar()?;
        let total_rows = u32::try_from(scrollbar.total).unwrap_or(u32::MAX);
        let mut corpus = Self::default();
        let mut graphemes = vec!['\0'; 8];

        for y in 0..total_rows {
            let row_ref = terminal.grid_ref(Point::Screen(PointCoordinate { x: 0, y }))?;
            let wrapped = row_ref.row()?.is_wrapped()?;
            let mut pending_spaces = Vec::new();
            for x in 0..cols {
                let point = PointCoordinate { x, y };
                let reference = terminal.grid_ref(Point::Screen(point))?;
                let cell = reference.cell()?;
                if cell.wide()? == CellWide::SpacerTail {
                    continue;
                }
                if !cell.has_text()? {
                    pending_spaces.push(point);
                    continue;
                }

                for pending in pending_spaces.drain(..) {
                    corpus.push_grapheme(" ", pending, 1);
                }
                let count = match reference.graphemes(&mut graphemes) {
                    Ok(count) => count,
                    Err(Error::OutOfSpace { required }) => {
                        graphemes.resize(required, '\0');
                        reference.graphemes(&mut graphemes)?
                    }
                    Err(error) => return Err(error),
                };
                let text = graphemes[..count].iter().collect::<String>();
                corpus.push_grapheme(
                    &text,
                    point,
                    if cell.wide()? == CellWide::Wide { 2 } else { 1 },
                );
            }

            if !wrapped && y + 1 < total_rows {
                corpus.bytes.push(b'\n');
                corpus.cells.push(None);
            }
        }
        Ok(corpus)
    }

    fn push_grapheme(&mut self, text: &str, point: PointCoordinate, width: u8) {
        self.bytes.extend_from_slice(text.as_bytes());
        self.cells.extend(std::iter::repeat_n(
            Some(CellMapping { point, width }),
            text.len(),
        ));
    }

    fn literal_matches(&self, query: &str) -> Vec<FindMatch> {
        let needle = query.as_bytes();
        if needle.is_empty() || needle.contains(&b'\n') || needle.len() > self.bytes.len() {
            return Vec::new();
        }

        let mut matches = Vec::new();
        let mut offset = 0;
        while offset + needle.len() <= self.bytes.len() {
            let end_offset = offset + needle.len();
            if self.bytes[offset..end_offset].eq_ignore_ascii_case(needle)
                && let (Some(start), Some(end)) = (self.cells[offset], self.cells[end_offset - 1])
            {
                matches.push(FindMatch {
                    start: start.point,
                    end: end.point,
                    end_width: end.width,
                });
                offset = end_offset;
            } else {
                offset += 1;
            }
        }
        matches
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn corpus(parts: &[(&str, PointCoordinate)]) -> SearchCorpus {
        let mut corpus = SearchCorpus::default();
        for (text, point) in parts {
            corpus.push_grapheme(text, *point, 1);
        }
        corpus
    }

    #[test]
    fn literal_matching_folds_ascii_case() {
        let corpus = corpus(&[("SpaceTerm", PointCoordinate { x: 0, y: 0 })]);

        assert_eq!(corpus.literal_matches("spACETerm").len(), 1);
    }

    #[test]
    fn literal_matching_keeps_non_ascii_exact() {
        let corpus = corpus(&[("É", PointCoordinate { x: 0, y: 0 })]);

        assert!(corpus.literal_matches("é").is_empty());
    }

    #[test]
    fn byte_mapping_returns_grapheme_head_cells() {
        let corpus = corpus(&[
            ("e\u{301}", PointCoordinate { x: 2, y: 4 }),
            ("🙂", PointCoordinate { x: 3, y: 4 }),
        ]);

        assert_eq!(
            corpus.literal_matches("\u{301}🙂"),
            vec![FindMatch {
                start: PointCoordinate { x: 2, y: 4 },
                end: PointCoordinate { x: 3, y: 4 },
                end_width: 1,
            }]
        );
    }

    #[test]
    fn literal_matching_is_non_overlapping() {
        let corpus = corpus(&[("aaaa", PointCoordinate { x: 0, y: 0 })]);

        assert_eq!(corpus.literal_matches("aa").len(), 2);
    }

    #[test]
    fn empty_query_has_no_matches() {
        let corpus = corpus(&[("SpaceTerm", PointCoordinate { x: 0, y: 0 })]);

        assert!(corpus.literal_matches("").is_empty());
    }
}
