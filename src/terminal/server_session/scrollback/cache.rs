//! The Scrollback rows one client holds for one Pane, keyed by absolute history position.
use super::History;
use super::row::CachedRow;
use spaceterm_server::protocol::limits::{
    FIRST_PAGE_ROWS, MAX_CACHED_ROWS, MAX_PAGE_ROWS, MAX_PAGES_IN_FLIGHT,
};
use std::collections::BTreeMap;
use std::ops::Range;

/// One `Operation::ScrollbackPage` this cache asked for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct PageRequest {
    pub(crate) epoch: u64,
    pub(crate) start: u64,
    pub(crate) rows: u16,
}

impl PageRequest {
    fn range(self) -> Range<u64> {
        self.start..self.start + u64::from(self.rows)
    }
}

#[derive(Default)]
pub(super) struct ScrollbackCache {
    /// The Terminal Session whose history the rows belong to.
    session: Option<[u8; 32]>,
    history: History,
    rows: BTreeMap<u64, CachedRow>,
    /// Pages asked for and not answered yet, of any epoch or connection. Every one counts toward
    /// the in-flight limit until its reply settles it; the link answers each exactly once.
    in_flight: Vec<PageRequest>,
    /// Ranges answered for the current history, cached or not. A refused or shortened page is not
    /// asked for again until a frame reports different history.
    settled: Vec<Range<u64>>,
    /// Whether this epoch already asked for a page. The first page is smaller, so the rows above
    /// the screen arrive sooner.
    requested: bool,
    /// Increases whenever cached content changes, so readers can keep derived state.
    revision: u64,
}

impl ScrollbackCache {
    /// Binds the cache to a channel's Terminal Session. Another session's rows are discarded.
    /// Returns whether the session is unchanged.
    pub(super) fn attach(&mut self, session: [u8; 32]) -> bool {
        let same = self.session == Some(session);
        if !same {
            self.clear();
            self.session = Some(session);
        }
        same
    }

    /// Applies a frame's history. A new epoch discards every row; otherwise rows that were
    /// discarded at the top, or that moved back onto the screen, leave the cache. Returns whether
    /// the epoch is unchanged.
    pub(super) fn observe(&mut self, history: History) -> bool {
        let same = history.epoch == self.history.epoch;
        if !same {
            self.clear();
        }
        if history != self.history {
            self.settled.clear();
        }
        self.history = history;
        let retained = history.oldest..history.total;
        let before = self.rows.len();
        self.rows.retain(|position, _| retained.contains(position));
        if self.rows.len() != before {
            self.revision += 1;
        }
        same
    }

    pub(super) fn history(&self) -> History {
        self.history
    }

    pub(super) fn revision(&self) -> u64 {
        self.revision
    }

    pub(super) fn get(&self, position: u64) -> Option<&CachedRow> {
        self.rows.get(&position)
    }

    /// Adds rows the server reported for consecutive positions from `start`, keeping only
    /// retained positions. `focus` is the position the reader is closest to; rows farthest from
    /// it leave first when the cache is full.
    fn insert(&mut self, start: u64, rows: impl IntoIterator<Item = CachedRow>, focus: u64) {
        let retained = self.history.oldest..self.history.total;
        let mut changed = false;
        for (position, row) in (start..).zip(rows) {
            if retained.contains(&position) {
                self.rows.insert(position, row);
                changed = true;
            }
        }
        if changed {
            self.revision += 1;
            self.evict(focus);
        }
    }

    /// Settles a page this cache asked for. Rows of an older epoch are discarded. A page that
    /// starts later than asked shows the server discarded the rows before it.
    pub(super) fn page(
        &mut self,
        request: PageRequest,
        page: Option<(u64, u64, Vec<CachedRow>)>,
        focus: u64,
    ) {
        if let Some(index) = self.in_flight.iter().position(|sent| *sent == request) {
            self.in_flight.swap_remove(index);
        }
        if request.epoch != self.history.epoch {
            return;
        }
        self.settled.push(request.range());
        if let Some((epoch, start, rows)) = page
            && epoch == self.history.epoch
        {
            if start > request.start && start > self.history.oldest {
                self.history.oldest = start.min(self.history.total);
                let oldest = self.history.oldest;
                let before = self.rows.len();
                self.rows.retain(|position, _| *position >= oldest);
                if self.rows.len() != before {
                    self.revision += 1;
                }
            }
            self.insert(start, rows, focus);
        }
    }

    /// Forgets requests that were never sent, so they neither count toward the in-flight limit
    /// nor block asking for their rows again.
    pub(super) fn unsent(&mut self, requests: &[PageRequest]) {
        for request in requests {
            if let Some(index) = self.in_flight.iter().position(|sent| sent == request) {
                self.in_flight.swap_remove(index);
            }
        }
    }

    /// Whether every retained position in `range` is cached.
    pub(super) fn covers(&self, range: Range<u64>) -> bool {
        let range = self.retained(range);
        self.rows.range(range.clone()).count() as u64 == range.end - range.start
    }

    /// Pages to ask for so the retained positions in `wanted` become cached, nearest to `focus`
    /// first, within the in-flight limit.
    pub(super) fn request(&mut self, wanted: Range<u64>, focus: u64) -> Vec<PageRequest> {
        let wanted = self.retained(wanted);
        let mut missing = Vec::new();
        let mut cursor = wanted.start;
        for position in self
            .rows
            .range(wanted.clone())
            .map(|(position, _)| *position)
        {
            if position > cursor {
                missing.push(cursor..position);
            }
            cursor = position + 1;
        }
        if cursor < wanted.end {
            missing.push(cursor..wanted.end);
        }
        let epoch = self.history.epoch;
        let asked = self
            .in_flight
            .iter()
            .filter(|sent| sent.epoch == epoch)
            .map(|sent| sent.range())
            .chain(self.settled.iter().cloned())
            .collect::<Vec<_>>();
        let missing = missing
            .into_iter()
            .flat_map(|run| subtract(run, &asked))
            .collect::<Vec<_>>();
        let distance = |run: &Range<u64>| {
            if focus < run.start {
                run.start - focus
            } else {
                focus.saturating_sub(run.end - 1)
            }
        };
        let mut runs = missing;
        runs.sort_by_key(distance);
        let mut requests = Vec::new();
        for mut run in runs {
            while !run.is_empty() && self.in_flight.len() < MAX_PAGES_IN_FLIGHT {
                let size = u64::from(if self.requested {
                    MAX_PAGE_ROWS
                } else {
                    FIRST_PAGE_ROWS
                });
                // Carve from the end nearest the reader, so the rows it shows arrive first.
                let page = if focus < run.end {
                    run.start..(run.start + size).min(run.end)
                } else {
                    run.end.saturating_sub(size).max(run.start)..run.end
                };
                if page.start == run.start {
                    run.start = page.end;
                } else {
                    run.end = page.start;
                }
                let request = PageRequest {
                    epoch: self.history.epoch,
                    start: page.start,
                    rows: u16::try_from(page.end - page.start).unwrap_or(MAX_PAGE_ROWS),
                };
                self.requested = true;
                self.in_flight.push(request);
                requests.push(request);
            }
        }
        requests
    }

    fn retained(&self, range: Range<u64>) -> Range<u64> {
        let start = range.start.max(self.history.oldest);
        start..range.end.min(self.history.total).max(start)
    }

    fn evict(&mut self, focus: u64) {
        while self.rows.len() > MAX_CACHED_ROWS {
            let (Some(first), Some(last)) = (
                self.rows.first_key_value().map(|(position, _)| *position),
                self.rows.last_key_value().map(|(position, _)| *position),
            ) else {
                return;
            };
            if focus.abs_diff(first) >= focus.abs_diff(last) {
                self.rows.pop_first();
            } else {
                self.rows.pop_last();
            }
        }
    }

    fn clear(&mut self) {
        if !self.rows.is_empty() {
            self.revision += 1;
        }
        self.rows.clear();
        self.settled.clear();
        self.requested = false;
    }
}

/// The parts of `run` that no asked range covers.
fn subtract(run: Range<u64>, asked: &[Range<u64>]) -> Vec<Range<u64>> {
    let mut parts = vec![run];
    for sent in asked {
        let sent = sent.clone();
        parts = parts
            .into_iter()
            .flat_map(|part| {
                if sent.end <= part.start || part.end <= sent.start {
                    return vec![part];
                }
                [part.start..sent.start, sent.end..part.end]
                    .into_iter()
                    .filter(|piece| piece.start < piece.end)
                    .collect()
            })
            .collect();
    }
    parts
}
