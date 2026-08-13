//! Bounded semantic terminal snapshots for native accessibility clients.

use std::ops::Range;
use std::ptr::NonNull;

use crate::error::{Error, Result, from_result};
use crate::{Terminal, ffi};

const MAX_CELLS: usize = 16_384;
const MAX_ROWS: usize = 256;

/// The terminal screen represented by a snapshot.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum Screen {
    /// The retained primary screen, including scrollback.
    Primary,
    /// The alternate screen viewport.
    Alternate,
}

impl TryFrom<ffi::AccessibilityScreen::Type> for Screen {
    type Error = Error;

    fn try_from(value: ffi::AccessibilityScreen::Type) -> Result<Self> {
        match value {
            ffi::AccessibilityScreen::PRIMARY => Ok(Self::Primary),
            ffi::AccessibilityScreen::ALTERNATE => Ok(Self::Alternate),
            _ => Err(Error::InvalidValue),
        }
    }
}

impl From<Screen> for ffi::AccessibilityScreen::Type {
    fn from(value: Screen) -> Self {
        match value {
            Screen::Primary => ffi::AccessibilityScreen::PRIMARY,
            Screen::Alternate => ffi::AccessibilityScreen::ALTERNATE,
        }
    }
}

/// Stable identity of a physical row within a screen generation.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct RowId {
    /// Screen containing the row.
    pub screen: Screen,
    /// Generation of that screen buffer.
    pub screen_generation: u64,
    /// Ghostty page generation containing the row.
    pub node_serial: u64,
    /// Row index within the page.
    pub page_row: u16,
}

impl TryFrom<ffi::AccessibilityRowId> for RowId {
    type Error = Error;

    fn try_from(value: ffi::AccessibilityRowId) -> Result<Self> {
        Ok(Self {
            screen: value.screen.try_into()?,
            screen_generation: value.screen_generation,
            node_serial: value.node_serial,
            page_row: value.page_row,
        })
    }
}

impl From<RowId> for ffi::AccessibilityRowId {
    fn from(value: RowId) -> Self {
        Self {
            screen: value.screen.into(),
            screen_generation: value.screen_generation,
            node_serial: value.node_serial,
            page_row: value.page_row,
        }
    }
}

/// A generation-bound cell position suitable for selection mutation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CellRef {
    /// Stable row identity.
    pub row: RowId,
    /// Content revision of the row.
    pub row_revision: u64,
    /// Zero-based grid column.
    pub column: u16,
}

impl TryFrom<ffi::AccessibilityCellRef> for CellRef {
    type Error = Error;

    fn try_from(value: ffi::AccessibilityCellRef) -> Result<Self> {
        Ok(Self {
            row: value.row.try_into()?,
            row_revision: value.row_revision,
            column: value.column,
        })
    }
}

impl From<CellRef> for ffi::AccessibilityCellRef {
    fn from(value: CellRef) -> Self {
        Self {
            row: value.row.into(),
            row_revision: value.row_revision,
            column: value.column,
        }
    }
}

/// Text-bearing terminal cell in a changed row.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Cell {
    /// Cell text, including every codepoint in its grapheme cluster.
    pub text: String,
    /// Zero-based grid column.
    pub column: u16,
    /// Width of the grapheme in terminal cells.
    pub width_cells: u16,
}

/// Complete contents of one changed row.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Row {
    /// Stable row identity.
    pub id: RowId,
    /// Content revision used by [`CellRef`].
    pub revision: u64,
    /// Whether the following physical row continues this logical line.
    pub soft_wrapped: bool,
    /// Text-bearing cells in grid order.
    pub cells: Vec<Cell>,
}

/// Selection endpoints included in a complete snapshot.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Selection {
    /// First endpoint in terminal order.
    pub start: CellRef,
    /// Last endpoint in terminal order.
    pub end: CellRef,
    /// Whether Ghostty represents the selection as a rectangle.
    pub rectangle: bool,
}

/// A semantic delta copied out of Ghostty-owned storage.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Snapshot {
    /// Monotonic state update revision.
    pub revision: u64,
    /// Active terminal screen.
    pub screen: Screen,
    /// Active screen generation.
    pub screen_generation: u64,
    /// Whether every pending changed row has been extracted.
    pub complete: bool,
    /// Whether another bounded update is required.
    pub more: bool,
    /// Monotonic identity of the topology reduction represented by this snapshot.
    pub topology_epoch: u64,
    /// Replacement row topology, present only when it changed.
    pub topology: Option<Vec<RowId>>,
    /// Visible physical row indexes in the document.
    pub visible_lines: Range<usize>,
    /// Cursor location, published with complete snapshots.
    pub cursor: Option<CellRef>,
    /// Selection location, published with complete snapshots.
    pub selection: Option<Selection>,
    /// Rows extracted by this bounded update.
    pub changed_rows: Vec<Row>,
}

/// Hard work limits for one incremental update.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct UpdateOptions {
    /// Maximum inspected grid cells.
    pub max_cells: usize,
    /// Maximum extracted rows.
    pub max_rows: usize,
}

impl Default for UpdateOptions {
    fn default() -> Self {
        Self {
            max_cells: MAX_CELLS,
            max_rows: MAX_ROWS,
        }
    }
}

/// Persistent, worker-owned incremental accessibility observer.
#[derive(Debug)]
pub struct State {
    inner: NonNull<ffi::AccessibilityState>,
    topology: [TopologyAccumulator; 2],
}

impl State {
    /// Create empty accessibility observer state.
    pub fn new() -> Result<Self> {
        let mut raw = std::ptr::null_mut();
        let result = unsafe { ffi::ghostty_accessibility_state_new(&raw mut raw) };
        from_result(result)?;
        Ok(Self {
            inner: NonNull::new(raw).ok_or(Error::InvalidValue)?,
            topology: std::array::from_fn(|_| TopologyAccumulator::default()),
        })
    }

    /// Observe dirty state and copy one bounded semantic delta.
    pub fn update(
        &mut self,
        terminal: &Terminal<'_, '_>,
        options: UpdateOptions,
    ) -> Result<Snapshot> {
        if options.max_cells == 0 || options.max_rows == 0 {
            return Err(Error::InvalidValue);
        }
        let raw_options = ffi::AccessibilityUpdateOptions {
            size: std::mem::size_of::<ffi::AccessibilityUpdateOptions>(),
            max_cells: options.max_cells.min(MAX_CELLS),
            max_rows: options.max_rows.min(MAX_ROWS),
        };
        let mut raw_snapshot = ffi::AccessibilitySnapshot {
            size: std::mem::size_of::<ffi::AccessibilitySnapshot>(),
            ..Default::default()
        };
        let mut context = UpdateContext::default();
        let result = unsafe {
            ffi::ghostty_accessibility_state_update(
                self.inner.as_ptr(),
                terminal.inner.as_raw(),
                &raw_options,
                std::ptr::from_mut(&mut context).cast(),
                Some(topology_callback),
                Some(row_callback),
                Some(cell_callback),
                &raw mut raw_snapshot,
            )
        };
        from_result(result)?;
        if context.invalid {
            return Err(Error::InvalidValue);
        }
        let screen = raw_snapshot.screen.try_into()?;
        let topology_index = match screen {
            Screen::Primary => 0,
            Screen::Alternate => 1,
        };
        let topology = (raw_snapshot.topology_changed || raw_snapshot.topology_complete)
            .then(|| {
                self.topology[topology_index].push(
                    raw_snapshot.topology_epoch,
                    raw_snapshot.topology_complete,
                    context.topology,
                )
            })
            .flatten();
        let cursor = raw_snapshot
            .has_cursor
            .then(|| raw_snapshot.cursor.try_into())
            .transpose()?;
        let selection = raw_snapshot
            .has_selection
            .then(|| {
                Ok::<_, Error>(Selection {
                    start: raw_snapshot.selection_start.try_into()?,
                    end: raw_snapshot.selection_end.try_into()?,
                    rectangle: raw_snapshot.selection_rectangle,
                })
            })
            .transpose()?;
        Ok(Snapshot {
            revision: raw_snapshot.revision,
            screen,
            screen_generation: raw_snapshot.screen_generation,
            complete: raw_snapshot.complete,
            more: raw_snapshot.more,
            topology_epoch: raw_snapshot.topology_epoch,
            topology,
            visible_lines: raw_snapshot.visible_start
                ..raw_snapshot
                    .visible_start
                    .saturating_add(raw_snapshot.visible_len),
            cursor,
            selection,
            changed_rows: context.rows,
        })
    }

    /// Replace or clear the active selection if the supplied refs are current.
    ///
    /// Returns `false` without mutation when either endpoint is stale.
    pub fn set_selection(
        &mut self,
        terminal: &Terminal<'_, '_>,
        selection: Option<(CellRef, CellRef)>,
    ) -> Result<bool> {
        let raw = selection.map(|(start, end)| (start.into(), end.into()));
        let (start, end) = raw
            .as_ref()
            .map_or((std::ptr::null(), std::ptr::null()), |(start, end)| {
                (std::ptr::from_ref(start), std::ptr::from_ref(end))
            });
        let mut changed = false;
        let result = unsafe {
            ffi::ghostty_accessibility_state_set_selection(
                self.inner.as_ptr(),
                terminal.inner.as_raw(),
                start,
                end,
                &raw mut changed,
            )
        };
        from_result(result)?;
        Ok(changed)
    }
}

impl Drop for State {
    fn drop(&mut self) {
        unsafe { ffi::ghostty_accessibility_state_free(self.inner.as_ptr()) }
    }
}

#[derive(Debug, Default)]
struct TopologyAccumulator {
    epoch: u64,
    rows: Vec<RowId>,
}

impl TopologyAccumulator {
    fn push(&mut self, epoch: u64, complete: bool, chunk: Vec<RowId>) -> Option<Vec<RowId>> {
        if self.epoch != epoch {
            self.epoch = epoch;
            self.rows.clear();
        }
        self.rows.extend(chunk);
        complete.then(|| std::mem::take(&mut self.rows))
    }
}

#[derive(Default)]
struct UpdateContext {
    topology: Vec<RowId>,
    rows: Vec<Row>,
    invalid: bool,
}

unsafe extern "C" fn topology_callback(
    userdata: *mut std::ffi::c_void,
    raw: *const ffi::AccessibilityRowId,
) -> bool {
    if userdata.is_null() || raw.is_null() {
        return false;
    }
    // SAFETY: Ghostty invokes callbacks synchronously with the context and
    // row pointers supplied by `State::update` for the duration of that call.
    let (context, raw) = unsafe { (&mut *userdata.cast::<UpdateContext>(), &*raw) };
    match (*raw).try_into() {
        Ok(row) => context.topology.push(row),
        Err(_) => context.invalid = true,
    }
    !context.invalid
}

unsafe extern "C" fn row_callback(
    userdata: *mut std::ffi::c_void,
    raw: *const ffi::AccessibilityRow,
) -> bool {
    if userdata.is_null() || raw.is_null() {
        return false;
    }
    // SAFETY: See `topology_callback`; all callbacks share that lifetime.
    let (context, raw) = unsafe { (&mut *userdata.cast::<UpdateContext>(), &*raw) };
    match (*raw).id.try_into() {
        Ok(id) => context.rows.push(Row {
            id,
            revision: (*raw).revision,
            soft_wrapped: (*raw).soft_wrapped,
            cells: Vec::new(),
        }),
        Err(_) => context.invalid = true,
    }
    !context.invalid
}

unsafe extern "C" fn cell_callback(
    userdata: *mut std::ffi::c_void,
    raw: *const ffi::AccessibilityCell,
) -> bool {
    if userdata.is_null() || raw.is_null() {
        return false;
    }
    // SAFETY: See `topology_callback`; all callbacks share that lifetime.
    let (context, raw) = unsafe { (&mut *userdata.cast::<UpdateContext>(), &*raw) };
    let Some(row) = context.rows.last_mut() else {
        context.invalid = true;
        return false;
    };
    let extra_codepoints = if raw.extra_codepoints_len == 0 {
        &[]
    } else if raw.extra_codepoints.is_null() {
        context.invalid = true;
        return false;
    } else {
        // SAFETY: Ghostty guarantees this borrowed slice is valid for the
        // synchronous callback, and the NULL case was rejected above.
        unsafe { std::slice::from_raw_parts(raw.extra_codepoints, raw.extra_codepoints_len) }
    };
    let Some(base) = char::from_u32(raw.codepoint) else {
        context.invalid = true;
        return false;
    };
    let mut text = String::from(base);
    for codepoint in extra_codepoints {
        let Some(character) = char::from_u32(*codepoint) else {
            context.invalid = true;
            return false;
        };
        text.push(character);
    }
    let width_cells = match (*raw).wide {
        ffi::AccessibilityCellWide::NARROW => 1,
        ffi::AccessibilityCellWide::WIDE => 2,
        _ => {
            context.invalid = true;
            return false;
        }
    };
    row.cells.push(Cell {
        text,
        column: (*raw).column,
        width_cells,
    });
    true
}

#[cfg(test)]
mod tests {
    use super::{RowId, Screen, TopologyAccumulator};

    fn row(node_serial: u64, page_row: u16) -> RowId {
        RowId {
            screen: Screen::Primary,
            screen_generation: 7,
            node_serial,
            page_row,
        }
    }

    #[test]
    fn topology_is_published_only_after_the_final_chunk() {
        let mut accumulator = TopologyAccumulator::default();

        assert_eq!(accumulator.push(1, false, vec![row(10, 0)]), None);
        assert_eq!(
            accumulator.push(1, true, vec![row(10, 1)]),
            Some(vec![row(10, 0), row(10, 1)])
        );
    }

    #[test]
    fn a_new_epoch_discards_an_incomplete_topology() {
        let mut accumulator = TopologyAccumulator::default();

        assert_eq!(accumulator.push(1, false, vec![row(10, 0)]), None);
        assert_eq!(
            accumulator.push(2, true, vec![row(20, 0)]),
            Some(vec![row(20, 0)])
        );
    }

    #[test]
    fn screen_switch_does_not_discard_an_incomplete_topology() {
        let mut accumulators: [TopologyAccumulator; 2] =
            std::array::from_fn(|_| TopologyAccumulator::default());

        assert_eq!(accumulators[0].push(1, false, vec![row(10, 0)]), None);
        assert_eq!(
            accumulators[1].push(2, true, vec![row(20, 0)]),
            Some(vec![row(20, 0)])
        );
        assert_eq!(
            accumulators[0].push(1, true, vec![row(10, 1)]),
            Some(vec![row(10, 0), row(10, 1)])
        );
    }
}
