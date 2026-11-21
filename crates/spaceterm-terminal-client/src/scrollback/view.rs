//! This client's position in the Scrollback and the screen it presents there.
use super::Presented;
use super::{History, Lines, Point};
use crate::find::TerminalFindSnapshot;
use crate::snapshot::{
    ContentDamageSnapshot, PresentationGeneration, RowSnapshot, ScreenState, ScrollbarSnapshot,
    Snapshot, SnapshotDamage, ViewportSnapshot,
};
use std::collections::HashMap;
use std::sync::Arc;

/// Where the view starts. Following, it shows the screen and moves with output. Scrolled back,
/// it stays on the same rows while output continues below.
#[derive(Default)]
pub(super) struct Viewport {
    top: Option<u64>,
}

impl Viewport {
    /// The line at the top of the view.
    pub(super) fn top(&self, history: History) -> u64 {
        self.top.map_or(history.total, |top| {
            top.clamp(history.oldest, history.total)
        })
    }

    pub(super) fn following(&self) -> bool {
        self.top.is_none()
    }

    /// Returns whether the view moved.
    pub(super) fn set_top(&mut self, history: History, top: u64) -> bool {
        let before = self.top(history);
        self.top = (top < history.total).then(|| top.max(history.oldest));
        self.top(history) != before
    }

    pub(super) fn scroll(&mut self, history: History, rows: i64) -> bool {
        let top = self.top(history);
        let target = if rows < 0 {
            top.saturating_sub(rows.unsigned_abs())
        } else {
            top.saturating_add(rows.unsigned_abs())
        };
        self.set_top(history, target)
    }

    pub(super) fn follow(&mut self) -> bool {
        self.top.take().is_some()
    }

    /// Keeps a scrolled view on retained rows after a frame.
    pub(super) fn observe(&mut self, history: History) {
        if let Some(top) = self.top {
            self.set_top(history, top);
        }
    }
}

/// The presented snapshot of each cached history row in view, and its selected variant with the
/// Selection revision it was marked at. History rows of one epoch never change, so a snapshot
/// stays valid until the epoch ends.
pub(super) type Shown<L, C> = HashMap<u64, ShownRow<L, C>>;

pub(super) struct ShownRow<L, C> {
    row: RowSnapshot<L, C>,
    soft: bool,
    selected: Option<(u64, RowSnapshot<L, C>)>,
}

/// Builds the presented screen. A view following the screen with no Selection and no Find
/// reuses the frame's rows as they are.
pub(super) fn compose<S: Snapshot>(
    screen: &S,
    lines: Lines<'_, S::Link, S::Color>,
    (placeholders, shown): (&super::Placeholders<S>, &mut Shown<S::Link, S::Color>),
    top: u64,
    (selection, selection_revision): (Option<(Point, Point)>, u64),
    find: Option<Arc<TerminalFindSnapshot>>,
    generation: PresentationGeneration,
) -> Presented<S> {
    let total = lines.total();
    let start = lines.range().start;
    let rows = screen.screen().rows.len();
    let offset = total - top;
    let mut snapshot = screen.clone();
    let presented = snapshot.screen_mut();
    presented.generation = generation;
    presented.selection_present = selection.is_some();
    presented.selection_revision = selection_revision;
    presented.find = find;
    let offset_rows = top - start;
    presented.viewport = ViewportSnapshot {
        offset_rows,
        visible_rows: rows as u64,
    };
    presented.scrollbar = ScrollbarSnapshot {
        total_rows: total - start + rows as u64,
        offset_rows,
        visible_rows: rows as u64,
    };
    if offset == 0 && selection.is_none() {
        return Presented {
            screen: Arc::new(snapshot),
            scrolled_rows: 0,
        };
    }
    let blank = blank_row(screen.screen());
    let mut presented_rows = Vec::with_capacity(rows);
    let mut wrapped = Vec::with_capacity(rows);
    let shown_lines = top..top + rows as u64;
    shown.retain(|line, _| shown_lines.contains(line) && *line < total);
    for line in shown_lines {
        let selected = selection.filter(|(start, end)| (start.line..=end.line).contains(&line));
        // A cached history row keeps one presented snapshot between presents, so the renderer
        // finds it unchanged by pointer.
        if line < total
            && let Some(cached) = lines.get(line)
        {
            let entry = shown.entry(line).or_insert_with(|| ShownRow {
                row: cached.snapshot(),
                soft: cached.wrapped(),
                selected: None,
            });
            let row = match selected {
                None => Arc::clone(&entry.row),
                Some(range) => match &entry.selected {
                    Some((revision, row)) if *revision == selection_revision => Arc::clone(row),
                    _ => {
                        let row = select(Arc::clone(&entry.row), line, range);
                        entry.selected = Some((selection_revision, Arc::clone(&row)));
                        row
                    }
                },
            };
            presented_rows.push(row);
            wrapped.push(entry.soft);
            continue;
        }
        let (row, soft) = match lines.get(line) {
            Some(row) => (row.snapshot(), row.wrapped()),
            None => placeholders
                .get(&line)
                .cloned()
                .unwrap_or_else(|| (Arc::clone(&blank), false)),
        };
        presented_rows.push(match selected {
            Some(range) => select(row, line, range),
            None => row,
        });
        wrapped.push(soft);
    }
    presented.rows = presented_rows.into();
    presented.row_soft_wrapped = wrapped.into();
    presented.damage = SnapshotDamage {
        content: ContentDamageSnapshot::Full,
        cursor: ContentDamageSnapshot::Full,
        scrollbar: true,
        viewport: true,
        selection_presence: true,
        search: true,
        ..screen.screen().damage.clone()
    };
    if offset > 0 {
        let cursor = snapshot.cursor_mut();
        cursor.position = cursor.position.and_then(|mut position| {
            position.row = u16::try_from(u64::from(position.row) + offset)
                .ok()
                .filter(|row| usize::from(*row) < rows)?;
            Some(position)
        });
    }
    Presented {
        screen: Arc::new(snapshot),
        scrolled_rows: offset,
    }
}

/// The row's cells with the Selection's columns marked. A wide character is selected with its
/// spacer, as copying includes it whole.
fn select<L: Clone, C: Copy>(
    row: RowSnapshot<L, C>,
    line: u64,
    (start, end): (Point, Point),
) -> RowSnapshot<L, C> {
    if line < start.line || line > end.line || row.is_empty() {
        return row;
    }
    let last = row.len() - 1;
    let first = if line == start.line {
        usize::from(start.column)
    } else {
        0
    };
    let through = if line == end.line {
        usize::from(end.column).min(last)
    } else {
        last
    };
    let mut cells = row.to_vec();
    for cell in &mut cells[first..=through] {
        cell.selected = true;
    }
    if through < last && cells[through + 1].spacer_tail {
        cells[through + 1].selected = true;
    }
    cells.into()
}

fn blank_row<L: Clone, C: Copy>(screen: &ScreenState<L, C>) -> RowSnapshot<L, C> {
    vec![super::row::blank_cell(); usize::from(screen.size.cols)].into()
}
