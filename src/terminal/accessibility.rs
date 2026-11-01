use std::collections::BTreeMap;
use std::ops::Range;
use std::sync::{Arc, OnceLock};

use super::PresentationGeneration;
use super::emulator::ActiveScreenSnapshot;

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum AccessibilityScreen {
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

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct AccessibilityRowId {
    pub screen: AccessibilityScreen,
    pub screen_generation: usize,
    pub node_serial: u64,
    pub page_row: u16,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AccessibilityCellRef {
    pub row: AccessibilityRowId,
    pub row_revision: u64,
    pub column: u16,
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

/// A row a client decoded, ready to retain. It holds its text once, and its cell spans only when
/// it is not one column per character.
#[cfg(feature = "desktop")]
#[derive(Clone, Debug)]
pub(crate) struct AccessibilityRetainedRow(Arc<AccessibilityRow>);

#[cfg(feature = "desktop")]
impl AccessibilityRetainedRow {
    /// A row of one cell per character of `text`, each one column wide, from column 0.
    pub(crate) fn simple(
        id: AccessibilityRowId,
        revision: u64,
        soft_wrapped: bool,
        text: String,
    ) -> Self {
        Self(Arc::new(AccessibilityRow::new(
            id,
            revision,
            soft_wrapped,
            text,
            None,
        )))
    }

    pub(crate) fn id(&self) -> AccessibilityRowId {
        self.0.id
    }

    pub(crate) fn revision(&self) -> u64 {
        self.0.revision
    }

    /// A row whose cells are `cells`, in column order: each one's first column, width and the
    /// length of its text in bytes. The cells cover `text` exactly.
    pub(crate) fn with_cells(
        id: AccessibilityRowId,
        revision: u64,
        soft_wrapped: bool,
        text: String,
        cells: impl ExactSizeIterator<Item = (u16, u16, u16)>,
    ) -> Self {
        let mut byte = 0_u32;
        let spans = cells
            .map(|(column, width, bytes)| {
                let span = RowSpan {
                    column,
                    width,
                    byte,
                };
                byte += u32::from(bytes);
                span
            })
            .collect::<Box<[_]>>();
        Self(Arc::new(AccessibilityRow::new(
            id,
            revision,
            soft_wrapped,
            text,
            Some(spans),
        )))
    }
}

/// One whole semantic revision a client decoded: its topology when it changed, the rows it sent,
/// and what it presents. Rows it does not send keep their retained contents.
#[cfg(feature = "desktop")]
pub(crate) struct AccessibilityRevision {
    pub(crate) screen: AccessibilityScreen,
    pub(crate) screen_generation: usize,
    pub(crate) topology: Option<Arc<[AccessibilityRowId]>>,
    pub(crate) rows: Vec<AccessibilityRetainedRow>,
    pub(crate) visible_lines: Range<usize>,
    pub(crate) cursor: Option<AccessibilityCellRef>,
    pub(crate) selection: Option<AccessibilitySelectionRefs>,
}

/// A revision that leaves a line of its topology without a row, or names another screen.
#[cfg(feature = "desktop")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct IncompleteRevision;

/// The topology of the revision a client applied last, with the row retained at each line.
#[cfg(feature = "desktop")]
pub(crate) struct AppliedTopology<'a> {
    pub(crate) screen: AccessibilityScreen,
    pub(crate) screen_generation: usize,
    rows: &'a RetainedRows,
}

#[cfg(feature = "desktop")]
impl AppliedTopology<'_> {
    pub(crate) fn topology(&self) -> &Arc<[AccessibilityRowId]> {
        &self.rows.topology
    }

    /// The revision of the row retained at `line`.
    pub(crate) fn row_revision(&self, line: usize) -> Option<u64> {
        Some(self.rows.rows.get(line)?.as_ref()?.revision)
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AccessibilityGeometry {
    origin_x: f32,
    origin_y: f32,
    cell_width: f32,
    line_height: f32,
}

impl AccessibilityGeometry {
    pub fn new(origin_x: f32, origin_y: f32, cell_width: f32, line_height: f32) -> Option<Self> {
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
#[cfg_attr(
    not(feature = "desktop"),
    allow(
        dead_code,
        reason = "native accessibility adapters consume notification policy"
    )
)]
pub(crate) enum AccessibilityNotification {
    Value,
    Selection,
    Focus,
}

#[cfg_attr(
    not(feature = "desktop"),
    allow(
        dead_code,
        reason = "native accessibility adapters consume notification policy"
    )
)]
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
#[cfg_attr(
    not(feature = "desktop"),
    allow(
        dead_code,
        reason = "native accessibility adapters consume notification policy"
    )
)]
pub(crate) struct AccessibilityNotifications(u8);

#[cfg_attr(
    any(not(target_os = "macos"), not(feature = "desktop")),
    allow(
        dead_code,
        reason = "only a native accessibility Adapter posts notifications"
    )
)]
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

/// One cell's columns, its byte range in its row's text and its UTF-16 range there.
#[derive(Clone, Debug, Eq, PartialEq)]
struct CellBounds {
    columns: Range<u16>,
    bytes: Range<usize>,
    utf16: Range<usize>,
}

/// The first column and width of one cell, and where its text starts in its row.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct RowSpan {
    column: u16,
    width: u16,
    byte: u32,
}

/// Where one cell starts in its row's text, in bytes and in UTF-16 code units.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct CellStart {
    byte: u32,
    utf16: u32,
}

/// What a row needs besides its text when it is not one ASCII character per column.
#[derive(Debug, Eq, PartialEq)]
struct RowLayout {
    /// Every cell in column order. `None` when cell `i` is character `i` at column `i`, one
    /// column wide.
    spans: Option<Box<[RowSpan]>>,
    /// Where each cell starts, then the end of the text. Only rows with non-ASCII text have it;
    /// in an ASCII row a byte offset is the UTF-16 offset.
    utf16: Option<Box<[CellStart]>>,
}

/// A retained row. It holds its text once and derives each cell's columns, text, UTF-16 range
/// and reference on demand from its identity, revision and column.
#[derive(Debug)]
struct AccessibilityRow {
    id: AccessibilityRowId,
    revision: u64,
    text: Box<str>,
    /// `None` for a row of one ASCII character per column from column 0.
    layout: Option<Box<RowLayout>>,
    len_utf16: u32,
    soft_wrapped: bool,
}

impl AccessibilityRow {
    fn from_update(update: AccessibilityRowUpdate) -> Arc<Self> {
        let mut text = String::with_capacity(update.cells.iter().map(|cell| cell.text.len()).sum());
        let mut spans = Vec::with_capacity(update.cells.len());
        let mut next_column = 0_u16;
        for cell in &update.cells {
            let column = cell.column.max(next_column);
            spans.push(RowSpan {
                column,
                width: cell.width_cells,
                byte: text.len() as u32,
            });
            text.push_str(&cell.text);
            next_column = column.saturating_add(cell.width_cells);
        }
        Arc::new(Self::with_spans(
            update.id,
            update.revision,
            update.soft_wrapped,
            text,
            spans,
        ))
    }

    /// A row whose cells are `spans`, in column order, covering `text`. Keeps the spans only
    /// when the row is not one column per character.
    fn with_spans(
        id: AccessibilityRowId,
        revision: u64,
        soft_wrapped: bool,
        text: String,
        spans: Vec<RowSpan>,
    ) -> Self {
        let mut characters = text.char_indices();
        let simple = spans.iter().enumerate().all(|(index, span)| {
            usize::from(span.column) == index
                && span.width == 1
                && characters
                    .next()
                    .is_some_and(|(byte, _)| byte as u32 == span.byte)
        }) && characters.next().is_none();
        Self::new(
            id,
            revision,
            soft_wrapped,
            text,
            (!simple).then(|| spans.into_boxed_slice()),
        )
    }

    fn new(
        id: AccessibilityRowId,
        revision: u64,
        soft_wrapped: bool,
        text: String,
        spans: Option<Box<[RowSpan]>>,
    ) -> Self {
        let text = text.into_boxed_str();
        let utf16 = (!text.is_ascii()).then(|| cell_starts(&text, spans.as_deref()));
        let len_utf16 = utf16
            .as_ref()
            .and_then(|starts| starts.last())
            .map_or(text.len() as u32, |end| end.utf16);
        let layout =
            (spans.is_some() || utf16.is_some()).then(|| Box::new(RowLayout { spans, utf16 }));
        Self {
            id,
            revision,
            text,
            layout,
            len_utf16,
            soft_wrapped,
        }
    }

    fn len_utf16(&self) -> usize {
        self.len_utf16 as usize
    }

    fn spans(&self) -> Option<&[RowSpan]> {
        self.layout.as_ref()?.spans.as_deref()
    }

    fn utf16(&self) -> Option<&[CellStart]> {
        self.layout.as_ref()?.utf16.as_deref()
    }

    fn cell_count(&self) -> usize {
        match (self.spans(), self.utf16()) {
            (Some(spans), _) => spans.len(),
            (None, Some(starts)) => starts.len() - 1,
            (None, None) => self.text.len(),
        }
    }

    fn cell(&self, index: usize) -> CellBounds {
        let columns = match self.spans() {
            Some(spans) => {
                spans[index].column..spans[index].column.saturating_add(spans[index].width)
            }
            None => index as u16..(index as u16).saturating_add(1),
        };
        let (bytes, utf16) = match (self.spans(), self.utf16()) {
            (_, Some(starts)) => (
                starts[index].byte as usize..starts[index + 1].byte as usize,
                starts[index].utf16 as usize..starts[index + 1].utf16 as usize,
            ),
            (Some(spans), None) => {
                let start = spans[index].byte as usize;
                let end = spans
                    .get(index + 1)
                    .map_or(self.text.len(), |next| next.byte as usize);
                (start..end, start..end)
            }
            (None, None) => (index..index + 1, index..index + 1),
        };
        CellBounds {
            columns,
            bytes,
            utf16,
        }
    }

    fn cells(&self) -> impl Iterator<Item = CellBounds> + '_ {
        (0..self.cell_count()).map(|index| self.cell(index))
    }

    fn reference(&self, column: u16) -> AccessibilityCellRef {
        AccessibilityCellRef {
            row: self.id,
            row_revision: self.revision,
            column,
        }
    }

    /// The cell whose columns contain `column`.
    fn cell_at_column(&self, column: u16) -> Option<CellBounds> {
        let index = match self.spans() {
            Some(spans) => {
                spans.partition_point(|span| span.column.saturating_add(span.width) <= column)
            }
            None => usize::from(column),
        };
        (index < self.cell_count())
            .then(|| self.cell(index))
            .filter(|cell| cell.columns.contains(&column))
    }

    /// The cell whose UTF-16 range contains `local`.
    fn cell_at_utf16(&self, local: usize) -> Option<CellBounds> {
        if local >= self.len_utf16() {
            return None;
        }
        let index = match (self.spans(), self.utf16()) {
            (_, Some(starts)) => starts[..starts.len() - 1]
                .partition_point(|start| start.utf16 as usize <= local)
                .checked_sub(1)?,
            (Some(spans), None) => spans
                .partition_point(|span| span.byte as usize <= local)
                .checked_sub(1)?,
            (None, None) => local,
        };
        Some(self.cell(index)).filter(|cell| cell.utf16.contains(&local))
    }

    /// Whether `local` starts or ends a cell.
    fn is_cell_boundary(&self, local: usize) -> bool {
        local == self.len_utf16()
            || self
                .cell_at_utf16(local)
                .is_some_and(|cell| cell.utf16.start == local)
    }

    fn byte_for_utf16(&self, local: usize) -> Option<usize> {
        if local == self.len_utf16() {
            return Some(self.text.len());
        }
        let cell = self.cell_at_utf16(local)?;
        let mut utf16 = cell.utf16.start;
        for (offset, character) in self.text[cell.bytes.clone()].char_indices() {
            if utf16 == local {
                return Some(cell.bytes.start + offset);
            }
            utf16 += character.len_utf16();
        }
        None
    }

    /// Whether `other` holds the same revision, text and cells.
    fn same_content(&self, other: &Self) -> bool {
        self.revision == other.revision
            && self.soft_wrapped == other.soft_wrapped
            && self.text == other.text
            && self.layout == other.layout
    }
}

/// Where each cell of `text` starts, in bytes and UTF-16 code units, then where the text ends.
/// Without `spans`, each character is a cell.
fn cell_starts(text: &str, spans: Option<&[RowSpan]>) -> Box<[CellStart]> {
    match spans {
        Some(spans) => starts_at(
            text,
            spans.iter().map(|span| span.byte as usize),
            spans.len(),
        ),
        None => starts_at(
            text,
            text.char_indices().map(|(byte, _)| byte),
            text.chars().count(),
        ),
    }
}

fn starts_at(text: &str, cells: impl Iterator<Item = usize>, count: usize) -> Box<[CellStart]> {
    let mut starts = Vec::with_capacity(count + 1);
    let (mut previous, mut utf16) = (0_usize, 0_u32);
    for byte in cells.chain(std::iter::once(text.len())) {
        utf16 += text
            .get(previous..byte)
            .map_or(0, |part| part.encode_utf16().count()) as u32;
        previous = byte;
        starts.push(CellStart {
            byte: byte as u32,
            utf16,
        });
    }
    starts.into_boxed_slice()
}

#[derive(Debug)]
struct AccessibilityRowBlock {
    /// The Screen generation and node serial its rows share, read through `key`.
    page: (usize, u64),
    rows: Box<[Arc<AccessibilityRow>]>,
    /// Local row indices ordered by page row, then by position, so an identity resolves to its
    /// first row in document order without visiting the block.
    by_page_row: Box<[u32]>,
    /// Where each row starts in the block's UTF-16 text, then the block's end.
    utf16_prefix: Box<[u32]>,
    len_utf16: usize,
}

impl AccessibilityRowBlock {
    fn new(
        rows: Box<[Arc<AccessibilityRow>]>,
        document_last_row: usize,
        row_start: usize,
    ) -> Arc<Self> {
        let mut utf16_prefix = Vec::with_capacity(rows.len() + 1);
        utf16_prefix.push(0);
        let mut len_utf16 = 0_usize;
        for (index, row) in rows.iter().enumerate() {
            len_utf16 = len_utf16.saturating_add(row.len_utf16());
            if row_start + index != document_last_row && !row.soft_wrapped {
                len_utf16 = len_utf16.saturating_add(1);
            }
            utf16_prefix.push(len_utf16 as u32);
        }
        let page = rows
            .first()
            .map(|row| (row.id.screen_generation, row.id.node_serial))
            .unwrap_or_default();
        let mut by_page_row = (0..rows.len() as u32).collect::<Box<[_]>>();
        by_page_row.sort_unstable_by_key(|local| (rows[*local as usize].id.page_row, *local));
        Arc::new(Self {
            page,
            rows,
            by_page_row,
            utf16_prefix: utf16_prefix.into_boxed_slice(),
            len_utf16,
        })
    }

    /// The page this block's rows belong to. Every read counts as one search step.
    fn key(&self) -> (usize, u64) {
        count_search_step();
        self.page
    }

    fn local_row(&self, id: AccessibilityRowId) -> Option<usize> {
        let page_row = |local: u32| {
            count_search_step();
            self.rows[local as usize].id.page_row
        };
        let first = self
            .by_page_row
            .partition_point(|local| page_row(*local) < id.page_row);
        self.by_page_row[first..]
            .iter()
            .copied()
            .take_while(|local| page_row(*local) == id.page_row)
            .map(|local| local as usize)
            .find(|local| self.rows[*local].id == id)
    }

    /// Where local row `local` starts in the block's UTF-16 text.
    fn row_start(&self, local: usize) -> usize {
        self.utf16_prefix[local] as usize
    }

    fn shares_rows(&self, rows: &[Option<Arc<AccessibilityRow>>], is_document_tail: bool) -> bool {
        self.rows.len() == rows.len()
            && self
                .rows
                .iter()
                .zip(rows)
                .all(|(left, right)| right.as_ref().is_some_and(|right| Arc::ptr_eq(left, right)))
            && self.rows.last().is_none_or(|row| {
                let expected =
                    row.len_utf16() + usize::from(!is_document_tail && !row.soft_wrapped);
                self.row_start(self.rows.len()) - self.row_start(self.rows.len().saturating_sub(1))
                    == expected
            })
    }
}

#[cfg_attr(
    not(target_os = "macos"),
    allow(
        dead_code,
        reason = "only a native accessibility Adapter materializes document text"
    )
)]
#[derive(Debug)]
struct AccessibilityDocument {
    topology: Arc<[AccessibilityRowId]>,
    /// Rows grouped by page in document order. A block whose rows did not change is shared with
    /// the previous document, so publishers find changed rows without visiting retained history.
    blocks: Arc<[Arc<AccessibilityRowBlock>]>,
    /// Block indices ordered by their `(screen_generation, node_serial)` key, then by position.
    /// With each block's page row order it forms the identity-to-line index.
    blocks_by_key: Box<[usize]>,
    block_row_prefix: Arc<[usize]>,
    block_utf16_prefix: Arc<[usize]>,
    rows: usize,
    len_utf16: usize,
    materialized: OnceLock<String>,
}

impl AccessibilityDocument {
    fn empty() -> Arc<Self> {
        Arc::new(Self {
            topology: Arc::from([]),
            blocks: Arc::from([]),
            blocks_by_key: Box::new([]),
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
        Some(self.block_utf16_prefix[block_index] + self.blocks[block_index].row_start(local))
    }

    fn range_for_line(&self, line: usize) -> Option<Range<usize>> {
        let (row, block_index) = self.row(line)?;
        let local = line - self.block_row_prefix[block_index];
        let start =
            self.block_utf16_prefix[block_index] + self.blocks[block_index].row_start(local);
        let end = start + row.len_utf16() + usize::from(line + 1 < self.rows && !row.soft_wrapped);
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
            .partition_point(|start| *start as usize <= local_index)
            .checked_sub(1)?
            .min(block.rows.len().checked_sub(1)?);
        Some(self.block_row_prefix[block_index] + local_row)
    }

    fn row_for_id(&self, id: AccessibilityRowId) -> Option<(usize, &AccessibilityRow)> {
        let key = (id.screen_generation, id.node_serial);
        let block_key = |block_index: &usize| self.blocks[*block_index].key();
        let first = self
            .blocks_by_key
            .partition_point(|block_index| block_key(block_index) < key);
        self.blocks_by_key[first..]
            .iter()
            .take_while(|block_index| block_key(block_index) == key)
            .find_map(|block_index| {
                let block = &self.blocks[*block_index];
                let local = block.local_row(id)?;
                Some((
                    self.block_row_prefix[*block_index] + local,
                    &*block.rows[local],
                ))
            })
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
        local >= row.len_utf16() || row.is_cell_boundary(local)
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
            let text_end = row_start + row.len_utf16();
            let local_start = range.start.saturating_sub(row_start).min(row.len_utf16());
            let local_end = range.end.saturating_sub(row_start).min(row.len_utf16());
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

    #[cfg_attr(
        not(target_os = "macos"),
        allow(
            dead_code,
            reason = "only a native accessibility Adapter materializes document text"
        )
    )]
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
    cursor_ref: Option<AccessibilityCellRef>,
    selection_refs: Option<AccessibilitySelectionRefs>,
}

impl TerminalAccessibilityData {
    /// Resolves the cursor and Selection references through the document's identity index, so
    /// binding a retained document costs the same at any Scrollback size.
    fn retained(request: RetainedAccessibilityRequest) -> Self {
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
        let resolved_cursor = cursor.and_then(|reference| {
            let (line, row) = resolve_ref(&document, reference)?;
            let column = row
                .cell_at_column(reference.column)
                .map_or(reference.column, |cell| cell.columns.start);
            let offset = offset_for_column(&document, line, row, reference.column, false);
            Some(((line, column), offset))
        });
        let cursor_cell = resolved_cursor.map(|(cell, _)| cell);
        let cursor_ref = cursor.filter(|_| cursor_cell.is_some());
        let cursor = resolved_cursor
            .and_then(|(_, offset)| offset)
            .unwrap_or(visible_range.start);
        // AppKit exposes one contiguous UTF-16 range. A terminal rectangle therefore uses the
        // smallest contiguous range containing its ordered endpoints; the refs keep the shape.
        let selection_range = selection.and_then(|selection| {
            range_for_refs(&document, selection.start, selection.end)
                .map(|range| (selection, range))
        });
        Self {
            generation,
            content_revision,
            screen,
            screen_generation,
            document,
            visible_lines,
            visible_range,
            selection: selection_range.as_ref().map(|(_, range)| range.clone()),
            cursor: cursor..cursor,
            cursor_cell,
            cursor_ref,
            selection_refs: selection_range.map(|(selection, _)| selection),
        }
    }

    /// Whether `other` presents the same screen, visible lines, cursor and Selection for the
    /// same generation, so publishing it would tell assistive technology nothing new.
    fn presents_same(&self, other: &Self) -> bool {
        self.screen == other.screen
            && self.generation == other.generation
            && self.visible_lines == other.visible_lines
            && self.cursor == other.cursor
            && self.selection == other.selection
    }
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
pub struct AccessibilitySelectionRequest {
    pub generation: PresentationGeneration,
    pub content_revision: u64,
    pub screen: AccessibilityScreen,
    pub range: Range<usize>,
}

#[derive(Clone, Debug)]
pub struct TerminalAccessibilityModel {
    data: Arc<TerminalAccessibilityData>,
}

/// Read-only projection of a retained terminal row, including its document offsets.
pub struct AccessibilityRowView<'a> {
    pub id: AccessibilityRowId,
    pub revision: u64,
    pub soft_wrapped: bool,
    pub range: Range<usize>,
    pub text: &'a str,
    pub len_utf16: usize,
    row: &'a AccessibilityRow,
}

pub struct AccessibilityCellView<'a> {
    pub text: &'a str,
    pub utf16: Range<usize>,
    pub columns: Range<u16>,
}

impl<'a> AccessibilityRowView<'a> {
    pub fn cells(&self) -> impl Iterator<Item = AccessibilityCellView<'a>> + '_ {
        let row = self.row;
        row.cells().map(move |cell| AccessibilityCellView {
            text: &row.text[cell.bytes],
            utf16: cell.utf16,
            columns: cell.columns,
        })
    }
}

impl TerminalAccessibilityModel {
    pub fn rows(&self) -> impl Iterator<Item = AccessibilityRowView<'_>> {
        let document = &self.data.document;
        document
            .blocks
            .iter()
            .flat_map(|block| block.rows.iter())
            .enumerate()
            .map(|(line, row)| AccessibilityRowView {
                id: row.id,
                revision: row.revision,
                soft_wrapped: row.soft_wrapped,
                range: document
                    .range_for_line(line)
                    .expect("retained row has a document range"),
                text: &row.text,
                len_utf16: row.len_utf16(),
                row,
            })
    }

    pub fn row(&self, line: usize) -> Option<AccessibilityRowView<'_>> {
        let document = &self.data.document;
        let (row, _) = document.row(line)?;
        Some(AccessibilityRowView {
            id: row.id,
            revision: row.revision,
            soft_wrapped: row.soft_wrapped,
            range: document.range_for_line(line)?,
            text: &row.text,
            len_utf16: row.len_utf16(),
            row,
        })
    }

    /// None means row identities or order changed and the publisher must rebuild topology.
    pub fn changed_rows_since(&self, previous: &Self) -> Option<Vec<usize>> {
        let document = &self.data.document;
        let old = &previous.data.document;
        if self.shares_document(previous) {
            return Some(Vec::new());
        }
        if !Arc::ptr_eq(&document.topology, &old.topology) {
            return None;
        }
        // One topology groups its rows into the same blocks.
        let mut lines = Vec::new();
        for ((block, old_block), start) in document
            .blocks
            .iter()
            .zip(old.blocks.iter())
            .zip(document.block_row_prefix.iter())
        {
            if Arc::ptr_eq(block, old_block) {
                continue;
            }
            lines.extend(
                block
                    .rows
                    .iter()
                    .zip(old_block.rows.iter())
                    .enumerate()
                    .filter(|(_, (row, old_row))| !Arc::ptr_eq(row, old_row))
                    .map(|(local, _)| start + local),
            );
        }
        Some(lines)
    }

    pub fn visible_lines(&self) -> Range<usize> {
        self.data.visible_lines.clone()
    }

    pub fn cursor_cell(&self) -> Option<(usize, u16)> {
        self.data.cursor_cell
    }
    pub fn from_screen(screen: &super::ScreenSnapshot) -> Self {
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
        let mut retained = Vec::with_capacity(lines.len());
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
            retained.push(AccessibilityRow::from_update(AccessibilityRowUpdate {
                id,
                revision: 1,
                soft_wrapped: line.soft_wrapped,
                cells,
            }));
            topology.push(id);
        }
        let mut rows = RetainedRows::default();
        rows.set_topology(&Arc::from(topology));
        for row in retained {
            rows.insert(row).expect("the topology names every row");
        }
        let document = build_document(&rows, None);
        let visible_lines =
            visible_lines.start.min(document.rows)..visible_lines.end.min(document.rows);
        let visible_range = visible_range(&document, visible_lines.clone());
        let selection_refs = selected.first().zip(selected.last()).map(
            |((start_row, start_col), (end_row, end_col))| AccessibilitySelectionRefs {
                start: AccessibilityCellRef {
                    row: *start_row,
                    row_revision: 1,
                    column: *start_col,
                },
                end: AccessibilityCellRef {
                    row: *end_row,
                    row_revision: 1,
                    column: *end_col,
                },
                rectangle: false,
            },
        );
        let selection = selection_refs
            .and_then(|selection| range_for_refs(&document, selection.start, selection.end));
        let cursor_cell = cursor.and_then(|(line, column)| {
            let (row, _) = document.row(line)?;
            let column = row
                .cell_at_column(column)
                .map_or(column, |cell| cell.columns.start);
            Some((line, column))
        });
        let cursor_ref = cursor_cell.and_then(|(line, column)| {
            let (row, _) = document.row(line)?;
            Some(AccessibilityCellRef {
                row: row.id,
                row_revision: row.revision,
                column,
            })
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
                cursor_ref,
                selection_refs,
            }),
        }
    }

    pub fn generation(&self) -> PresentationGeneration {
        self.data.generation
    }

    pub fn shares_snapshot(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.data, &other.data)
    }

    pub fn shares_document(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.data.document, &other.data.document)
    }

    pub fn active_screen(&self) -> AccessibilityScreen {
        self.data.screen
    }

    pub(crate) fn screen_generation(&self) -> usize {
        self.data.screen_generation
    }

    /// Stable cursor cell identity, present only when it names a retained row revision.
    pub(crate) fn cursor_ref(&self) -> Option<AccessibilityCellRef> {
        self.data.cursor_ref
    }

    /// Stable Selection endpoints, present only when both name retained row revisions.
    pub(crate) fn selection_refs(&self) -> Option<AccessibilitySelectionRefs> {
        self.data.selection_refs
    }

    #[cfg_attr(
        not(target_os = "macos"),
        allow(
            dead_code,
            reason = "only a native accessibility Adapter answers text queries"
        )
    )]
    pub fn text(&self) -> &str {
        self.data.document.text()
    }

    pub fn len_utf16(&self) -> usize {
        self.data.document.len_utf16
    }

    pub fn visible_range(&self) -> Range<usize> {
        self.data.visible_range.clone()
    }

    pub fn selection_range(&self) -> Option<Range<usize>> {
        self.data.selection.clone()
    }

    pub fn cursor_range(&self) -> Range<usize> {
        self.data.cursor.clone()
    }

    pub fn selected_or_cursor_range(&self) -> Range<usize> {
        self.data
            .selection
            .clone()
            .unwrap_or_else(|| self.data.cursor.clone())
    }

    pub fn range_for_line(&self, line: usize) -> Option<Range<usize>> {
        self.data.document.range_for_line(line)
    }

    pub fn line_for_index(&self, index: usize) -> Option<usize> {
        self.data.document.line_for_index(index)
    }

    #[cfg_attr(
        not(target_os = "macos"),
        allow(
            dead_code,
            reason = "only a native accessibility Adapter answers text queries"
        )
    )]
    pub fn range_for_index(&self, index: usize) -> Option<Range<usize>> {
        if index >= self.len_utf16() {
            return None;
        }
        let line = self.line_for_index(index)?;
        let row_start = self.data.document.row_start(line)?;
        let (row, _) = self.data.document.row(line)?;
        let local = index.saturating_sub(row_start);
        row.cell_at_utf16(local)
            .map(|cell| row_start + cell.utf16.start..row_start + cell.utf16.end)
            .or_else(|| (index < self.len_utf16()).then_some(index..index + 1))
    }

    pub fn text_for_range(&self, range: Range<usize>) -> Option<String> {
        self.data.document.text_for_range(range)
    }

    pub fn selection_request(&self, range: Range<usize>) -> Option<AccessibilitySelectionRequest> {
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
            for cell in row.cells() {
                let cell_range = row_start + cell.utf16.start..row_start + cell.utf16.end;
                if cell_range.start < range.end && cell_range.end > range.start {
                    let reference = row.reference(cell.columns.start);
                    first.get_or_insert(reference);
                    last = Some(reference);
                }
            }
        }
        first.zip(last)
    }

    pub fn bounds_for_range(
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
                row.cells()
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
            for cell in row.cells() {
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

    pub fn index_for_point(
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
        row.cell_at_column(column)
            .map(|cell| row_start + cell.utf16.start)
    }

    #[cfg_attr(
        not(target_os = "macos"),
        allow(
            dead_code,
            reason = "only a native accessibility Adapter answers text queries"
        )
    )]
    pub fn range_for_point(
        &self,
        x: f32,
        y: f32,
        geometry: AccessibilityGeometry,
    ) -> Option<Range<usize>> {
        self.range_for_index(self.index_for_point(x, y, geometry)?)
    }
}

/// Work that row-identity lookups did on one thread, so a test can prove that a re-bind's cost
/// does not follow the retained row count.
#[cfg(test)]
#[cfg_attr(
    not(feature = "desktop"),
    allow(dead_code, reason = "the re-bind cost test drives the desktop client")
)]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct Probes {
    /// Lookups in a screen's retained rows or live identities.
    pub(crate) map_lookups: usize,
    /// Row blocks and rows that row-identity searches visited in a document.
    pub(crate) search_steps: usize,
}

#[cfg(test)]
thread_local! {
    static PROBES: std::cell::Cell<Probes> = const {
        std::cell::Cell::new(Probes { map_lookups: 0, search_steps: 0 })
    };
}

/// The lookups this thread has made so far.
#[cfg(test)]
#[cfg_attr(
    not(feature = "desktop"),
    allow(dead_code, reason = "the re-bind cost test drives the desktop client")
)]
pub(crate) fn probes() -> Probes {
    PROBES.with(std::cell::Cell::get)
}

fn count_map_lookup() {
    #[cfg(test)]
    PROBES.with(|probes| {
        let mut counted = probes.get();
        counted.map_lookups += 1;
        probes.set(counted);
    });
}

fn count_search_step() {
    #[cfg(test)]
    PROBES.with(|probes| {
        let mut counted = probes.get();
        counted.search_steps += 1;
        probes.set(counted);
    });
}

/// A changed row whose identity the screen's topology does not name.
#[derive(Debug, Eq, PartialEq)]
struct RowOutsideTopology;

/// The live identities one screen's topology names and the rows retained for them, by line. It
/// retains only live identities and counts the lines that have a row, so completeness needs no
/// pass over the rows.
#[derive(Debug)]
struct RetainedRows {
    topology: Arc<[AccessibilityRowId]>,
    /// Lines ordered by identity, then by line, so every line an identity names is found by one
    /// search.
    by_id: Box<[u32]>,
    rows: Box<[Option<Arc<AccessibilityRow>>]>,
    filled: usize,
}

impl Default for RetainedRows {
    fn default() -> Self {
        Self {
            topology: Arc::from([]),
            by_id: Box::new([]),
            rows: Box::new([]),
            filled: 0,
        }
    }
}

impl RetainedRows {
    /// Makes `topology` the live identities, keeps the row of every identity it still names and
    /// drops every other row.
    fn set_topology(&mut self, topology: &Arc<[AccessibilityRowId]>) {
        if Arc::ptr_eq(&self.topology, topology) {
            return;
        }
        let mut by_id = (0..topology.len() as u32).collect::<Box<[_]>>();
        by_id.sort_unstable_by_key(|line| (topology[*line as usize], *line));
        let rows = topology
            .iter()
            .map(|id| {
                self.lines(*id)
                    .next()
                    .and_then(|line| self.rows[line].clone())
            })
            .collect::<Box<[_]>>();
        self.filled = rows.iter().filter(|row| row.is_some()).count();
        self.topology = Arc::clone(topology);
        self.by_id = by_id;
        self.rows = rows;
    }

    /// Every line the topology names `id` at, in line order.
    fn lines(&self, id: AccessibilityRowId) -> impl Iterator<Item = usize> + '_ {
        let first = self
            .by_id
            .partition_point(|line| self.topology[*line as usize] < id);
        self.by_id[first..]
            .iter()
            .map(|line| *line as usize)
            .take_while(move |line| self.topology[*line] == id)
    }

    fn get(&self, id: &AccessibilityRowId) -> Option<&Arc<AccessibilityRow>> {
        count_map_lookup();
        let line = self.lines(*id).next()?;
        self.rows[line].as_ref()
    }

    /// Retains `row` at every line the topology names its identity.
    fn insert(&mut self, row: Arc<AccessibilityRow>) -> Result<(), RowOutsideTopology> {
        count_map_lookup();
        let first = self
            .by_id
            .partition_point(|line| self.topology[*line as usize] < row.id);
        let mut named = false;
        for line in self.by_id[first..].iter().map(|line| *line as usize) {
            if self.topology[line] != row.id {
                break;
            }
            named = true;
            if self.rows[line].replace(Arc::clone(&row)).is_none() {
                self.filled += 1;
            }
        }
        named.then_some(()).ok_or(RowOutsideTopology)
    }

    /// Whether every line of the topology has a row, without visiting the rows.
    fn is_complete(&self) -> bool {
        self.filled == self.rows.len()
    }

    /// The rows in topology order, when every line has one.
    fn ordered(&self) -> Option<&[Option<Arc<AccessibilityRow>>]> {
        self.is_complete().then_some(&self.rows)
    }
}

#[derive(Debug)]
struct AccessibilityScreenState {
    rows: RetainedRows,
    document: Arc<AccessibilityDocument>,
    document_dirty: bool,
    content_revision: u64,
}

impl Default for AccessibilityScreenState {
    fn default() -> Self {
        Self {
            rows: RetainedRows::default(),
            document: AccessibilityDocument::empty(),
            document_dirty: false,
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
    /// The model this state published last.
    #[cfg(feature = "desktop")]
    pub(crate) fn latest(&self) -> Option<&TerminalAccessibilityModel> {
        self.latest.as_ref()
    }

    pub(crate) fn reset_presentation(&mut self) {
        self.latest = None;
    }

    pub(crate) fn apply(
        &mut self,
        update: AccessibilityUpdate,
        generation: PresentationGeneration,
    ) -> Option<Arc<TerminalAccessibilityModel>> {
        if update.complete == update.more {
            return None;
        }
        let (screen, screen_generation) = (update.screen, update.screen_generation);
        self.target(screen, screen_generation);
        let named = |id: &AccessibilityRowId| {
            id.screen == screen && id.screen_generation == screen_generation
        };
        if let Some(topology) = &update.topology
            && !topology.iter().all(named)
        {
            return None;
        }
        // A row outside the topology is not retained.
        let _ = self.stage(
            screen,
            update.topology.map(Arc::from),
            update
                .changed_rows
                .into_iter()
                .filter(|row| named(&row.id))
                .map(AccessibilityRow::from_update),
        );
        if !update.complete {
            return None;
        }
        self.publish(
            screen,
            screen_generation,
            update.visible_lines,
            update.cursor,
            update.selection,
            generation,
        )
    }

    /// Applies a client's whole semantic revision. Its rows and topology name only its screen
    /// and screen generation, and together with the rows retained from its base they give every
    /// line of the topology a row; otherwise it is refused.
    #[cfg(feature = "desktop")]
    pub(crate) fn apply_revision(
        &mut self,
        revision: AccessibilityRevision,
        generation: PresentationGeneration,
    ) -> Result<Option<Arc<TerminalAccessibilityModel>>, IncompleteRevision> {
        let (screen, screen_generation) = (revision.screen, revision.screen_generation);
        self.target(screen, screen_generation);
        let named = |id: &AccessibilityRowId| {
            id.screen == screen && id.screen_generation == screen_generation
        };
        if revision
            .topology
            .as_ref()
            .is_some_and(|topology| !topology.iter().all(named))
            || !revision.rows.iter().all(|row| named(&row.0.id))
        {
            return Err(IncompleteRevision);
        }
        self.stage(
            screen,
            revision.topology,
            revision.rows.into_iter().map(|row| row.0),
        )
        .map_err(|_| IncompleteRevision)?;
        let state = self.screen(screen);
        if !state.rows.is_complete() || state.rows.topology.first().is_some_and(|id| !named(id)) {
            return Err(IncompleteRevision);
        }
        Ok(self.publish(
            screen,
            screen_generation,
            revision.visible_lines,
            revision.cursor,
            revision.selection,
            generation,
        ))
    }

    /// The screen and topology of the revision applied last, and the revision of the row at
    /// each line, which a client's next delta applies to.
    #[cfg(feature = "desktop")]
    pub(crate) fn applied(&self) -> Option<AppliedTopology<'_>> {
        let screen = self.active_screen?;
        Some(AppliedTopology {
            screen,
            screen_generation: self.active_screen_generation,
            rows: &self.screen(screen).rows,
        })
    }

    fn target(&mut self, screen: AccessibilityScreen, screen_generation: usize) {
        self.pending_semantic_target = true;
        self.active_screen = Some(screen);
        self.active_screen_generation = screen_generation;
    }

    #[cfg(feature = "desktop")]
    fn screen(&self, screen: AccessibilityScreen) -> &AccessibilityScreenState {
        match screen {
            AccessibilityScreen::Primary => &self.primary,
            AccessibilityScreen::Alternate => &self.alternate,
        }
    }

    /// Replaces the screen's topology, when given, then retains each changed row. Fails when a
    /// row names no line of the topology, after retaining the others.
    fn stage(
        &mut self,
        screen: AccessibilityScreen,
        topology: Option<Arc<[AccessibilityRowId]>>,
        rows: impl Iterator<Item = Arc<AccessibilityRow>>,
    ) -> Result<(), RowOutsideTopology> {
        let state = match screen {
            AccessibilityScreen::Primary => &mut self.primary,
            AccessibilityScreen::Alternate => &mut self.alternate,
        };
        if let Some(topology) = topology
            && state.rows.topology != topology
        {
            state.rows.set_topology(&topology);
            state.document_dirty = true;
        }
        let mut result = Ok(());
        for replacement in rows {
            let unchanged = state
                .rows
                .get(&replacement.id)
                .is_some_and(|existing| existing.same_content(&replacement));
            if unchanged {
                continue;
            }
            match state.rows.insert(replacement) {
                Ok(()) => state.document_dirty = true,
                Err(error) => result = Err(error),
            }
        }
        result
    }

    /// Publishes the screen's retained document with the given presentation, once every line has
    /// a row, unless it would tell assistive technology nothing new.
    fn publish(
        &mut self,
        screen: AccessibilityScreen,
        screen_generation: usize,
        visible_lines: Range<usize>,
        cursor: Option<AccessibilityCellRef>,
        selection: Option<AccessibilitySelectionRefs>,
        generation: PresentationGeneration,
    ) -> Option<Arc<TerminalAccessibilityModel>> {
        let state = match screen {
            AccessibilityScreen::Primary => &mut self.primary,
            AccessibilityScreen::Alternate => &mut self.alternate,
        };
        if !state.rows.is_complete() {
            return None;
        }
        // Incomplete deltas may be followed by an empty completion.
        let document = if state.document_dirty {
            build_document(&state.rows, Some(&state.document))
        } else {
            Arc::clone(&state.document)
        };
        state.document_dirty = false;
        let document_changed = !Arc::ptr_eq(&document, &state.document);
        if document_changed {
            state.content_revision = state.content_revision.saturating_add(1);
            state.document = Arc::clone(&document);
        }
        self.pending_semantic_target = false;
        let data = TerminalAccessibilityData::retained(RetainedAccessibilityRequest {
            generation,
            content_revision: state.content_revision,
            screen,
            screen_generation,
            document,
            visible_lines,
            cursor,
            selection,
        });
        if !document_changed
            && self
                .latest
                .as_ref()
                .is_some_and(|latest| latest.data.presents_same(&data))
        {
            return None;
        }
        let model = TerminalAccessibilityModel {
            data: Arc::new(data),
        };
        self.latest = Some(model.clone());
        Some(Arc::new(model))
    }

    #[cfg(feature = "desktop")]
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
    rows: &RetainedRows,
    previous: Option<&Arc<AccessibilityDocument>>,
) -> Arc<AccessibilityDocument> {
    if rows.topology.is_empty() {
        return previous
            .filter(|document| document.rows == 0)
            .cloned()
            .unwrap_or_else(AccessibilityDocument::empty);
    }
    let Some(ordered) = rows.ordered() else {
        return previous
            .cloned()
            .unwrap_or_else(AccessibilityDocument::empty);
    };
    let same = |left: &Arc<AccessibilityRow>, right: &Option<Arc<AccessibilityRow>>| {
        right.as_ref().is_some_and(|right| Arc::ptr_eq(left, right))
    };
    if let Some(previous) = previous
        && previous.rows == ordered.len()
        && previous
            .blocks
            .iter()
            .flat_map(|block| block.rows.iter())
            .zip(ordered)
            .all(|(left, right)| same(left, right))
    {
        return Arc::clone(previous);
    }

    // Publishers compare topologies by identity, so an equal topology keeps the previous one.
    let topology = previous
        .filter(|document| document.topology == rows.topology)
        .map_or_else(
            || Arc::clone(&rows.topology),
            |document| Arc::clone(&document.topology),
        );
    let previous_blocks = previous
        .into_iter()
        .flat_map(|document| document.blocks.iter())
        .map(|block| (block.key(), Arc::clone(block)))
        .collect::<BTreeMap<_, _>>();
    let key = |row: &Option<Arc<AccessibilityRow>>| {
        row.as_ref()
            .map(|row| (row.id.screen_generation, row.id.node_serial))
    };
    let last_row = topology.len() - 1;
    let mut blocks = Vec::new();
    let mut row_start = 0_usize;
    while row_start < ordered.len() {
        let page = key(&ordered[row_start]);
        let len = ordered[row_start..]
            .iter()
            .take_while(|row| key(row) == page)
            .count();
        let group = &ordered[row_start..row_start + len];
        let is_tail = row_start + len == ordered.len();
        let block = page
            .and_then(|page| previous_blocks.get(&page))
            .filter(|block| block.shares_rows(group, is_tail))
            .cloned()
            .unwrap_or_else(|| {
                let group = group
                    .iter()
                    .map(|row| Arc::clone(row.as_ref().expect("an ordered document has every row")))
                    .collect::<Box<[_]>>();
                AccessibilityRowBlock::new(group, last_row, row_start)
            });
        row_start += len;
        blocks.push(block);
    }
    let mut blocks_by_key = (0..blocks.len()).collect::<Vec<_>>();
    blocks_by_key.sort_unstable_by_key(|block_index| {
        let (screen_generation, node_serial) = blocks[*block_index].key();
        (screen_generation, node_serial, *block_index)
    });
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
        topology,
        blocks: Arc::from(blocks),
        blocks_by_key: blocks_by_key.into_boxed_slice(),
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
    if let Some(cell) = row.cell_at_column(column) {
        return Some(
            row_start
                + if inclusive_end {
                    cell.utf16.end
                } else {
                    cell.utf16.start
                },
        );
    }
    let ending = column
        .checked_sub(1)
        .and_then(|last| row.cell_at_column(last))
        .filter(|cell| cell.columns.end == column);
    Some(row_start + ending.map_or(row.len_utf16(), |cell| cell.utf16.end))
}

/// The line and row a reference names, only while the document retains its row revision.
fn resolve_ref(
    document: &AccessibilityDocument,
    reference: AccessibilityCellRef,
) -> Option<(usize, &AccessibilityRow)> {
    let (line, row) = document.row_for_id(reference.row)?;
    (row.revision == reference.row_revision).then_some((line, row))
}

fn range_for_refs(
    document: &AccessibilityDocument,
    start: AccessibilityCellRef,
    end: AccessibilityCellRef,
) -> Option<Range<usize>> {
    let bounds = |reference: AccessibilityCellRef| {
        let (line, row) = resolve_ref(document, reference)?;
        Some((
            offset_for_column(document, line, row, reference.column, false)?,
            offset_for_column(document, line, row, reference.column, true)?,
        ))
    };
    let (start_begin, start_end) = bounds(start)?;
    let (end_begin, end_end) = bounds(end)?;
    Some(start_begin.min(end_begin)..start_end.max(end_end))
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
    fn retained_rows_keep_spans_and_a_utf16_index_only_when_they_need_them() {
        let id = row_id(AccessibilityScreen::Primary, 1, 10, 0);
        let retain = |cells: &[(&str, u16)]| {
            let mut column = 0;
            let cells = cells
                .iter()
                .map(|(text, width)| {
                    let cell = AccessibilityCell::at_column(*text, column, *width);
                    column += width;
                    cell
                })
                .collect();
            AccessibilityRow::from_update(AccessibilityRowUpdate {
                id,
                revision: 1,
                soft_wrapped: false,
                cells,
            })
        };
        let layout = |row: &AccessibilityRow| (row.spans().is_some(), row.utf16().is_some());
        let cells = |row: &AccessibilityRow| {
            row.cells()
                .map(|cell| (cell.columns, cell.bytes, cell.utf16))
                .collect::<Vec<_>>()
        };

        let ascii = retain(&[("a", 1), ("b", 1), ("c", 1)]);
        assert!(
            ascii.layout.is_none(),
            "an ASCII row of one column per character"
        );
        assert_eq!(
            cells(&ascii),
            [(0..1, 0..1, 0..1), (1..2, 1..2, 1..2), (2..3, 2..3, 2..3)]
        );

        let wide_ascii = retain(&[("a", 1), ("b", 2), ("c", 1)]);
        assert_eq!(layout(&wide_ascii), (true, false));
        assert_eq!(
            cells(&wide_ascii),
            [(0..1, 0..1, 0..1), (1..3, 1..2, 1..2), (3..4, 2..3, 2..3)]
        );
        assert_eq!(
            wide_ascii.cell_at_column(2).map(|cell| cell.columns),
            Some(1..3)
        );

        let accented = retain(&[("é", 1), ("x", 1)]);
        assert_eq!(layout(&accented), (false, true));
        assert_eq!(cells(&accented), [(0..1, 0..2, 0..1), (1..2, 2..3, 1..2)]);

        let emoji = retain(&[("😀", 2), ("e\u{301}", 1), ("z", 1)]);
        assert_eq!(layout(&emoji), (true, true));
        assert_eq!(
            cells(&emoji),
            [(0..2, 0..4, 0..2), (2..3, 4..7, 2..4), (3..4, 7..8, 4..5)]
        );
        assert_eq!(emoji.len_utf16(), 5);
        assert_eq!(emoji.cell_at_utf16(1).map(|cell| cell.columns), Some(0..2));
        assert_eq!(emoji.cell_at_utf16(3).map(|cell| cell.bytes), Some(4..7));
        assert!(emoji.is_cell_boundary(2) && !emoji.is_cell_boundary(3));
        assert_eq!(emoji.byte_for_utf16(2), Some(4));
        assert_eq!(emoji.byte_for_utf16(3), Some(5));
        assert_eq!(emoji.byte_for_utf16(5), Some(8));
        assert_eq!(
            ascii.reference(1),
            AccessibilityCellRef {
                row: id,
                row_revision: 1,
                column: 1
            },
            "a cell reference derives from the row's identity, revision and column"
        );
        assert!(
            std::mem::size_of::<AccessibilityRow>() <= 64,
            "an ASCII row retains its text and 64 bytes at most"
        );
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
        let resumed = state
            .apply(
                AccessibilityUpdate {
                    revision: 3,
                    screen: AccessibilityScreen::Primary,
                    screen_generation: 1,
                    complete: true,
                    more: false,
                    topology: None,
                    visible_lines: 0..1,
                    cursor: None,
                    selection: None,
                    changed_rows: vec![update_row(primary_id, 2, "resumed", false)],
                },
                PresentationGeneration::test(3),
            )
            .unwrap();
        assert_eq!(resumed.text(), "resumed");
        assert_eq!(alternate.text(), "alternate");
    }

    #[cfg(feature = "desktop")]
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

    #[cfg(feature = "desktop")]
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

    #[cfg(feature = "desktop")]
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
                PresentationGeneration::test(1),
            )
            .unwrap();

        assert_eq!(state.resolve_selection(&request), None);
        assert!(
            state
                .resolve_selection(&alternate.selection_request(0..1).unwrap())
                .is_some()
        );
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
    fn cursor_and_selection_resolve_rows_across_unordered_and_repeated_pages() {
        let id = |serial, page_row| row_id(AccessibilityScreen::Primary, 1, serial, page_row);
        // Page 10 lists its rows out of order and appears again after page 11.
        let topology = vec![
            id(10, 3),
            id(10, 1),
            id(10, 2),
            id(11, 1),
            id(11, 0),
            id(10, 5),
        ];
        let update = |cursor, selection| AccessibilityUpdate {
            revision: 1,
            screen: AccessibilityScreen::Primary,
            screen_generation: 1,
            complete: true,
            more: false,
            topology: Some(topology.clone()),
            visible_lines: 0..6,
            cursor,
            selection,
            changed_rows: topology
                .iter()
                .zip(["a", "b", "c", "d", "e", "f"])
                .map(|(id, text)| update_row(*id, 1, text, false))
                .collect(),
        };
        let cell = |row, row_revision| AccessibilityCellRef {
            row,
            row_revision,
            column: 0,
        };
        let mut state = TerminalAccessibilityState::default();
        let model = state
            .apply(
                update(
                    Some(cell(id(11, 0), 1)),
                    Some(AccessibilitySelectionRefs {
                        start: cell(id(10, 2), 1),
                        end: cell(id(10, 5), 1),
                        rectangle: false,
                    }),
                ),
                PresentationGeneration::test(1),
            )
            .unwrap();
        assert_eq!(model.text(), "a\nb\nc\nd\ne\nf");
        assert_eq!(model.cursor_cell(), Some((4, 0)));
        assert_eq!(model.cursor_range(), 8..8);
        assert_eq!(model.selection_range(), Some(4..11));

        // A missing identity or a stale row revision resolves nothing.
        let mut state = TerminalAccessibilityState::default();
        let model = state
            .apply(
                update(
                    Some(cell(id(10, 4), 1)),
                    Some(AccessibilitySelectionRefs {
                        start: cell(id(10, 2), 2),
                        end: cell(id(10, 5), 1),
                        rectangle: false,
                    }),
                ),
                PresentationGeneration::test(1),
            )
            .unwrap();
        assert_eq!(model.cursor_cell(), None);
        assert_eq!(model.cursor_range(), 0..0);
        assert_eq!(model.selection_range(), None);
        assert!(model.selection_refs().is_none());
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
        notifications.extend(AccessibilityNotifications::ORDERED);
        assert_eq!(notifications.len(), 3);
        assert_eq!(
            notifications.take().iter().collect::<Vec<_>>(),
            AccessibilityNotifications::ORDERED
        );
        assert!(notifications.is_empty());
    }

    #[test]
    fn retained_membership_tracks_trim_and_incomplete_topology_replacement() {
        let a = row_id(AccessibilityScreen::Primary, 1, 10, 0);
        let b = row_id(AccessibilityScreen::Primary, 1, 10, 1);
        let c = row_id(AccessibilityScreen::Primary, 1, 10, 2);
        let update = |topology, changed_rows, complete| AccessibilityUpdate {
            revision: 1,
            screen: AccessibilityScreen::Primary,
            screen_generation: 1,
            complete,
            more: !complete,
            topology,
            visible_lines: 0..4,
            cursor: None,
            selection: None,
            changed_rows,
        };
        let mut state = TerminalAccessibilityState::default();
        let initial = state
            .apply(
                update(
                    Some(vec![a, b]),
                    vec![update_row(a, 1, "a", false), update_row(b, 1, "b", false)],
                    true,
                ),
                PresentationGeneration::test(1),
            )
            .unwrap();
        assert_eq!(initial.text(), "a\nb");
        assert!(
            state
                .apply(
                    update(Some(vec![b, c]), vec![update_row(b, 2, "B", false)], false),
                    PresentationGeneration::test(2),
                )
                .is_none()
        );
        let replaced = state
            .apply(
                update(
                    None,
                    vec![
                        update_row(a, 2, "stale", false),
                        update_row(c, 1, "C", false),
                    ],
                    true,
                ),
                PresentationGeneration::test(3),
            )
            .unwrap();
        assert_eq!(replaced.text(), "B\nC");

        // A trimmed row's rejected continuation cannot satisfy a later topology.
        assert!(
            state
                .apply(
                    update(Some(vec![a, b, c]), vec![], false),
                    PresentationGeneration::test(4)
                )
                .is_none()
        );
        assert!(
            state
                .apply(update(None, vec![], true), PresentationGeneration::test(5))
                .is_none()
        );
        let restored = state
            .apply(
                update(None, vec![update_row(a, 3, "A", false)], true),
                PresentationGeneration::test(6),
            )
            .unwrap();
        assert_eq!(restored.text(), "A\nB\nC");

        // Membership is unordered; publication must retain the topology's order.
        let reordered = state
            .apply(
                update(Some(vec![c, b, a, c]), vec![], true),
                PresentationGeneration::test(7),
            )
            .unwrap();
        assert_eq!(reordered.text(), "C\nB\nA\nC");
        let empty = state
            .apply(
                update(Some(vec![]), vec![], true),
                PresentationGeneration::test(8),
            )
            .unwrap();
        assert_eq!(empty.text(), "");
        let still_empty = state
            .apply(
                update(None, vec![update_row(c, 2, "stale", false)], true),
                PresentationGeneration::test(9),
            )
            .unwrap();
        assert_eq!(still_empty.text(), "");
        assert!(
            state
                .apply(
                    update(Some(vec![c]), vec![], true),
                    PresentationGeneration::test(10)
                )
                .is_none()
        );
        let fresh = state
            .apply(
                update(None, vec![update_row(c, 3, "fresh", false)], true),
                PresentationGeneration::test(11),
            )
            .unwrap();
        assert_eq!(fresh.text(), "fresh");
    }

    #[test]
    fn retained_rows_reject_a_row_the_topology_does_not_name() {
        let live = row_id(AccessibilityScreen::Primary, 1, 10, 0);
        let outside = row_id(AccessibilityScreen::Primary, 1, 10, 1);
        let mut rows = RetainedRows::default();
        rows.set_topology(&Arc::from([live]));

        assert_eq!(
            rows.insert(AccessibilityRow::from_update(update_row(
                outside, 1, "outside", false
            ))),
            Err(RowOutsideTopology)
        );
        assert!(
            !rows.is_complete(),
            "a rejected row cannot stand in for a missing live row"
        );
        assert_eq!(
            rows.insert(AccessibilityRow::from_update(update_row(
                live, 1, "live", false
            ))),
            Ok(())
        );
        assert!(rows.is_complete());
    }

    #[test]
    fn a_same_generation_update_publishes_a_changed_cursor_selection_or_visible_start() {
        let ids =
            [0, 1, 2, 3].map(|page_row| row_id(AccessibilityScreen::Primary, 1, 10, page_row));
        let cell = |line: usize, column: u16| AccessibilityCellRef {
            row: ids[line],
            row_revision: 1,
            column,
        };
        let update = |topology: bool,
                      visible_lines: Range<usize>,
                      cursor: AccessibilityCellRef,
                      selection: Option<AccessibilitySelectionRefs>| {
            AccessibilityUpdate {
                revision: 1,
                screen: AccessibilityScreen::Primary,
                screen_generation: 1,
                complete: true,
                more: false,
                topology: topology.then(|| ids.to_vec()),
                visible_lines,
                cursor: Some(cursor),
                selection,
                changed_rows: if topology {
                    ["ab", "cd", "ef", "gh"]
                        .iter()
                        .zip(ids)
                        .map(|(text, id)| update_row(id, 1, text, false))
                        .collect()
                } else {
                    Vec::new()
                },
            }
        };
        let generation = PresentationGeneration::test(1);
        let selection = AccessibilitySelectionRefs {
            start: cell(0, 0),
            end: cell(0, 1),
            rectangle: false,
        };
        let mut state = TerminalAccessibilityState::default();
        let initial = state
            .apply(update(true, 0..4, cell(0, 0), None), generation)
            .unwrap();
        assert_eq!(initial.text(), "ab\ncd\nef\ngh");
        assert!(
            state
                .apply(update(false, 0..4, cell(0, 0), None), generation)
                .is_none(),
            "an unchanged presentation is not republished"
        );

        let moved = state
            .apply(update(false, 0..4, cell(0, 1), None), generation)
            .expect("a cursor move alone is published");
        assert_eq!(moved.cursor_range(), 1..1);
        let selected = state
            .apply(update(false, 0..4, cell(0, 1), Some(selection)), generation)
            .expect("a Selection change alone is published");
        assert_eq!(selected.selection_range(), Some(0..2));
        let scrolled = state
            .apply(update(false, 1..4, cell(0, 1), Some(selection)), generation)
            .expect("a visible start change alone is published");
        assert_eq!(scrolled.visible_lines(), 1..4);
        assert!(
            state
                .apply(update(false, 1..9, cell(0, 1), Some(selection)), generation)
                .is_none(),
            "a visible end past the document end clamps to the published visible lines"
        );
    }

    #[test]
    fn retained_membership_rejects_stale_generations_and_invalid_topologies() {
        let old = row_id(AccessibilityScreen::Primary, 1, 10, 0);
        let new = row_id(AccessibilityScreen::Primary, 2, 10, 0);
        let other_screen = row_id(AccessibilityScreen::Alternate, 2, 10, 0);
        let update = |screen_generation, topology, changed_rows, complete| AccessibilityUpdate {
            revision: 1,
            screen: AccessibilityScreen::Primary,
            screen_generation,
            complete,
            more: !complete,
            topology,
            visible_lines: 0..1,
            cursor: None,
            selection: None,
            changed_rows,
        };
        let mut state = TerminalAccessibilityState::default();
        assert_eq!(
            state
                .apply(
                    update(
                        1,
                        Some(vec![old]),
                        vec![update_row(old, 1, "old", false)],
                        true
                    ),
                    PresentationGeneration::test(1)
                )
                .unwrap()
                .text(),
            "old"
        );
        assert!(
            state
                .apply(
                    update(
                        2,
                        Some(vec![new]),
                        vec![update_row(old, 2, "stale", false)],
                        false
                    ),
                    PresentationGeneration::test(2)
                )
                .is_none()
        );
        assert!(
            state
                .apply(
                    update(2, None, vec![update_row(old, 3, "stale", false)], true),
                    PresentationGeneration::test(3)
                )
                .is_none()
        );
        let current = state
            .apply(
                update(2, None, vec![update_row(new, 1, "new", false)], true),
                PresentationGeneration::test(4),
            )
            .unwrap();
        assert_eq!(current.text(), "new");

        for invalid in [old, other_screen] {
            assert!(
                state
                    .apply(
                        update(
                            2,
                            Some(vec![invalid]),
                            vec![update_row(new, 2, "rejected", false)],
                            true
                        ),
                        PresentationGeneration::test(5)
                    )
                    .is_none()
            );
        }
        let retained = state
            .apply(
                update(2, None, vec![update_row(new, 2, "retained", false)], true),
                PresentationGeneration::test(6),
            )
            .unwrap();
        assert_eq!(retained.text(), "retained");
    }
    #[test]
    fn incomplete_topology_changes_publish_on_empty_completion() {
        let a = row_id(AccessibilityScreen::Primary, 1, 10, 0);
        let b = row_id(AccessibilityScreen::Primary, 1, 10, 1);
        let update = |topology, changed_rows, complete| AccessibilityUpdate {
            revision: 1,
            screen: AccessibilityScreen::Primary,
            screen_generation: 1,
            complete,
            more: !complete,
            topology,
            visible_lines: 0..2,
            cursor: None,
            selection: None,
            changed_rows,
        };
        for (topology, expected) in [(vec![b], "b"), (vec![b, a], "b\na"), (vec![], "")] {
            let mut state = TerminalAccessibilityState::default();
            let initial = state
                .apply(
                    update(
                        Some(vec![a, b]),
                        vec![update_row(a, 1, "a", false), update_row(b, 1, "b", false)],
                        true,
                    ),
                    PresentationGeneration::test(1),
                )
                .unwrap();
            assert!(
                state
                    .apply(
                        update(Some(topology.clone()), vec![], false),
                        PresentationGeneration::test(2)
                    )
                    .is_none()
            );
            assert_eq!(state.latest.as_ref().unwrap().text(), "a\nb");
            // Repeating the pending topology cannot clear its unpublished change.
            assert!(
                state
                    .apply(
                        update(Some(topology), vec![], false),
                        PresentationGeneration::test(3)
                    )
                    .is_none()
            );
            let complete = state
                .apply(update(None, vec![], true), PresentationGeneration::test(4))
                .unwrap();
            assert_eq!(complete.text(), expected);
            assert!(complete.data.content_revision > initial.data.content_revision);
            assert_eq!(initial.text(), "a\nb");
            let unchanged = state
                .apply(update(None, vec![], true), PresentationGeneration::test(5))
                .unwrap();
            assert!(Arc::ptr_eq(
                &complete.data.document,
                &unchanged.data.document
            ));
            assert_eq!(
                complete.data.content_revision,
                unchanged.data.content_revision
            );
        }
    }

    #[test]
    fn incomplete_row_changes_survive_empty_completion_and_screen_switches() {
        let primary = row_id(AccessibilityScreen::Primary, 1, 10, 0);
        let alternate = row_id(AccessibilityScreen::Alternate, 2, 20, 0);
        let update =
            |screen, screen_generation, topology, changed_rows, complete| AccessibilityUpdate {
                revision: 1,
                screen,
                screen_generation,
                complete,
                more: !complete,
                topology,
                visible_lines: 0..1,
                cursor: None,
                selection: None,
                changed_rows,
            };
        let mut state = TerminalAccessibilityState::default();
        let initial = state
            .apply(
                update(
                    AccessibilityScreen::Primary,
                    1,
                    Some(vec![primary]),
                    vec![update_row(primary, 1, "primary", false)],
                    true,
                ),
                PresentationGeneration::test(1),
            )
            .unwrap();
        assert!(
            state
                .apply(
                    update(
                        AccessibilityScreen::Primary,
                        1,
                        None,
                        vec![update_row(primary, 2, "changed", false)],
                        false
                    ),
                    PresentationGeneration::test(2)
                )
                .is_none()
        );
        let other = state
            .apply(
                update(
                    AccessibilityScreen::Alternate,
                    2,
                    Some(vec![alternate]),
                    vec![update_row(alternate, 1, "alternate", false)],
                    true,
                ),
                PresentationGeneration::test(3),
            )
            .unwrap();
        assert_eq!(other.text(), "alternate");
        let complete = state
            .apply(
                update(AccessibilityScreen::Primary, 1, None, vec![], true),
                PresentationGeneration::test(4),
            )
            .unwrap();
        assert_eq!(complete.text(), "changed");
        assert!(complete.data.content_revision > initial.data.content_revision);
        assert_eq!(initial.text(), "primary");
        assert_eq!(other.text(), "alternate");
    }
}
