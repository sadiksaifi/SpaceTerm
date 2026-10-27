//! This client's position in the Scrollback and the screen it presents there.
use super::{History, Lines, Point};
use crate::terminal::emulator::{
    ContentDamageSnapshot, ScrollbarSnapshot, SnapshotDamage, ViewportSnapshot,
};
use crate::terminal::{PresentationGeneration, RowSnapshot, ScreenSnapshot, TerminalFindSnapshot};
use std::collections::BTreeMap;
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
        self.top
            .map_or(history.total, |top| top.clamp(history.oldest, history.total))
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

/// Builds the presented screen. A view following the screen with no Selection and no Find
/// reuses the frame's rows as they are.
pub(super) fn compose(
    screen: &Arc<ScreenSnapshot>,
    lines: Lines<'_>,
    placeholders: &BTreeMap<u64, (RowSnapshot, bool)>,
    top: u64,
    (selection, selection_revision): (Option<(Point, Point)>, u64),
    find: Option<Arc<TerminalFindSnapshot>>,
    generation: PresentationGeneration,
) -> Arc<ScreenSnapshot> {
    let total = lines.total();
    let start = lines.range().start;
    let rows = screen.rows.len();
    let offset = total - top;
    let mut presented = ScreenSnapshot::clone(screen);
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
        return Arc::new(presented);
    }
    let blank = blank_row(screen);
    let mut presented_rows = Vec::with_capacity(rows);
    let mut wrapped = Vec::with_capacity(rows);
    for line in top..top + rows as u64 {
        let (row, soft) = match lines.get(line) {
            Some(row) => (row.snapshot(), row.wrapped()),
            None => placeholders
                .get(&line)
                .cloned()
                .unwrap_or_else(|| (Arc::clone(&blank), false)),
        };
        presented_rows.push(match selection {
            Some(range) => select(row, line, range),
            None => row,
        });
        wrapped.push(soft);
    }
    presented.rows = presented_rows.into();
    presented.row_soft_wrapped = wrapped.into();
    if offset > 0 {
        presented.cursor.position = presented.cursor.position.and_then(|mut position| {
            position.row = u16::try_from(u64::from(position.row) + offset)
                .ok()
                .filter(|row| usize::from(*row) < rows)?;
            Some(position)
        });
        let shift = i32::try_from(offset).unwrap_or(i32::MAX);
        presented.graphics.placements = presented
            .graphics
            .placements
            .iter()
            .map(|placement| {
                let mut placement = placement.clone();
                placement.viewport_row = placement.viewport_row.saturating_add(shift);
                placement
            })
            .collect();
    }
    presented.damage = SnapshotDamage {
        content: ContentDamageSnapshot::Full,
        cursor: ContentDamageSnapshot::Full,
        scrollbar: true,
        viewport: true,
        selection_presence: true,
        search: true,
        ..screen.damage.clone()
    };
    Arc::new(presented)
}

/// The row's cells with the Selection's columns marked. A wide character is selected with its
/// spacer, as copying includes it whole.
fn select(row: RowSnapshot, line: u64, (start, end): (Point, Point)) -> RowSnapshot {
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

fn blank_row(screen: &ScreenSnapshot) -> RowSnapshot {
    vec![super::row::blank_cell(); usize::from(screen.size.cols)].into()
}
