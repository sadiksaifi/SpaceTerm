//! The client's Scrollback Cache for one Pane and the reading that runs over it.
//!
//! The server pushes the visible screen and reports its history. This client keeps history rows
//! in its own cache and moves its own view through them, so wheel scrolling, Scroll Commands,
//! Selection, Terminal Find and copy cost no round trip within cached rows, and another client's
//! view never moves (ADR 0041). Missing rows arrive as background `ScrollbackPage` replies.
//!
//! Every row has an absolute line number within one history epoch. History position `p` is line
//! `p`; visible row `r` is line `total + r`. When output scrolls, a visible row becomes history
//! under the same line number, so Selection, Find matches and a scrolled view stay on their
//! content.
//!
//! The server pushes only the screen, so a scrolled view shows images placed on rows still on the
//! screen and none placed on history rows. Frames carry no shell-integration prompt marks, so a
//! triple-click selects whole lines without stopping at a prompt. Rows that scroll off the screen
//! reach the cache through pages, never from frames, whose rows may have changed before they
//! scrolled; until their page arrives, a scrolled-back view shows them as the last frame did.
mod cache;
mod find;
mod row;
mod selection;
#[cfg(test)]
mod tests;
mod view;

pub use cache::PageRequest;
pub use row::CachedRow;
pub use selection::{Grid, PointerEvent};

use crate::find::{FindDirection, FindQueryGeneration, MAX_QUERY_BYTES};
use crate::selection::{SelectionCopy, SelectionCopyOptions};
use crate::snapshot::HistorySnapshot as History;
use crate::snapshot::{
    ActiveScreenSnapshot, ColorValue, PresentationGeneration, Rgb, RowSnapshot, ScreenSnapshot,
    ScreenState, ScrollbackMovement, Snapshot,
};
use cache::ScrollbackCache;
use row::RowRef;
use spaceterm_protocol::pane::effects::Hyperlink;
use spaceterm_protocol::protocol::limits::{FIRST_PAGE_ROWS, MAX_CACHED_ROWS, MAX_PAGE_ROWS};
use std::collections::BTreeMap;
use std::ops::Range;
use std::sync::Arc;
use std::time::Instant;

/// A cell in absolute line numbers.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
struct Point {
    line: u64,
    column: u16,
}

/// The rows a reader can see: cached history and the visible screen.
struct Lines<'a, L, C> {
    cache: &'a ScrollbackCache<L, C>,
    screen: &'a ScreenState<L, C>,
    history: History,
    /// The alternate screen has no Scrollback, so only its visible rows are readable.
    alternate: bool,
}

impl<L, C> Copy for Lines<'_, L, C> {}

impl<L, C> Clone for Lines<'_, L, C> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<'a, L: Clone + Eq, C: Copy + Eq> Lines<'a, L, C> {
    fn get(self, line: u64) -> Option<RowRef<'a, L, C>> {
        if line < self.history.total {
            return (!self.alternate && line >= self.history.oldest)
                .then(|| self.cache.get(line).map(RowRef::Cached))
                .flatten();
        }
        let row = usize::try_from(line - self.history.total).ok()?;
        Some(RowRef::Visible(
            self.screen.rows.get(row)?,
            self.screen
                .row_soft_wrapped
                .get(row)
                .copied()
                .unwrap_or(false),
        ))
    }

    /// Every readable line, cached or not.
    fn range(self) -> Range<u64> {
        let start = if self.alternate {
            self.history.total
        } else {
            self.history.oldest
        };
        start..self.history.total + self.screen.rows.len() as u64
    }

    fn total(self) -> u64 {
        self.history.total
    }
}

fn lines<'a, S: Snapshot>(
    cache: &'a ScrollbackCache<S::Link, S::Color>,
    snapshot: &'a S,
) -> Lines<'a, S::Link, S::Color> {
    let screen = snapshot.screen();
    Lines {
        cache,
        screen,
        history: cache.history(),
        alternate: screen.active_screen == ActiveScreenSnapshot::Alternate,
    }
}

/// The outcome of copying the Selection.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CopyOutcome {
    Nothing,
    /// Some selected rows are not cached yet. Their pages are requested.
    Pending,
    Ready(SelectionCopy),
}

/// One Pane's Scrollback Cache and this client's view of it.
pub struct Scrollback<S: Snapshot = ScreenSnapshot> {
    cache: ScrollbackCache<S::Link, S::Color>,
    view: view::Viewport,
    find: find::LocalFind,
    selection: selection::LocalSelection,
    screen: Option<Arc<S>>,
    /// Asks for the rows just above the screen once after attaching.
    warm: bool,
    /// The copy asked for while its rows were missing, and the copy prepared for it, with the
    /// revisions it was prepared at.
    copying: Option<SelectionCopyOptions>,
    prepared: Option<((u64, u64, u64), SelectionCopyOptions, CopyOutcome)>,
    /// Increases when a frame rewrites a visible row inside the Selection.
    selected_content: u64,
    /// Rows earlier frames showed that scrolled into history before their page arrived. They
    /// stand in for those rows on screen only; Find and copy read cached rows alone.
    placeholders: Placeholders<S>,
    shown: view::Shown<S::Link, S::Color>,
}

/// A Selection's ordered endpoints, each an absolute line of history epoch `epoch` and a
/// column, both included. Line `p` is history position `p`; visible row `r` is line `total + r`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EpochSelection {
    pub epoch: u64,
    pub start: (u64, u16),
    pub end: (u64, u16),
}

/// What assistive technology sees of this client's view: how far it is scrolled back, the
/// Selection, and the history of the frame presented last, which places each line.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AccessibilityView {
    pub scrolled: u64,
    pub selection: Option<EpochSelection>,
    /// `None` before the first frame.
    pub history: Option<History>,
    pub alternate: bool,
}

type Placeholders<S> = BTreeMap<
    u64,
    (
        RowSnapshot<<S as Snapshot>::Link, <S as Snapshot>::Color>,
        bool,
    ),
>;

/// A composed screen and its offset behind the live screen.
pub struct Presented<S> {
    pub screen: Arc<S>,
    pub scrolled_rows: u64,
}

impl<S: Snapshot> Default for Scrollback<S> {
    fn default() -> Self {
        Self {
            cache: ScrollbackCache::default(),
            view: view::Viewport::default(),
            find: find::LocalFind::default(),
            selection: selection::LocalSelection::default(),
            screen: None,
            warm: false,
            copying: None,
            prepared: None,
            selected_content: 0,
            placeholders: BTreeMap::new(),
            shown: view::Shown::default(),
        }
    }
}

impl<S: Snapshot> Scrollback<S> {
    /// Starts presenting a channel's Terminal Session, including after a reconnect. Rows stay
    /// cached only when the session is unchanged and the next frame keeps its history epoch.
    pub fn attach(&mut self, session: [u8; 32]) {
        if !self.cache.attach(session) {
            self.reset_reading();
        }
        self.warm = true;
    }

    /// Keeps the cached rows, the view's position and the Selection for the next presentation
    /// of the same Terminal Session. Find ends, because that presentation sends its own query.
    pub fn retain(&mut self) {
        self.cache.forget_in_flight();
        self.find = find::LocalFind::default();
        self.copying = None;
        self.prepared = None;
    }

    /// Applies the newest decoded frame and the history it reports.
    pub fn screen(&mut self, screen: Arc<S>) {
        let history = screen.screen().history;
        let same_epoch = history.epoch == self.cache.history().epoch;
        if let Some(previous) = self.screen.clone().filter(|_| same_epoch) {
            if self.selected_rows_changed(&previous, &screen) {
                self.selected_content += 1;
            }
            self.keep_placeholders(&previous, history);
        }
        if !self.cache.observe(history) {
            self.reset_reading();
        }
        self.prune_placeholders();
        let alternate = screen.screen().active_screen == ActiveScreenSnapshot::Alternate;
        self.screen = Some(screen);
        if alternate {
            self.view.follow();
        } else {
            self.view.observe(history);
        }
        let readable = self.lines_range();
        let floor = if alternate {
            readable.start
        } else {
            self.floor()
        };
        self.selection
            .observe(floor.max(readable.start)..readable.end);
    }

    /// Moves the view by native wheel steps; positive steps move toward older rows.
    pub fn wheel(&mut self, vertical_steps: i32) -> bool {
        if self.alternate() || vertical_steps == 0 {
            return false;
        }
        let history = self.cache.history();
        self.view.scroll(history, -i64::from(vertical_steps))
    }

    pub fn scroll(&mut self, movement: ScrollbackMovement) -> bool {
        if self.alternate() {
            return false;
        }
        let history = self.cache.history();
        let page = i64::from(self.rows().max(1));
        match movement {
            ScrollbackMovement::PageUp => self.view.scroll(history, -page),
            ScrollbackMovement::PageDown => self.view.scroll(history, page),
            ScrollbackMovement::Top => self.view.set_top(history, history.oldest),
            ScrollbackMovement::Bottom => self.view.follow(),
        }
    }

    /// Moves the view to `offset_rows` below the oldest retained row, as the scrollbar reports.
    pub fn scroll_to(&mut self, offset_rows: u64) -> bool {
        if self.alternate() {
            return false;
        }
        let history = self.cache.history();
        self.view
            .set_top(history, history.oldest.saturating_add(offset_rows))
    }

    /// Typing and pasting return the view to the screen and clear the Selection, as they do in
    /// a local terminal.
    pub fn input(&mut self) -> bool {
        let cleared = self.selection.clear();
        self.view.follow() | cleared
    }

    pub fn pointer(&mut self, event: PointerEvent, grid: Grid, now: Instant) -> bool {
        let Some(screen) = self.screen.clone() else {
            return false;
        };
        let top = self.view.top(self.cache.history());
        let lines = lines(&self.cache, screen.as_ref());
        self.selection.pointer(event, grid, now, lines, top)
    }

    /// Whether a selection drag past the top or bottom edge wants `autoscroll` ticks.
    pub fn autoscrolling(&self) -> bool {
        self.selection.autoscrolling()
    }

    /// Scrolls one row in the drag's direction and extends the Selection to the pointer.
    pub fn autoscroll(&mut self, grid: Grid) -> bool {
        let Some(screen) = self.screen.clone() else {
            return false;
        };
        let Some(step) = self.selection.autoscroll_lines() else {
            return false;
        };
        let history = self.cache.history();
        if !self.alternate() {
            self.view.scroll(history, step);
        }
        let top = self.view.top(history);
        let lines = lines(&self.cache, screen.as_ref());
        self.selection.continue_drag(grid, lines, top)
    }

    /// Another owner took the pointer, such as the program or a hidden view.
    pub fn pointer_cancel(&mut self) {
        self.selection.cancel_gesture();
    }

    /// Sets the Selection assistive technology chose, or clears it. A Selection of another
    /// history epoch, or with an endpoint outside the readable lines, leaves the Selection
    /// unchanged. Returns whether it changed.
    pub fn select(&mut self, selection: Option<EpochSelection>) -> bool {
        let Some(screen) = self.screen.clone() else {
            return false;
        };
        let lines = lines(&self.cache, screen.as_ref());
        let readable = lines.range();
        let range = match selection {
            None => None,
            Some(selection) => {
                let point = |(line, column): (u64, u16)| {
                    readable.contains(&line).then_some(Point { line, column })
                };
                match point(selection.start).zip(point(selection.end)) {
                    Some(range) if selection.epoch == lines.history.epoch => Some(range),
                    _ => return false,
                }
            }
        };
        self.selection.replace(range)
    }

    pub fn selection_present(&self) -> bool {
        self.selection.range().is_some()
    }

    /// Sets a query within the byte limit. Oversized input retires the preceding query.
    pub fn set_find_query(&mut self, generation: FindQueryGeneration, text: String) -> bool {
        if text.len() > MAX_QUERY_BYTES {
            return self.end_find(generation);
        }
        self.find.set_query(generation, text);
        true
    }

    pub fn navigate_find(
        &mut self,
        generation: FindQueryGeneration,
        direction: FindDirection,
    ) -> bool {
        let Some(screen) = self.screen.clone() else {
            return false;
        };
        let history = self.cache.history();
        let alternate = self.alternate();
        let rows = self.rows();
        let top = self.view.top(history);
        let lines = lines(&self.cache, screen.as_ref());
        let Some(target) = self.find.navigate(generation, direction, lines, top, rows) else {
            return false;
        };
        if !alternate {
            self.view.set_top(history, target);
        }
        true
    }

    pub fn end_find(&mut self, generation: FindQueryGeneration) -> bool {
        self.find.end(generation)
    }

    /// Copies the Selection. Missing rows are requested, and a later call completes the copy.
    pub fn copy(&mut self, options: SelectionCopyOptions) -> CopyOutcome {
        let Some(screen) = self.screen.clone() else {
            return CopyOutcome::Nothing;
        };
        let key = (
            self.selection.revision(),
            self.cache.revision(),
            self.selected_content,
        );
        if let Some((saved_key, saved, copy)) = &self.prepared
            && *saved_key == key
            && *saved == options
        {
            return copy.clone();
        }
        let copy = self.selection.copy(
            lines(&self.cache, screen.as_ref()),
            screen.palette(),
            options,
        );
        self.copying = (copy == CopyOutcome::Pending).then_some(options);
        self.prepared = Some((key, options, copy.clone()));
        copy
    }

    /// The view and Selection for assistive technology.
    pub fn accessibility_view(&self) -> AccessibilityView {
        let history = self.cache.history();
        AccessibilityView {
            scrolled: history.total - self.view.top(history),
            selection: self.selection.range().map(|(start, end)| EpochSelection {
                epoch: history.epoch,
                start: (start.line, start.column),
                end: (end.line, end.column),
            }),
            history: self.screen.is_some().then_some(history),
            alternate: self.alternate(),
        }
    }

    /// Whether `next` rewrites a row of `previous` that the Selection covers and `next` shows.
    fn selected_rows_changed(&self, previous: &S, next: &S) -> bool {
        let Some((start, end)) = self.selection.range() else {
            return false;
        };
        let before = lines(&self.cache, previous);
        let after = Lines {
            history: next.screen().history,
            alternate: next.screen().active_screen == ActiveScreenSnapshot::Alternate,
            ..lines(&self.cache, next)
        };
        let next = next.screen();
        let shown = next.history.total..next.history.total + next.rows.len() as u64;
        (start.line.max(shown.start)..=end.line.min(shown.end.saturating_sub(1))).any(
            |line| match (before.get(line), after.get(line)) {
                (Some(old), Some(new)) => !old.same(new),
                (None, None) => false,
                _ => true,
            },
        )
    }

    /// Keeps the rows of `previous` that scrolled into history and are not cached, so a
    /// scrolled-back view does not blank them until their page arrives.
    fn keep_placeholders(&mut self, previous: &S, history: History) {
        let previous = previous.screen();
        let old_total = self.cache.history().total;
        for (offset, row) in previous.rows.iter().enumerate() {
            let line = old_total + offset as u64;
            if line >= history.total {
                break;
            }
            if self.cache.get(line).is_none() {
                let wrapped = previous
                    .row_soft_wrapped
                    .get(offset)
                    .copied()
                    .unwrap_or(false);
                self.placeholders.insert(line, (Arc::clone(row), wrapped));
            }
        }
    }

    /// Drops placeholders the cache replaced, that left retained history, or that exceed one
    /// page of the newest rows.
    fn prune_placeholders(&mut self) {
        let history = self.cache.history();
        let cache = &self.cache;
        self.placeholders.retain(|line, _| {
            (history.oldest..history.total).contains(line) && cache.get(*line).is_none()
        });
        while self.placeholders.len() > usize::from(MAX_PAGE_ROWS) {
            self.placeholders.pop_first();
        }
    }

    /// Whether a pending copy can now complete.
    pub fn copy_ready(&mut self) -> bool {
        let Some(options) = self.copying else {
            return false;
        };
        matches!(self.copy(options), CopyOutcome::Ready(_))
    }

    /// Pages to ask the server for now. Rows the view, an open Find or a pending copy needs come
    /// first; at most four pages are ever in flight.
    pub fn requests(&mut self) -> Vec<PageRequest> {
        if self.alternate() || self.screen.is_none() {
            return Vec::new();
        }
        let history = self.cache.history();
        let focus = self.focus();
        let floor = self.floor();
        let mut requests = Vec::new();
        if let Some((start, end)) = self.selection.range().filter(|_| self.copying.is_some()) {
            requests.extend(
                self.cache
                    .request(start.line.max(floor)..end.line + 1, focus),
            );
        }
        if !self.view.following() {
            // The view's rows, and a page beyond each edge for the next steps.
            let top = self.view.top(history);
            let page = u64::from(MAX_PAGE_ROWS);
            let end = top + u64::from(self.rows());
            requests.extend(
                self.cache
                    .request(top.saturating_sub(page)..end + page, focus),
            );
        }
        if self.warm {
            self.warm = false;
            requests.extend(self.cache.request(
                history.total.saturating_sub(u64::from(FIRST_PAGE_ROWS))..history.total,
                focus,
            ));
        }
        if self.find.active() {
            requests.extend(self.cache.request(floor..history.total, focus));
        }
        requests
    }

    /// Forgets requests the link could not send.
    pub fn unsent(&mut self, requests: &[PageRequest]) {
        self.cache.unsent(requests);
    }

    /// Whether the Selection's rows are all cached, so copying needs no page.
    pub fn copy_available(&self) -> bool {
        let Some((start, end)) = self.selection.range() else {
            return false;
        };
        self.screen.is_some() && self.cache.covers(start.line..end.line + 1)
    }

    /// Settles a page request. `page` is the reply's epoch, start and rows, or `None` when the
    /// server refused it or the connection ended.
    pub fn page(&mut self, request: PageRequest, page: Option<DecodedPage<S::Link, S::Color>>) {
        let focus = self.focus();
        self.cache.page(request, page, focus);
        self.prune_placeholders();
    }

    /// The screen this client presents: the newest frame, moved to the view's position, with
    /// the Selection and Find matches applied.
    pub fn present(&mut self, generation: PresentationGeneration) -> Option<Presented<S>> {
        let screen = self.screen.clone()?;
        let history = self.cache.history();
        let top = self.view.top(history);
        let lines = lines(&self.cache, screen.as_ref());
        let find = self.find.snapshot(lines, top, self.rows());
        Some(view::compose(
            screen.as_ref(),
            lines,
            (&self.placeholders, &mut self.shown),
            top,
            (self.selection.range(), self.selection.revision()),
            find,
            generation,
        ))
    }

    /// Whether the view follows the screen rather than showing history.
    pub fn following(&self) -> bool {
        self.view.following()
    }

    fn reset_reading(&mut self) {
        self.placeholders.clear();
        self.shown.clear();
        self.view.follow();
        self.selection.clear();
        self.find.invalidate();
        self.copying = None;
        self.prepared = None;
    }

    fn lines_range(&self) -> Range<u64> {
        self.screen
            .as_deref()
            .map_or(0..0, |screen| lines(&self.cache, screen).range())
    }

    fn alternate(&self) -> bool {
        self.screen
            .as_ref()
            .is_some_and(|screen| screen.screen().active_screen == ActiveScreenSnapshot::Alternate)
    }

    fn rows(&self) -> u16 {
        self.screen.as_ref().map_or(0, |screen| {
            u16::try_from(screen.screen().rows.len()).unwrap_or(u16::MAX)
        })
    }

    /// The oldest line Find and copying read: the cache holds at most the newest
    /// [`MAX_CACHED_ROWS`] history rows at once.
    fn floor(&self) -> u64 {
        let history = self.cache.history();
        history
            .total
            .saturating_sub(MAX_CACHED_ROWS as u64)
            .max(history.oldest)
    }

    fn focus(&self) -> u64 {
        self.view.top(self.cache.history())
    }
}

/// A validated page's epoch, first position and packed rows.
pub type DecodedPage<L = Hyperlink, C = Rgb> = (u64, u64, Vec<CachedRow<L, C>>);

/// Decodes an answer that matches its request against the current history epoch's width.
pub fn decode_page<L: Clone + Eq, C: ColorValue>(
    request: PageRequest,
    page: spaceterm_protocol::protocol::Page,
    columns: u16,
    resolve_link: impl FnMut(&Hyperlink) -> Option<Arc<L>>,
) -> Option<DecodedPage<L, C>> {
    if page.epoch != request.epoch
        || page.start < request.start
        || page.rows.len() > usize::from(request.rows)
    {
        return None;
    }
    let rows = crate::screen::decode_page(&page, columns, resolve_link)
        .ok()?
        .iter()
        .map(|(cells, wrapped)| CachedRow::new(cells, *wrapped))
        .collect();
    Some((page.epoch, page.start, rows))
}
