use std::collections::{BTreeMap, BTreeSet};
use std::ops::Range;
use std::sync::{Arc, OnceLock};

use super::PresentationGeneration;
use super::emulator::ActiveScreenSnapshot;

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) enum AccessibilityScreen {
    Primary,
    Alternate,
}

impl From<ActiveScreenSnapshot> for AccessibilityScreen {
    fn from(screen: ActiveScreenSnapshot) -> Self {
        match screen {
            ActiveScreenSnapshot::Primary => Self::Primary,
            ActiveScreenSnapshot::Alternate => Self::Alternate,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) struct AccessibilityRowId {
    pub(crate) screen: AccessibilityScreen,
    pub(crate) screen_generation: usize,
    pub(crate) node_serial: u64,
    pub(crate) page_row: u16,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct AccessibilityCellRef {
    pub(crate) row: AccessibilityRowId,
    pub(crate) row_revision: u64,
    pub(crate) column: u16,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct AccessibilityCell {
    text: Arc<str>,
    column: u16,
    width_cells: u16,
    selected: bool,
}

impl AccessibilityCell {
    pub(crate) fn new(text: impl Into<String>, width_cells: u16, selected: bool) -> Self {
        Self {
            text: Arc::from(text.into()),
            column: 0,
            width_cells: width_cells.max(1),
            selected,
        }
    }

    pub(crate) fn at_column(text: impl Into<Arc<str>>, column: u16, width_cells: u16) -> Self {
        Self {
            text: text.into(),
            column,
            width_cells: width_cells.max(1),
            selected: false,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct AccessibilityLine {
    cells: Vec<AccessibilityCell>,
    soft_wrapped: bool,
}

impl AccessibilityLine {
    pub(crate) fn new(cells: Vec<AccessibilityCell>, soft_wrapped: bool) -> Self {
        Self {
            cells,
            soft_wrapped,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct AccessibilityRowUpdate {
    pub(crate) id: AccessibilityRowId,
    pub(crate) revision: u64,
    pub(crate) soft_wrapped: bool,
    pub(crate) cells: Vec<AccessibilityCell>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct AccessibilitySelectionRefs {
    pub(crate) start: AccessibilityCellRef,
    pub(crate) end: AccessibilityCellRef,
    pub(crate) rectangle: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct AccessibilityUpdate {
    pub(crate) revision: u64,
    pub(crate) screen: AccessibilityScreen,
    pub(crate) screen_generation: usize,
    pub(crate) complete: bool,
    pub(crate) more: bool,
    pub(crate) topology: Option<Vec<AccessibilityRowId>>,
    pub(crate) visible_lines: Range<usize>,
    pub(crate) cursor: Option<AccessibilityCellRef>,
    pub(crate) selection: Option<AccessibilitySelectionRefs>,
    pub(crate) changed_rows: Vec<AccessibilityRowUpdate>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct AccessibilityGeometry {
    origin_x: f32,
    origin_y: f32,
    cell_width: f32,
    line_height: f32,
}

impl AccessibilityGeometry {
    pub(crate) fn new(
        origin_x: f32,
        origin_y: f32,
        cell_width: f32,
        line_height: f32,
    ) -> Option<Self> {
        (origin_x.is_finite()
            && origin_y.is_finite()
            && cell_width.is_finite()
            && cell_width > 0.0
            && line_height.is_finite()
            && line_height > 0.0)
            .then_some(Self {
                origin_x,
                origin_y,
                cell_width,
                line_height,
            })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum AccessibilityNotification {
    Value,
    Selection,
    Focus,
}

impl AccessibilityNotification {
    const fn bit(self) -> u8 {
        match self {
            Self::Value => 1 << 0,
            Self::Selection => 1 << 1,
            Self::Focus => 1 << 2,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct AccessibilityNotifications(u8);

impl AccessibilityNotifications {
    const ORDERED: [AccessibilityNotification; 3] = [
        AccessibilityNotification::Value,
        AccessibilityNotification::Selection,
        AccessibilityNotification::Focus,
    ];

    pub(crate) fn insert(&mut self, notification: AccessibilityNotification) {
        self.0 |= notification.bit();
    }

    #[cfg(test)]
    pub(crate) fn extend(
        &mut self,
        notifications: impl IntoIterator<Item = AccessibilityNotification>,
    ) {
        for notification in notifications {
            self.insert(notification);
        }
    }

    pub(crate) fn contains(self, notification: AccessibilityNotification) -> bool {
        self.0 & notification.bit() != 0
    }

    pub(crate) fn without(mut self, notification: AccessibilityNotification) -> Self {
        self.0 &= !notification.bit();
        self
    }

    pub(crate) fn iter(self) -> impl Iterator<Item = AccessibilityNotification> {
        Self::ORDERED
            .into_iter()
            .filter(move |notification| self.contains(*notification))
    }

    #[cfg(test)]
    pub(crate) fn len(self) -> usize {
        self.iter().count()
    }

    pub(crate) fn is_empty(self) -> bool {
        self.0 == 0
    }

    pub(crate) fn take(&mut self) -> Self {
        std::mem::take(self)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct CellMapping {
    columns: Range<u16>,
    utf16: Range<usize>,
    reference: AccessibilityCellRef,
}

#[derive(Debug)]
struct AccessibilityRow {
    id: AccessibilityRowId,
    revision: u64,
    text: Arc<str>,
    utf16_bytes: Arc<[usize]>,
    cells: Arc<[CellMapping]>,
    soft_wrapped: bool,
    len_utf16: usize,
}

impl AccessibilityRow {
    fn from_update(update: AccessibilityRowUpdate) -> Arc<Self> {
        let mut text = String::new();
        let mut mappings = Vec::with_capacity(update.cells.len());
        let mut utf16 = 0_usize;
        let mut next_column = 0_u16;
        for cell in update.cells {
            let column = cell.column.max(next_column);
            let start = utf16;
            text.push_str(&cell.text);
            utf16 = utf16.saturating_add(cell.text.encode_utf16().count());
            let end = utf16;
            let column_end = column.saturating_add(cell.width_cells);
            mappings.push(CellMapping {
                columns: column..column_end,
                utf16: start..end,
                reference: AccessibilityCellRef {
                    row: update.id,
                    row_revision: update.revision,
                    column,
                },
            });
            next_column = column_end;
        }
        let utf16_bytes = Arc::from(utf16_byte_index(&text, utf16));
        Arc::new(Self {
            id: update.id,
            revision: update.revision,
            text: Arc::from(text),
            utf16_bytes,
            cells: Arc::from(mappings),
            soft_wrapped: update.soft_wrapped,
            len_utf16: utf16,
        })
    }

    fn byte_for_utf16(&self, index: usize) -> Option<usize> {
        self.utf16_bytes
            .get(index)
            .copied()
            .filter(|byte| *byte != usize::MAX)
    }
}

#[derive(Debug)]
struct AccessibilityRowBlock {
    screen_generation: usize,
    node_serial: u64,
    rows: Arc<[Arc<AccessibilityRow>]>,
    utf16_prefix: Arc<[usize]>,
    len_utf16: usize,
}

impl AccessibilityRowBlock {
    fn new(
        rows: Vec<Arc<AccessibilityRow>>,
        document_last_row: usize,
        row_start: usize,
    ) -> Arc<Self> {
        let mut utf16_prefix = Vec::with_capacity(rows.len() + 1);
        utf16_prefix.push(0);
        let mut len_utf16 = 0_usize;
        for (index, row) in rows.iter().enumerate() {
            len_utf16 = len_utf16.saturating_add(row.len_utf16);
            if row_start + index != document_last_row && !row.soft_wrapped {
                len_utf16 = len_utf16.saturating_add(1);
            }
            utf16_prefix.push(len_utf16);
        }
        let (screen_generation, node_serial) = rows
            .first()
            .map(|row| (row.id.screen_generation, row.id.node_serial))
            .unwrap_or_default();
        Arc::new(Self {
            screen_generation,
            node_serial,
            rows: Arc::from(rows),
            utf16_prefix: Arc::from(utf16_prefix),
            len_utf16,
        })
    }

    fn shares_rows(&self, rows: &[Arc<AccessibilityRow>], is_document_tail: bool) -> bool {
        self.rows.len() == rows.len()
            && self
                .rows
                .iter()
                .zip(rows)
                .all(|(left, right)| Arc::ptr_eq(left, right))
            && self.rows.last().is_none_or(|row| {
                let expected = row.len_utf16 + usize::from(!is_document_tail && !row.soft_wrapped);
                self.utf16_prefix[self.rows.len()]
                    - self.utf16_prefix[self.rows.len().saturating_sub(1)]
                    == expected
            })
    }
}

#[derive(Debug)]
struct AccessibilityDocument {
    blocks: Arc<[Arc<AccessibilityRowBlock>]>,
    block_row_prefix: Arc<[usize]>,
    block_utf16_prefix: Arc<[usize]>,
    rows: usize,
    len_utf16: usize,
    materialized: OnceLock<String>,
}

impl AccessibilityDocument {
    fn empty() -> Arc<Self> {
        Arc::new(Self {
            blocks: Arc::from([]),
            block_row_prefix: Arc::from([0]),
            block_utf16_prefix: Arc::from([0]),
            rows: 0,
            len_utf16: 0,
            materialized: OnceLock::new(),
        })
    }

    fn row(&self, line: usize) -> Option<(&AccessibilityRow, usize)> {
        if line >= self.rows {
            return None;
        }
        let block_index = self
            .block_row_prefix
            .partition_point(|start| *start <= line)
            .checked_sub(1)?
            .min(self.blocks.len().checked_sub(1)?);
        let block = self.blocks.get(block_index)?;
        let local = line.checked_sub(self.block_row_prefix[block_index])?;
        Some((block.rows.get(local)?, block_index))
    }

    fn row_start(&self, line: usize) -> Option<usize> {
        let (_, block_index) = self.row(line)?;
        let local = line - self.block_row_prefix[block_index];
        Some(self.block_utf16_prefix[block_index] + self.blocks[block_index].utf16_prefix[local])
    }

    fn range_for_line(&self, line: usize) -> Option<Range<usize>> {
        let (row, block_index) = self.row(line)?;
        let local = line - self.block_row_prefix[block_index];
        let start =
            self.block_utf16_prefix[block_index] + self.blocks[block_index].utf16_prefix[local];
        let end = start + row.len_utf16 + usize::from(line + 1 < self.rows && !row.soft_wrapped);
        Some(start..end)
    }

    fn line_for_index(&self, index: usize) -> Option<usize> {
        if self.rows == 0 || index > self.len_utf16 {
            return None;
        }
        if index == self.len_utf16 {
            return Some(self.rows - 1);
        }
        let block_index = self
            .block_utf16_prefix
            .partition_point(|start| *start <= index)
            .checked_sub(1)?
            .min(self.blocks.len().checked_sub(1)?);
        let block = &self.blocks[block_index];
        let local_index = index - self.block_utf16_prefix[block_index];
        let local_row = block
            .utf16_prefix
            .partition_point(|start| *start <= local_index)
            .checked_sub(1)?
            .min(block.rows.len().checked_sub(1)?);
        Some(self.block_row_prefix[block_index] + local_row)
    }

    fn row_for_id(&self, id: AccessibilityRowId) -> Option<(usize, &AccessibilityRow)> {
        for (block_index, block) in self.blocks.iter().enumerate() {
            if block.screen_generation != id.screen_generation
                || block.node_serial != id.node_serial
            {
                continue;
            }
            if let Some(local) = block.rows.iter().position(|row| row.id == id) {
                return Some((
                    self.block_row_prefix[block_index] + local,
                    &block.rows[local],
                ));
            }
        }
        None
    }

    fn is_text_boundary(&self, index: usize) -> bool {
        if index == self.len_utf16 {
            return true;
        }
        let Some(line) = self.line_for_index(index) else {
            return false;
        };
        let Some((row, _)) = self.row(line) else {
            return false;
        };
        let Some(start) = self.row_start(line) else {
            return false;
        };
        let local = index.saturating_sub(start);
        local >= row.len_utf16
            || row
                .cells
                .iter()
                .any(|cell| cell.utf16.start == local || cell.utf16.end == local)
    }

    fn text_for_range(&self, range: Range<usize>) -> Option<String> {
        if range.start > range.end
            || range.end > self.len_utf16
            || !self.is_text_boundary(range.start)
            || !self.is_text_boundary(range.end)
        {
            return None;
        }
        if range.is_empty() {
            return Some(String::new());
        }
        let first = self.line_for_index(range.start)?;
        let last = self.line_for_index(range.end.saturating_sub(1))?;
        let mut text = String::new();
        for line in first..=last {
            let (row, _) = self.row(line)?;
            let row_start = self.row_start(line)?;
            let text_end = row_start + row.len_utf16;
            let local_start = range.start.saturating_sub(row_start).min(row.len_utf16);
            let local_end = range.end.saturating_sub(row_start).min(row.len_utf16);
            if local_start < local_end {
                let byte_start = row.byte_for_utf16(local_start)?;
                let byte_end = row.byte_for_utf16(local_end)?;
                text.push_str(row.text.get(byte_start..byte_end)?);
            }
            if line + 1 < self.rows
                && !row.soft_wrapped
                && range.start <= text_end
                && range.end > text_end
            {
                text.push('\n');
            }
        }
        Some(text)
    }

    fn text(&self) -> &str {
        self.materialized
            .get_or_init(|| self.text_for_range(0..self.len_utf16).unwrap_or_default())
    }
}

#[derive(Debug)]
struct TerminalAccessibilityData {
    generation: PresentationGeneration,
    content_revision: u64,
    screen: AccessibilityScreen,
    screen_generation: usize,
    document: Arc<AccessibilityDocument>,
    visible_lines: Range<usize>,
    visible_range: Range<usize>,
    selection: Option<Range<usize>>,
    cursor: Range<usize>,
    cursor_cell: Option<(usize, u16)>,
}

struct RetainedAccessibilityRequest {
    generation: PresentationGeneration,
    content_revision: u64,
    screen: AccessibilityScreen,
    screen_generation: usize,
    document: Arc<AccessibilityDocument>,
    visible_lines: Range<usize>,
    cursor: Option<AccessibilityCellRef>,
    selection: Option<AccessibilitySelectionRefs>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct AccessibilitySelectionRequest {
    pub(crate) generation: PresentationGeneration,
    pub(crate) content_revision: u64,
    pub(crate) screen: AccessibilityScreen,
    pub(crate) range: Range<usize>,
}

#[derive(Clone, Debug)]
pub(crate) struct TerminalAccessibilityModel {
    data: Arc<TerminalAccessibilityData>,
}

impl TerminalAccessibilityModel {
    pub(crate) fn from_screen(screen: &super::ScreenSnapshot) -> Self {
        let lines = screen
            .rows
            .iter()
            .enumerate()
            .map(|(row_index, row)| {
                let mut cells = Vec::with_capacity(row.len());
                for (index, cell) in row.iter().enumerate() {
                    if cell.spacer_tail {
                        continue;
                    }
                    let width = if row.get(index + 1).is_some_and(|next| next.spacer_tail) {
                        2
                    } else {
                        1
                    };
                    cells.push(AccessibilityCell::new(
                        cell.text.clone(),
                        width,
                        cell.selected,
                    ));
                }
                AccessibilityLine::new(
                    cells,
                    screen
                        .row_soft_wrapped
                        .get(row_index)
                        .copied()
                        .unwrap_or(false),
                )
            })
            .collect::<Vec<_>>();
        let cursor = screen
            .cursor
            .position
            .map(|position| (usize::from(position.row), position.column));
        Self::new_with_generation(lines, 0..screen.rows.len(), cursor, screen.generation)
    }

    #[cfg(test)]
    pub(crate) fn new(
        lines: Vec<AccessibilityLine>,
        visible_lines: Range<usize>,
        cursor: Option<(usize, u16)>,
    ) -> Self {
        Self::new_with_generation(
            lines,
            visible_lines,
            cursor,
            PresentationGeneration::default(),
        )
    }

    fn new_with_generation(
        lines: Vec<AccessibilityLine>,
        visible_lines: Range<usize>,
        cursor: Option<(usize, u16)>,
        generation: PresentationGeneration,
    ) -> Self {
        let mut selected = Vec::new();
        let mut rows = BTreeMap::new();
        let mut topology = Vec::with_capacity(lines.len());
        for (line_index, line) in lines.into_iter().enumerate() {
            let id = AccessibilityRowId {
                screen: AccessibilityScreen::Primary,
                screen_generation: 0,
                node_serial: 1,
                page_row: u16::try_from(line_index).unwrap_or(u16::MAX),
            };
            let mut column = 0_u16;
            let cells = line
                .cells
                .into_iter()
                .map(|mut cell| {
                    cell.column = column;
                    column = column.saturating_add(cell.width_cells);
                    if cell.selected {
                        selected.push((id, cell.column));
                    }
                    cell
                })
                .collect();
            rows.insert(
                id,
                AccessibilityRow::from_update(AccessibilityRowUpdate {
                    id,
                    revision: 1,
                    soft_wrapped: line.soft_wrapped,
                    cells,
                }),
            );
            topology.push(id);
        }
        let document = build_document(&topology, &rows, None);
        let visible_lines =
            visible_lines.start.min(document.rows)..visible_lines.end.min(document.rows);
        let visible_range = visible_range(&document, visible_lines.clone());
        let selection = selected.first().zip(selected.last()).and_then(
            |((start_row, start_col), (end_row, end_col))| {
                let start = AccessibilityCellRef {
                    row: *start_row,
                    row_revision: 1,
                    column: *start_col,
                };
                let end = AccessibilityCellRef {
                    row: *end_row,
                    row_revision: 1,
                    column: *end_col,
                };
                range_for_refs(&document, start, end)
            },
        );
        let cursor_cell = cursor.and_then(|(line, column)| {
            let (row, _) = document.row(line)?;
            let column = row
                .cells
                .iter()
                .find(|cell| cell.columns.contains(&column))
                .map_or(column, |cell| cell.columns.start);
            Some((line, column))
        });
        let cursor = cursor
            .and_then(|(line, column)| {
                let (row, _) = document.row(line)?;
                offset_for_column(&document, line, row, column, false)
            })
            .unwrap_or(visible_range.start);
        Self {
            data: Arc::new(TerminalAccessibilityData {
                generation,
                content_revision: 1,
                screen: AccessibilityScreen::Primary,
                screen_generation: 0,
                document,
                visible_lines,
                visible_range,
                selection,
                cursor: cursor..cursor,
                cursor_cell,
            }),
        }
    }

    fn from_retained(request: RetainedAccessibilityRequest) -> Self {
        let RetainedAccessibilityRequest {
            generation,
            content_revision,
            screen,
            screen_generation,
            document,
            visible_lines,
            cursor,
            selection,
        } = request;
        let visible_lines =
            visible_lines.start.min(document.rows)..visible_lines.end.min(document.rows);
        let visible_range = visible_range(&document, visible_lines.clone());
        let cursor_cell = cursor.and_then(|reference| {
            let (line, row) = document.row_for_id(reference.row)?;
            (row.revision == reference.row_revision).then_some(())?;
            let column = row
                .cells
                .iter()
                .find(|cell| cell.columns.contains(&reference.column))
                .map_or(reference.column, |cell| cell.columns.start);
            Some((line, column))
        });
        let cursor = cursor
            .and_then(|reference| offset_for_ref(&document, reference, false))
            .unwrap_or(visible_range.start);
        let selection = selection.and_then(|selection| {
            // AppKit exposes one contiguous UTF-16 range. A terminal rectangle therefore uses
            // the smallest contiguous range containing its ordered endpoints.
            let _rectangle = selection.rectangle;
            range_for_refs(&document, selection.start, selection.end)
        });
        Self {
            data: Arc::new(TerminalAccessibilityData {
                generation,
                content_revision,
                screen,
                screen_generation,
                document,
                visible_lines,
                visible_range,
                selection,
                cursor: cursor..cursor,
                cursor_cell,
            }),
        }
    }

    #[cfg(all(target_os = "macos", not(test)))]
    pub(crate) fn shares_snapshot(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.data, &other.data)
    }

    pub(crate) fn shares_document(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.data.document, &other.data.document)
    }

    pub(crate) fn active_screen(&self) -> AccessibilityScreen {
        self.data.screen
    }

    pub(crate) fn text(&self) -> &str {
        self.data.document.text()
    }

    pub(crate) fn len_utf16(&self) -> usize {
        self.data.document.len_utf16
    }

    pub(crate) fn visible_range(&self) -> Range<usize> {
        self.data.visible_range.clone()
    }

    pub(crate) fn selection_range(&self) -> Option<Range<usize>> {
        self.data.selection.clone()
    }

    pub(crate) fn cursor_range(&self) -> Range<usize> {
        self.data.cursor.clone()
    }

    pub(crate) fn selected_or_cursor_range(&self) -> Range<usize> {
        self.data
            .selection
            .clone()
            .unwrap_or_else(|| self.data.cursor.clone())
    }

    pub(crate) fn range_for_line(&self, line: usize) -> Option<Range<usize>> {
        self.data.document.range_for_line(line)
    }

    pub(crate) fn line_for_index(&self, index: usize) -> Option<usize> {
        self.data.document.line_for_index(index)
    }

    pub(crate) fn range_for_index(&self, index: usize) -> Option<Range<usize>> {
        if index >= self.len_utf16() {
            return None;
        }
        let line = self.line_for_index(index)?;
        let row_start = self.data.document.row_start(line)?;
        let (row, _) = self.data.document.row(line)?;
        let local = index.saturating_sub(row_start);
        row.cells
            .iter()
            .find(|cell| cell.utf16.contains(&local))
            .map(|cell| row_start + cell.utf16.start..row_start + cell.utf16.end)
            .or_else(|| (index < self.len_utf16()).then_some(index..index + 1))
    }

    pub(crate) fn text_for_range(&self, range: Range<usize>) -> Option<String> {
        self.data.document.text_for_range(range)
    }

    pub(crate) fn selection_request(
        &self,
        range: Range<usize>,
    ) -> Option<AccessibilitySelectionRequest> {
        if range.start > range.end || range.end > self.len_utf16() {
            return None;
        }
        let range = if range.is_empty() {
            range
        } else {
            let (start, end) = self.resolve_selection_range(range)?;
            range_for_refs(&self.data.document, start, end)?
        };
        Some(AccessibilitySelectionRequest {
            generation: self.data.generation,
            content_revision: self.data.content_revision,
            screen: self.data.screen,
            range,
        })
    }

    fn resolve_selection_range(
        &self,
        range: Range<usize>,
    ) -> Option<(AccessibilityCellRef, AccessibilityCellRef)> {
        if range.is_empty() {
            return None;
        }
        let mut first = None;
        let mut last = None;
        let first_line = self.line_for_index(range.start)?;
        let last_line = self.line_for_index(range.end.saturating_sub(1))?;
        for line in first_line..=last_line {
            let row_start = self.data.document.row_start(line)?;
            let (row, _) = self.data.document.row(line)?;
            for cell in row.cells.iter() {
                let cell_range = row_start + cell.utf16.start..row_start + cell.utf16.end;
                if cell_range.start < range.end && cell_range.end > range.start {
                    first.get_or_insert(cell.reference);
                    last = Some(cell.reference);
                }
            }
        }
        first.zip(last)
    }

    pub(crate) fn bounds_for_range(
        &self,
        range: Range<usize>,
        geometry: AccessibilityGeometry,
    ) -> Option<(f32, f32, f32, f32)> {
        if range.start > range.end
            || range.start < self.data.visible_range.start
            || range.end > self.data.visible_range.end
        {
            return None;
        }
        if range.is_empty() {
            let (line, cursor_column) = if range == self.data.cursor {
                match self.data.cursor_cell {
                    Some((line, column)) => (line, Some(column)),
                    None => (self.line_for_index(range.start)?, None),
                }
            } else {
                (self.line_for_index(range.start)?, None)
            };
            let visible_row = line.checked_sub(self.data.visible_lines.start)?;
            (line < self.data.visible_lines.end).then_some(())?;
            let row_start = self.data.document.row_start(line)?;
            let (row, _) = self.data.document.row(line)?;
            let local = range.start.saturating_sub(row_start);
            let column = cursor_column.unwrap_or_else(|| {
                row.cells
                    .iter()
                    .find_map(|cell| {
                        if cell.utf16.contains(&local) {
                            Some(cell.columns.start)
                        } else if cell.utf16.end == local {
                            Some(cell.columns.end)
                        } else {
                            None
                        }
                    })
                    .unwrap_or(0)
            });
            return Some((
                geometry.origin_x + f32::from(column) * geometry.cell_width,
                geometry.origin_y + visible_row as f32 * geometry.line_height,
                0.0,
                geometry.line_height,
            ));
        }
        let mut first_line = usize::MAX;
        let mut last_line = 0_usize;
        let mut first_column = u16::MAX;
        let mut last_column = 0_u16;
        let mut found = false;
        for line in self.data.visible_lines.clone() {
            let row_start = self.data.document.row_start(line)?;
            let (row, _) = self.data.document.row(line)?;
            for cell in row.cells.iter() {
                let cell_range = row_start + cell.utf16.start..row_start + cell.utf16.end;
                if cell_range.start < range.end && cell_range.end > range.start {
                    found = true;
                    first_line = first_line.min(line);
                    last_line = last_line.max(line);
                    first_column = first_column.min(cell.columns.start);
                    last_column = last_column.max(cell.columns.end);
                }
            }
        }
        if !found {
            return self.bounds_for_range(range.start..range.start, geometry);
        }
        Some((
            geometry.origin_x + f32::from(first_column) * geometry.cell_width,
            geometry.origin_y
                + (first_line - self.data.visible_lines.start) as f32 * geometry.line_height,
            f32::from(last_column - first_column) * geometry.cell_width,
            (last_line - first_line + 1) as f32 * geometry.line_height,
        ))
    }

    pub(crate) fn index_for_point(
        &self,
        x: f32,
        y: f32,
        geometry: AccessibilityGeometry,
    ) -> Option<usize> {
        if x < geometry.origin_x || y < geometry.origin_y {
            return None;
        }
        let line = self.data.visible_lines.start
            + ((y - geometry.origin_y) / geometry.line_height).floor() as usize;
        if line >= self.data.visible_lines.end {
            return None;
        }
        let column = ((x - geometry.origin_x) / geometry.cell_width).floor() as u16;
        let row_start = self.data.document.row_start(line)?;
        let (row, _) = self.data.document.row(line)?;
        let cell = row.cells.partition_point(|cell| cell.columns.end <= column);
        row.cells
            .get(cell)
            .filter(|cell| cell.columns.contains(&column))
            .map(|cell| row_start + cell.utf16.start)
    }

    pub(crate) fn range_for_point(
        &self,
        x: f32,
        y: f32,
        geometry: AccessibilityGeometry,
    ) -> Option<Range<usize>> {
        self.range_for_index(self.index_for_point(x, y, geometry)?)
    }
}

#[derive(Debug)]
struct AccessibilityScreenState {
    topology: Arc<[AccessibilityRowId]>,
    rows: BTreeMap<AccessibilityRowId, Arc<AccessibilityRow>>,
    document: Arc<AccessibilityDocument>,
    content_revision: u64,
}

impl Default for AccessibilityScreenState {
    fn default() -> Self {
        Self {
            topology: Arc::from([]),
            rows: BTreeMap::new(),
            document: AccessibilityDocument::empty(),
            content_revision: 0,
        }
    }
}

#[derive(Debug, Default)]
pub(crate) struct TerminalAccessibilityState {
    primary: AccessibilityScreenState,
    alternate: AccessibilityScreenState,
    latest: Option<TerminalAccessibilityModel>,
    active_screen: Option<AccessibilityScreen>,
    active_screen_generation: usize,
    pending_semantic_target: bool,
}

impl TerminalAccessibilityState {
    pub(crate) fn apply(
        &mut self,
        update: AccessibilityUpdate,
        generation: PresentationGeneration,
    ) -> Option<Arc<TerminalAccessibilityModel>> {
        if update.complete == update.more {
            return None;
        }
        self.pending_semantic_target = true;
        self.active_screen = Some(update.screen);
        self.active_screen_generation = update.screen_generation;
        let state = match update.screen {
            AccessibilityScreen::Primary => &mut self.primary,
            AccessibilityScreen::Alternate => &mut self.alternate,
        };
        let mut document_dirty = false;
        if let Some(topology) = update.topology.as_ref() {
            if topology.iter().any(|id| {
                id.screen != update.screen || id.screen_generation != update.screen_generation
            }) {
                return None;
            }
            let live = topology.iter().copied().collect::<BTreeSet<_>>();
            state.rows.retain(|id, _| live.contains(id));
            document_dirty = state.topology.as_ref() != topology.as_slice();
            state.topology = Arc::from(topology.clone());
        }
        for changed in update.changed_rows {
            if changed.id.screen != update.screen
                || changed.id.screen_generation != update.screen_generation
                || !state.topology.contains(&changed.id)
            {
                continue;
            }
            let replacement = AccessibilityRow::from_update(changed);
            let existing = state.rows.get(&replacement.id);
            document_dirty |= existing.is_none_or(|existing| {
                existing.revision != replacement.revision
                    || existing.soft_wrapped != replacement.soft_wrapped
                    || existing.text != replacement.text
                    || existing.cells != replacement.cells
            });
            let replacement = state
                .rows
                .get(&replacement.id)
                .filter(|existing| {
                    existing.revision == replacement.revision
                        && existing.soft_wrapped == replacement.soft_wrapped
                        && existing.text == replacement.text
                        && existing.cells == replacement.cells
                })
                .cloned()
                .unwrap_or(replacement);
            state.rows.insert(replacement.id, replacement);
        }
        if !update.complete || state.topology.iter().any(|id| !state.rows.contains_key(id)) {
            return None;
        }

        let document = if document_dirty {
            build_document(&state.topology, &state.rows, Some(&state.document))
        } else {
            Arc::clone(&state.document)
        };
        let document_changed = !Arc::ptr_eq(&document, &state.document);
        if document_changed {
            state.content_revision = state.content_revision.saturating_add(1);
            state.document = Arc::clone(&document);
        }
        self.pending_semantic_target = false;
        if !document_changed
            && self.latest.as_ref().is_some_and(|latest| {
                latest.data.screen == update.screen
                    && latest.data.generation == generation
                    && latest.data.visible_lines == update.visible_lines
                    && latest.data.cursor
                        == update
                            .cursor
                            .and_then(|reference| offset_for_ref(&document, reference, false))
                            .map_or_else(
                                || latest.data.visible_range.start..latest.data.visible_range.start,
                                |offset| offset..offset,
                            )
                    && latest.data.selection
                        == update.selection.and_then(|selection| {
                            range_for_refs(&document, selection.start, selection.end)
                        })
            })
        {
            return None;
        }
        let model = TerminalAccessibilityModel::from_retained(RetainedAccessibilityRequest {
            generation,
            content_revision: state.content_revision,
            screen: update.screen,
            screen_generation: update.screen_generation,
            document,
            visible_lines: update.visible_lines,
            cursor: update.cursor,
            selection: update.selection,
        });
        self.latest = Some(model.clone());
        Some(Arc::new(model))
    }

    pub(crate) fn resolve_selection(
        &self,
        request: &AccessibilitySelectionRequest,
    ) -> Option<Option<(AccessibilityCellRef, AccessibilityCellRef)>> {
        let model = self.latest.as_ref()?;
        if self.pending_semantic_target
            || request.generation != model.data.generation
            || request.content_revision != model.data.content_revision
            || request.screen != model.data.screen
            || self.active_screen != Some(request.screen)
            || self.active_screen_generation != model.data.screen_generation
        {
            return None;
        }
        if request.range.start > request.range.end || request.range.end > model.len_utf16() {
            return None;
        }
        if request.range.is_empty() {
            return Some(None);
        }
        model
            .resolve_selection_range(request.range.clone())
            .map(Some)
    }
}

fn build_document(
    topology: &[AccessibilityRowId],
    rows: &BTreeMap<AccessibilityRowId, Arc<AccessibilityRow>>,
    previous: Option<&Arc<AccessibilityDocument>>,
) -> Arc<AccessibilityDocument> {
    if topology.is_empty() {
        return previous
            .filter(|document| document.rows == 0)
            .cloned()
            .unwrap_or_else(AccessibilityDocument::empty);
    }
    let Some(ordered) = topology
        .iter()
        .map(|id| rows.get(id).cloned())
        .collect::<Option<Vec<_>>>()
    else {
        return previous
            .cloned()
            .unwrap_or_else(AccessibilityDocument::empty);
    };
    if let Some(previous) = previous
        && previous.rows == ordered.len()
        && previous
            .blocks
            .iter()
            .flat_map(|block| block.rows.iter())
            .zip(&ordered)
            .all(|(left, right)| Arc::ptr_eq(left, right))
    {
        return Arc::clone(previous);
    }

    let mut groups: Vec<Vec<Arc<AccessibilityRow>>> = Vec::new();
    for row in ordered {
        if groups
            .last()
            .and_then(|group| group.last())
            .is_none_or(|last| {
                last.id.screen_generation != row.id.screen_generation
                    || last.id.node_serial != row.id.node_serial
            })
        {
            groups.push(Vec::new());
        }
        groups.last_mut().expect("a row group exists").push(row);
    }
    let previous_blocks = previous
        .into_iter()
        .flat_map(|document| document.blocks.iter())
        .map(|block| {
            (
                (block.screen_generation, block.node_serial),
                Arc::clone(block),
            )
        })
        .collect::<BTreeMap<_, _>>();
    let last_row = topology.len() - 1;
    let mut row_start = 0_usize;
    let mut blocks = Vec::with_capacity(groups.len());
    for group in groups {
        let key = (group[0].id.screen_generation, group[0].id.node_serial);
        let is_tail = row_start + group.len() == topology.len();
        let block = previous_blocks
            .get(&key)
            .filter(|block| block.shares_rows(&group, is_tail))
            .cloned()
            .unwrap_or_else(|| AccessibilityRowBlock::new(group, last_row, row_start));
        row_start += block.rows.len();
        blocks.push(block);
    }
    let mut block_row_prefix = Vec::with_capacity(blocks.len() + 1);
    let mut block_utf16_prefix = Vec::with_capacity(blocks.len() + 1);
    block_row_prefix.push(0);
    block_utf16_prefix.push(0);
    let mut total_rows = 0_usize;
    let mut total_utf16 = 0_usize;
    for block in &blocks {
        total_rows += block.rows.len();
        total_utf16 += block.len_utf16;
        block_row_prefix.push(total_rows);
        block_utf16_prefix.push(total_utf16);
    }
    Arc::new(AccessibilityDocument {
        blocks: Arc::from(blocks),
        block_row_prefix: Arc::from(block_row_prefix),
        block_utf16_prefix: Arc::from(block_utf16_prefix),
        rows: total_rows,
        len_utf16: total_utf16,
        materialized: OnceLock::new(),
    })
}

fn visible_range(document: &AccessibilityDocument, visible_lines: Range<usize>) -> Range<usize> {
    if visible_lines.is_empty() {
        return 0..0;
    }
    let start = document.row_start(visible_lines.start).unwrap_or(0);
    let end = document
        .range_for_line(visible_lines.end - 1)
        .map_or(start, |range| range.end);
    start..end
}

fn offset_for_column(
    document: &AccessibilityDocument,
    line: usize,
    row: &AccessibilityRow,
    column: u16,
    inclusive_end: bool,
) -> Option<usize> {
    let row_start = document.row_start(line)?;
    if let Some(cell) = row.cells.iter().find(|cell| cell.columns.contains(&column)) {
        return Some(
            row_start
                + if inclusive_end {
                    cell.utf16.end
                } else {
                    cell.utf16.start
                },
        );
    }
    row.cells
        .iter()
        .find(|cell| cell.columns.end == column)
        .map_or(Some(row_start + row.len_utf16), |cell| {
            Some(row_start + cell.utf16.end)
        })
}

fn offset_for_ref(
    document: &AccessibilityDocument,
    reference: AccessibilityCellRef,
    inclusive_end: bool,
) -> Option<usize> {
    let (line, row) = document.row_for_id(reference.row)?;
    (row.revision == reference.row_revision).then_some(())?;
    offset_for_column(document, line, row, reference.column, inclusive_end)
}

fn range_for_refs(
    document: &AccessibilityDocument,
    start: AccessibilityCellRef,
    end: AccessibilityCellRef,
) -> Option<Range<usize>> {
    let start_begin = offset_for_ref(document, start, false)?;
    let start_end = offset_for_ref(document, start, true)?;
    let end_begin = offset_for_ref(document, end, false)?;
    let end_end = offset_for_ref(document, end, true)?;
    Some(start_begin.min(end_begin)..start_end.max(end_end))
}

fn utf16_byte_index(text: &str, len_utf16: usize) -> Vec<usize> {
    let mut index = vec![usize::MAX; len_utf16.saturating_add(1)];
    let mut utf16 = 0_usize;
    index[0] = 0;
    for (byte, character) in text.char_indices() {
        index[utf16] = byte;
        utf16 += character.len_utf16();
        index[utf16] = byte + character.len_utf8();
    }
    index
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cell(text: &str, width_cells: u16, selected: bool) -> AccessibilityCell {
        AccessibilityCell::new(text, width_cells, selected)
    }

    #[test]
    fn utf16_ranges_preserve_wide_combining_emoji_and_soft_wraps() {
        let model = TerminalAccessibilityModel::new(
            vec![
                AccessibilityLine::new(vec![cell("A\u{301}", 1, false), cell("😀", 2, true)], true),
                AccessibilityLine::new(vec![cell("界", 2, true), cell("x", 1, false)], false),
                AccessibilityLine::new(vec![cell("scrollback", 10, false)], false),
            ],
            1..3,
            Some((1, 2)),
        );

        assert_eq!(model.text(), "A\u{301}😀界x\nscrollback");
        assert_eq!(model.range_for_line(0), Some(0..4));
        assert_eq!(model.range_for_line(1), Some(4..7));
        assert_eq!(model.selection_range(), Some(2..5));
        assert_eq!(model.cursor_range(), 5..5);
        assert_eq!(model.visible_range(), 4..17);
    }

    #[test]
    fn line_and_range_conversion_are_stable_at_utf16_boundaries() {
        let model = TerminalAccessibilityModel::new(
            vec![
                AccessibilityLine::new(vec![cell("😀", 2, false), cell("a", 1, false)], false),
                AccessibilityLine::new(vec![cell("b", 1, false)], false),
            ],
            0..2,
            None,
        );
        assert_eq!(model.line_for_index(0), Some(0));
        assert_eq!(model.line_for_index(2), Some(0));
        assert_eq!(model.line_for_index(4), Some(1));
        assert_eq!(model.range_for_index(3), Some(3..4));
        assert_eq!(model.range_for_index(5), None);
        assert_eq!(model.text_for_range(1..2), None);
        assert_eq!(model.text_for_range(0..2), Some("😀".to_owned()));
    }

    #[test]
    fn string_ranges_reject_partial_combining_and_zwj_terminal_graphemes() {
        let model = TerminalAccessibilityModel::new(
            vec![AccessibilityLine::new(
                vec![
                    cell("A\u{301}", 1, false),
                    cell("👨\u{200d}👩\u{200d}👧\u{200d}👦", 2, false),
                    cell("x", 1, false),
                ],
                false,
            )],
            0..1,
            None,
        );

        assert_eq!(model.range_for_index(1), Some(0..2));
        assert_eq!(model.range_for_index(12), Some(2..13));
        assert_eq!(model.text_for_range(0..1), None);
        assert_eq!(model.text_for_range(1..2), None);
        assert_eq!(model.text_for_range(2..4), None);
        assert_eq!(model.text_for_range(4..5), None);
        assert_eq!(model.text_for_range(0..2), Some("A\u{301}".to_owned()));
        assert_eq!(
            model.text_for_range(2..13),
            Some("👨\u{200d}👩\u{200d}👧\u{200d}👦".to_owned())
        );
    }

    #[test]
    fn range_for_index_rejects_the_document_end_and_out_of_bounds_indices() {
        let model = TerminalAccessibilityModel::new(
            vec![AccessibilityLine::new(vec![cell("😀", 2, false)], false)],
            0..1,
            None,
        );

        assert_eq!(model.range_for_index(2), None);
        assert_eq!(model.range_for_index(3), None);
        let reversed = 2..model.len_utf16().saturating_sub(1);
        assert_eq!(model.text_for_range(reversed.clone()), None);
        assert_eq!(model.text_for_range(0..3), None);
        assert_eq!(model.selection_request(reversed), None);
        assert_eq!(model.selection_request(0..3), None);
    }

    #[test]
    fn family_zwj_partial_ranges_keep_selection_bounds_and_hit_testing_cell_atomic() {
        let model = TerminalAccessibilityModel::new(
            vec![AccessibilityLine::new(
                vec![cell("👨\u{200d}👩\u{200d}👧\u{200d}👦", 2, false)],
                false,
            )],
            0..1,
            None,
        );
        let geometry = AccessibilityGeometry::new(4.0, 8.0, 10.0, 20.0).unwrap();

        assert_eq!(model.selection_request(4..5).unwrap().range, 0..11);
        assert_eq!(
            model.bounds_for_range(4..5, geometry),
            Some((4.0, 8.0, 20.0, 20.0))
        );
        assert_eq!(model.range_for_point(5.0, 9.0, geometry), Some(0..11));
        assert_eq!(model.range_for_point(15.0, 9.0, geometry), Some(0..11));
    }

    #[test]
    fn string_ranges_preserve_complete_wide_emoji_and_hard_line_boundaries() {
        let model = TerminalAccessibilityModel::new(
            vec![
                AccessibilityLine::new(vec![cell("界", 2, false)], false),
                AccessibilityLine::new(vec![cell("👩\u{200d}💻", 2, false)], false),
            ],
            0..2,
            None,
        );

        assert_eq!(model.text(), "界\n👩\u{200d}💻");
        assert_eq!(model.range_for_index(0), Some(0..1));
        assert_eq!(model.range_for_index(1), Some(1..2));
        assert_eq!(model.range_for_index(4), Some(2..7));
        assert_eq!(model.text_for_range(1..2), Some("\n".to_owned()));
        assert_eq!(model.text_for_range(2..7), Some("👩\u{200d}💻".to_owned()));
    }

    #[test]
    fn a_single_selected_cell_uses_the_cells_inclusive_end() {
        let model = TerminalAccessibilityModel::new(
            vec![AccessibilityLine::new(
                vec![
                    cell("a", 1, false),
                    cell("😀", 2, true),
                    cell("z", 1, false),
                ],
                false,
            )],
            0..1,
            None,
        );

        assert_eq!(model.selection_range(), Some(1..3));
    }

    #[test]
    fn bounds_and_hit_testing_share_logical_cell_geometry() {
        let model = TerminalAccessibilityModel::new(
            vec![AccessibilityLine::new(
                vec![cell("a", 1, false), cell("😀", 2, false)],
                false,
            )],
            0..1,
            None,
        );
        let geometry = AccessibilityGeometry::new(4.0, 8.0, 10.0, 20.0).unwrap();
        assert_eq!(
            model.bounds_for_range(1..3, geometry),
            Some((14.0, 8.0, 20.0, 20.0))
        );
        assert_eq!(model.index_for_point(24.0, 18.0, geometry), Some(1));
        assert_eq!(model.range_for_point(24.0, 18.0, geometry), Some(1..3));
        assert_eq!(
            model.bounds_for_range(3..3, geometry),
            Some((34.0, 8.0, 0.0, 20.0))
        );
    }

    #[test]
    fn range_bounds_cover_visible_rows_and_reject_retained_offscreen_text() {
        let retained = TerminalAccessibilityModel::new(
            vec![
                AccessibilityLine::new(vec![cell("old", 3, false)], false),
                AccessibilityLine::new(vec![cell("visible", 7, false)], false),
            ],
            1..2,
            None,
        );
        let geometry = AccessibilityGeometry::new(0.0, 0.0, 10.0, 20.0).unwrap();
        assert_eq!(retained.bounds_for_range(0..3, geometry), None);
        assert_eq!(
            retained.bounds_for_range(4..11, geometry),
            Some((0.0, 0.0, 70.0, 20.0))
        );
    }

    #[test]
    fn cursor_bounds_exist_on_an_empty_line() {
        let model = TerminalAccessibilityModel::new(
            vec![
                AccessibilityLine::new(Vec::new(), false),
                AccessibilityLine::new(Vec::new(), false),
            ],
            0..2,
            Some((1, 0)),
        );
        let geometry = AccessibilityGeometry::new(4.0, 8.0, 10.0, 20.0).unwrap();
        assert_eq!(model.cursor_range(), 1..1);
        assert_eq!(
            model.bounds_for_range(1..1, geometry),
            Some((4.0, 28.0, 0.0, 20.0))
        );
    }

    #[test]
    fn trimmed_blank_rows_keep_explicit_cursor_geometry() {
        let id = row_id(AccessibilityScreen::Primary, 1, 10, 0);
        let mut state = TerminalAccessibilityState::default();
        let model = state
            .apply(
                AccessibilityUpdate {
                    revision: 1,
                    screen: AccessibilityScreen::Primary,
                    screen_generation: 1,
                    complete: true,
                    more: false,
                    topology: Some(vec![id]),
                    visible_lines: 0..1,
                    cursor: Some(AccessibilityCellRef {
                        row: id,
                        row_revision: 1,
                        column: 5,
                    }),
                    selection: None,
                    changed_rows: vec![AccessibilityRowUpdate {
                        id,
                        revision: 1,
                        soft_wrapped: false,
                        cells: Vec::new(),
                    }],
                },
                PresentationGeneration::test(1),
            )
            .unwrap();
        let geometry = AccessibilityGeometry::new(0.0, 0.0, 10.0, 20.0).unwrap();

        assert_eq!(model.text(), "");
        assert_eq!(model.cursor_range(), 0..0);
        assert_eq!(
            model.bounds_for_range(0..0, geometry),
            Some((50.0, 0.0, 0.0, 20.0))
        );
    }

    fn row_id(
        screen: AccessibilityScreen,
        generation: usize,
        serial: u64,
        page_row: u16,
    ) -> AccessibilityRowId {
        AccessibilityRowId {
            screen,
            screen_generation: generation,
            node_serial: serial,
            page_row,
        }
    }

    fn update_row(
        id: AccessibilityRowId,
        revision: u64,
        text: &str,
        wrapped: bool,
    ) -> AccessibilityRowUpdate {
        AccessibilityRowUpdate {
            id,
            revision,
            soft_wrapped: wrapped,
            cells: text
                .chars()
                .enumerate()
                .map(|(column, ch)| AccessibilityCell::at_column(ch.to_string(), column as u16, 1))
                .collect(),
        }
    }

    #[test]
    fn retained_updates_publish_only_complete_documents_and_reuse_arc_rows() {
        let first_id = row_id(AccessibilityScreen::Primary, 1, 10, 0);
        let second_id = row_id(AccessibilityScreen::Primary, 1, 10, 1);
        let mut state = TerminalAccessibilityState::default();
        let pending = AccessibilityUpdate {
            revision: 1,
            screen: AccessibilityScreen::Primary,
            screen_generation: 1,
            complete: false,
            more: true,
            topology: Some(vec![first_id, second_id]),
            visible_lines: 0..2,
            cursor: None,
            selection: None,
            changed_rows: vec![update_row(first_id, 1, "old", false)],
        };
        assert!(
            state
                .apply(pending, PresentationGeneration::test(1))
                .is_none()
        );
        let complete = state
            .apply(
                AccessibilityUpdate {
                    revision: 2,
                    screen: AccessibilityScreen::Primary,
                    screen_generation: 1,
                    complete: true,
                    more: false,
                    topology: None,
                    visible_lines: 0..2,
                    cursor: None,
                    selection: None,
                    changed_rows: vec![update_row(second_id, 1, "new", false)],
                },
                PresentationGeneration::test(2),
            )
            .unwrap();
        assert_eq!(complete.text(), "old\nnew");
        let first_row = Arc::clone(state.primary.rows.get(&first_id).unwrap());
        let changed = state
            .apply(
                AccessibilityUpdate {
                    revision: 3,
                    screen: AccessibilityScreen::Primary,
                    screen_generation: 1,
                    complete: true,
                    more: false,
                    topology: None,
                    visible_lines: 0..2,
                    cursor: None,
                    selection: None,
                    changed_rows: vec![update_row(second_id, 2, "NEW", false)],
                },
                PresentationGeneration::test(3),
            )
            .unwrap();
        assert_eq!(changed.text(), "old\nNEW");
        assert!(Arc::ptr_eq(
            &first_row,
            state.primary.rows.get(&first_id).unwrap()
        ));
    }

    #[test]
    fn alternate_viewport_preserves_the_primary_retained_document() {
        let primary_id = row_id(AccessibilityScreen::Primary, 1, 10, 0);
        let alternate_id = row_id(AccessibilityScreen::Alternate, 2, 20, 0);
        let mut state = TerminalAccessibilityState::default();
        let primary = state
            .apply(
                AccessibilityUpdate {
                    revision: 1,
                    screen: AccessibilityScreen::Primary,
                    screen_generation: 1,
                    complete: true,
                    more: false,
                    topology: Some(vec![primary_id]),
                    visible_lines: 0..1,
                    cursor: None,
                    selection: None,
                    changed_rows: vec![update_row(primary_id, 1, "primary", false)],
                },
                PresentationGeneration::test(1),
            )
            .unwrap();
        let primary_document = Arc::clone(&state.primary.document);
        let alternate = state
            .apply(
                AccessibilityUpdate {
                    revision: 2,
                    screen: AccessibilityScreen::Alternate,
                    screen_generation: 2,
                    complete: true,
                    more: false,
                    topology: Some(vec![alternate_id]),
                    visible_lines: 0..1,
                    cursor: None,
                    selection: None,
                    changed_rows: vec![update_row(alternate_id, 1, "alternate", false)],
                },
                PresentationGeneration::test(2),
            )
            .unwrap();
        assert_eq!(primary.text(), "primary");
        assert_eq!(alternate.text(), "alternate");
        assert!(Arc::ptr_eq(&primary_document, &state.primary.document));
    }

    #[test]
    fn stale_utf16_selection_requests_are_generation_and_revision_bound() {
        let id = row_id(AccessibilityScreen::Primary, 1, 10, 0);
        let cell_ref = AccessibilityCellRef {
            row: id,
            row_revision: 1,
            column: 1,
        };
        let mut state = TerminalAccessibilityState::default();
        let model = state
            .apply(
                AccessibilityUpdate {
                    revision: 1,
                    screen: AccessibilityScreen::Primary,
                    screen_generation: 1,
                    complete: true,
                    more: false,
                    topology: Some(vec![id]),
                    visible_lines: 0..1,
                    cursor: Some(cell_ref),
                    selection: None,
                    changed_rows: vec![AccessibilityRowUpdate {
                        id,
                        revision: 1,
                        soft_wrapped: false,
                        cells: vec![
                            AccessibilityCell::at_column("A", 0, 1),
                            AccessibilityCell::at_column("😀", 1, 2),
                        ],
                    }],
                },
                PresentationGeneration::test(7),
            )
            .unwrap();
        let request = model.selection_request(1..3).unwrap();
        assert_eq!(
            state.resolve_selection(&request),
            Some(Some((cell_ref, cell_ref)))
        );
        let stale_generation = AccessibilitySelectionRequest {
            generation: PresentationGeneration::test(6),
            ..request.clone()
        };
        assert_eq!(state.resolve_selection(&stale_generation), None);

        let current = state
            .apply(
                AccessibilityUpdate {
                    revision: 2,
                    screen: AccessibilityScreen::Primary,
                    screen_generation: 1,
                    complete: true,
                    more: false,
                    topology: None,
                    visible_lines: 0..1,
                    cursor: Some(AccessibilityCellRef {
                        row_revision: 2,
                        ..cell_ref
                    }),
                    selection: None,
                    changed_rows: vec![AccessibilityRowUpdate {
                        id,
                        revision: 2,
                        soft_wrapped: false,
                        cells: vec![
                            AccessibilityCell::at_column("B", 0, 1),
                            AccessibilityCell::at_column("😀", 1, 2),
                        ],
                    }],
                },
                PresentationGeneration::test(8),
            )
            .unwrap();
        let stale_revision = AccessibilitySelectionRequest {
            generation: PresentationGeneration::test(8),
            ..request
        };
        assert_eq!(state.resolve_selection(&stale_revision), None);
        assert!(
            state
                .resolve_selection(&current.selection_request(1..2).unwrap())
                .is_some()
        );
    }

    #[test]
    fn selection_is_rejected_while_a_new_semantic_target_is_incomplete() {
        let id = row_id(AccessibilityScreen::Primary, 1, 10, 0);
        let mut state = TerminalAccessibilityState::default();
        let model = state
            .apply(
                AccessibilityUpdate {
                    revision: 1,
                    screen: AccessibilityScreen::Primary,
                    screen_generation: 1,
                    complete: true,
                    more: false,
                    topology: Some(vec![id]),
                    visible_lines: 0..1,
                    cursor: None,
                    selection: None,
                    changed_rows: vec![update_row(id, 1, "old", false)],
                },
                PresentationGeneration::test(1),
            )
            .unwrap();
        let request = model.selection_request(0..1).unwrap();

        assert!(
            state
                .apply(
                    AccessibilityUpdate {
                        revision: 2,
                        screen: AccessibilityScreen::Primary,
                        screen_generation: 1,
                        complete: false,
                        more: true,
                        topology: None,
                        visible_lines: 0..1,
                        cursor: None,
                        selection: None,
                        changed_rows: Vec::new(),
                    },
                    PresentationGeneration::test(2),
                )
                .is_none()
        );
        assert_eq!(state.resolve_selection(&request), None);
    }

    #[test]
    fn selection_is_rejected_after_the_active_accessibility_screen_changes() {
        let primary_id = row_id(AccessibilityScreen::Primary, 1, 10, 0);
        let alternate_id = row_id(AccessibilityScreen::Alternate, 2, 20, 0);
        let mut state = TerminalAccessibilityState::default();
        let primary = state
            .apply(
                AccessibilityUpdate {
                    revision: 1,
                    screen: AccessibilityScreen::Primary,
                    screen_generation: 1,
                    complete: true,
                    more: false,
                    topology: Some(vec![primary_id]),
                    visible_lines: 0..1,
                    cursor: None,
                    selection: None,
                    changed_rows: vec![update_row(primary_id, 1, "primary", false)],
                },
                PresentationGeneration::test(1),
            )
            .unwrap();
        let request = primary.selection_request(0..1).unwrap();

        state
            .apply(
                AccessibilityUpdate {
                    revision: 2,
                    screen: AccessibilityScreen::Alternate,
                    screen_generation: 2,
                    complete: true,
                    more: false,
                    topology: Some(vec![alternate_id]),
                    visible_lines: 0..1,
                    cursor: None,
                    selection: None,
                    changed_rows: vec![update_row(alternate_id, 1, "alternate", false)],
                },
                PresentationGeneration::test(2),
            )
            .unwrap();

        assert_eq!(state.resolve_selection(&request), None);
    }

    #[test]
    fn retained_document_prunes_old_rows_and_reuses_unaffected_page_blocks() {
        let mut state = TerminalAccessibilityState::default();
        let topology = (0..10_001_u16)
            .map(|row| {
                row_id(
                    AccessibilityScreen::Primary,
                    1,
                    10 + u64::from(row / 250),
                    row % 250,
                )
            })
            .collect::<Vec<_>>();
        let changed_rows = topology
            .iter()
            .copied()
            .map(|id| update_row(id, 1, "x", false))
            .collect();
        let initial = state
            .apply(
                AccessibilityUpdate {
                    revision: 1,
                    screen: AccessibilityScreen::Primary,
                    screen_generation: 1,
                    complete: true,
                    more: false,
                    topology: Some(topology.clone()),
                    visible_lines: 9_999..10_001,
                    cursor: None,
                    selection: None,
                    changed_rows,
                },
                PresentationGeneration::test(1),
            )
            .unwrap();
        assert_eq!(initial.data.document.rows, 10_001);
        assert!(initial.data.document.materialized.get().is_none());
        let unaffected = Arc::clone(&state.primary.document.blocks[1]);

        let pruned = state
            .apply(
                AccessibilityUpdate {
                    revision: 2,
                    screen: AccessibilityScreen::Primary,
                    screen_generation: 1,
                    complete: true,
                    more: false,
                    topology: Some(topology[1..].to_vec()),
                    visible_lines: 9_998..10_000,
                    cursor: None,
                    selection: None,
                    changed_rows: Vec::new(),
                },
                PresentationGeneration::test(2),
            )
            .unwrap();
        assert_eq!(pruned.data.document.rows, 10_000);
        assert!(Arc::ptr_eq(&unaffected, &state.primary.document.blocks[1]));
    }

    #[test]
    fn selection_normalizes_graphemes_and_rejects_newline_only_ranges() {
        let model = TerminalAccessibilityModel::new(
            vec![
                AccessibilityLine::new(vec![cell("😀", 2, false)], false),
                AccessibilityLine::new(vec![cell("x", 1, false)], false),
            ],
            0..2,
            None,
        );

        let normalized = model.selection_request(1..2).unwrap();
        assert_eq!(normalized.range, 0..2);
        assert!(model.selection_request(2..3).is_none());
    }

    #[test]
    fn stale_row_revision_selection_refs_do_not_publish_a_selection() {
        let id = row_id(AccessibilityScreen::Primary, 1, 10, 0);
        let stale = AccessibilityCellRef {
            row: id,
            row_revision: 2,
            column: 0,
        };
        let mut state = TerminalAccessibilityState::default();
        let model = state
            .apply(
                AccessibilityUpdate {
                    revision: 1,
                    screen: AccessibilityScreen::Primary,
                    screen_generation: 1,
                    complete: true,
                    more: false,
                    topology: Some(vec![id]),
                    visible_lines: 0..1,
                    cursor: None,
                    selection: Some(AccessibilitySelectionRefs {
                        start: stale,
                        end: stale,
                        rectangle: false,
                    }),
                    changed_rows: vec![update_row(id, 1, "x", false)],
                },
                PresentationGeneration::test(1),
            )
            .unwrap();

        assert_eq!(model.selection_range(), None);
    }

    #[test]
    fn notifications_are_typed_and_coalesced() {
        let mut notifications = AccessibilityNotifications::default();
        notifications.extend([
            AccessibilityNotification::Value,
            AccessibilityNotification::Selection,
            AccessibilityNotification::Value,
            AccessibilityNotification::Focus,
        ]);

        assert_eq!(
            notifications.iter().collect::<Vec<_>>(),
            vec![
                AccessibilityNotification::Value,
                AccessibilityNotification::Selection,
                AccessibilityNotification::Focus
            ]
        );
        assert_eq!(
            (
                std::mem::size_of::<AccessibilityNotifications>(),
                notifications.len()
            ),
            (1, 3)
        );
    }

    #[test]
    fn notification_state_stays_fixed_capacity_under_sustained_updates() {
        let mut notifications = AccessibilityNotifications::default();
        for _ in 0..100_000 {
            notifications.extend(AccessibilityNotifications::ORDERED);
        }

        assert_eq!(notifications.len(), 3);
        assert_eq!(
            notifications.take().iter().collect::<Vec<_>>(),
            AccessibilityNotifications::ORDERED
        );
        assert!(notifications.is_empty());
    }
}
