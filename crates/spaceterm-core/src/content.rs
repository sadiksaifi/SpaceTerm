//! Bounded immutable semantic terminal pages, not PTY replay or client view state.
//!
//! Producers and wire decoders use checked constructors. Wire codecs enforce
//! their own frame limits as well. No native proofs, Prompt Owner, selection,
//! Find query, scroll position, image allocations or reservations are represented.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::num::{NonZeroU32, NonZeroU64};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use unicode_segmentation::UnicodeSegmentation;

use crate::authority::{ContentRevision, SessionKey};
use crate::geometry::SUPPORTED_GRID_LIMITS;

pub const MAX_PAGE_ROWS: usize = 256;
pub const MAX_PAGE_CELLS: usize = 16_384;
/// Conservative semantic/wire byte accounting leaves 256 KiB of the 2 MiB frame
/// for envelope overhead. Actual wire encoders must independently bound frames.
pub const MAX_PAGE_BYTES: usize = 1792 * 1024;
pub const MAX_GRAPHEME_BYTES: usize = 1024;
pub const MAX_HYPERLINK_BYTES: usize = 4096;
pub const MAX_PAGE_HYPERLINKS: usize = 1024;
pub const MAX_PAGE_RESOURCES: usize = 128;
pub const MAX_PAGE_PLACEMENTS: usize = 1024;
/// RGBA presentation can expand a 96 MiB native RGB image by 4/3. This 128 MiB
/// resource ceiling does not replace the terminal's 96 MiB native storage limit
/// or its 384 MiB aggregate decoded-residency admission.
pub const MAX_GRAPHICS_RESOURCE_BYTES: u32 = 128 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
pub enum Screen {
    Primary,
    Alternate,
}
/// Pointer-free identity matching the VT engine's stable physical row contract.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
pub struct RowId {
    pub screen: Screen,
    pub screen_generation: u64,
    pub node_serial: u64,
    pub page_row: u16,
}
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ContentBase {
    pub session: SessionKey,
    pub revision: ContentRevision,
    pub screen: Screen,
    pub screen_generation: u64,
    pub topology_revision: u64,
}
impl ContentBase {
    pub fn contains(self, row: RowId) -> bool {
        row.screen == self.screen && row.screen_generation == self.screen_generation
    }
}
/// A continuation resumes within a physical row or advances past it.
/// Column offsets count cells, including wide-cell spacers, not UTF-8 bytes.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum PagePosition {
    AfterRow {
        row: RowId,
        row_revision: u64,
    },
    WithinRow {
        row: RowId,
        row_revision: u64,
        columns: u16,
        next_column: u16,
    },
}
impl PagePosition {
    pub const fn row(self) -> RowId {
        match self {
            Self::AfterRow { row, .. } | Self::WithinRow { row, .. } => row,
        }
    }
}
/// A continuation binds the Session, content, screen, topology and physical row
/// revisions. Producers revalidate these comparisons before reading more content.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PageContinuation {
    pub base: ContentBase,
    pub position: PagePosition,
}
impl PageContinuation {
    /// Continue immediately after this fragment without skipping a partial row.
    pub fn new(base: ContentBase, row: &ContentRow) -> Result<Self, ContentError> {
        if !base.contains(row.id) {
            return Err(ContentError::InvalidContinuation);
        }
        let position = if row.end_column() == row.columns {
            PagePosition::AfterRow {
                row: row.id,
                row_revision: row.revision,
            }
        } else {
            PagePosition::WithinRow {
                row: row.id,
                row_revision: row.revision,
                columns: row.columns,
                next_column: row.end_column(),
            }
        };
        Ok(Self { base, position })
    }
    pub fn matches(self, current: ContentBase) -> bool {
        let valid_position = match self.position {
            PagePosition::AfterRow { .. } => true,
            PagePosition::WithinRow {
                columns,
                next_column,
                ..
            } => {
                next_column > 0
                    && next_column < columns
                    && u32::from(columns) <= SUPPORTED_GRID_LIMITS.max_columns()
            }
        };
        self.base == current && current.contains(self.position.row()) && valid_position
    }
    /// Compare a subsequent fragment before appending its cells to the same row.
    /// AfterRow requires resolving the next row in the producer's fenced topology.
    pub fn continues_row(self, current: ContentBase, next: &ContentRow) -> bool {
        self.matches(current)
            && matches!(self.position, PagePosition::WithinRow {
                row, row_revision, columns, next_column,
            } if row == next.id
                && row_revision == next.revision
                && columns == next.columns
                && next_column == next.start_column)
    }
}
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CellRef {
    pub session: SessionKey,
    pub row: RowId,
    pub row_revision: u64,
    pub column: u16,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum CellWidth {
    Single,
    Wide,
    SpacerHead,
    SpacerTail,
}
/// One complete extended grapheme, or an explicit wide-cell spacer. Debug never
/// exposes terminal text. Very large clusters fail admission rather than truncate.
#[derive(Clone, Eq, PartialEq)]
pub struct CellContent {
    text: String,
    width: CellWidth,
}
impl CellContent {
    pub fn grapheme(text: String, width: CellWidth) -> Result<Self, ContentError> {
        if !matches!(width, CellWidth::Single | CellWidth::Wide)
            || text.is_empty()
            || text.len() > MAX_GRAPHEME_BYTES
            || text.chars().any(char::is_control)
            || text.graphemes(true).count() != 1
        {
            return Err(ContentError::InvalidGrapheme);
        }
        Ok(Self { text, width })
    }
    pub fn spacer_head() -> Self {
        Self {
            text: String::new(),
            width: CellWidth::SpacerHead,
        }
    }
    pub fn spacer_tail() -> Self {
        Self {
            text: String::new(),
            width: CellWidth::SpacerTail,
        }
    }
    pub fn text(&self) -> &str {
        &self.text
    }
    pub const fn width(&self) -> CellWidth {
        self.width
    }
}
impl fmt::Debug for CellContent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CellContent")
            .field("text", &"[redacted]")
            .field("width", &self.width)
            .finish()
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Rgb {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub enum TerminalColor {
    #[default]
    Default,
    Palette(u8),
    Rgb(Rgb),
}
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub enum Underline {
    #[default]
    None,
    Single,
    Double,
    Curly,
    Dotted,
    Dashed,
}
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub enum SemanticContent {
    #[default]
    Output,
    Input,
    Prompt,
}
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct CellStyle {
    pub foreground: TerminalColor,
    pub background: TerminalColor,
    pub underline_color: TerminalColor,
    pub underline: Underline,
    pub bold: bool,
    pub faint: bool,
    pub italic: bool,
    pub blinking: bool,
    pub inverse: bool,
    pub invisible: bool,
    pub strikethrough: bool,
    pub overline: bool,
    pub semantic: SemanticContent,
}
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct HyperlinkId(NonZeroU32);
impl HyperlinkId {
    pub fn new(value: u32) -> Result<Self, ContentError> {
        Ok(Self(
            NonZeroU32::new(value).ok_or(ContentError::InvalidHyperlink)?,
        ))
    }
    pub const fn get(self) -> u32 {
        self.0.get()
    }
}
/// Untrusted descriptor, not a native-capable hyperlink target or file authority.
#[derive(Clone, Eq, PartialEq)]
pub struct HyperlinkDescriptor {
    id: HyperlinkId,
    target: String,
}
impl HyperlinkDescriptor {
    pub fn new(id: HyperlinkId, target: String) -> Result<Self, ContentError> {
        if target.is_empty()
            || target.len() > MAX_HYPERLINK_BYTES
            || target.chars().any(char::is_control)
        {
            return Err(ContentError::InvalidHyperlink);
        }
        Ok(Self { id, target })
    }
    pub const fn id(&self) -> HyperlinkId {
        self.id
    }
    pub fn target(&self) -> &str {
        &self.target
    }
}
impl fmt::Debug for HyperlinkDescriptor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HyperlinkDescriptor")
            .field("id", &self.id)
            .field("target", &"[redacted]")
            .finish()
    }
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StyledCell {
    content: CellContent,
    style: CellStyle,
    hyperlink: Option<HyperlinkId>,
}
impl StyledCell {
    pub const fn new(
        content: CellContent,
        style: CellStyle,
        hyperlink: Option<HyperlinkId>,
    ) -> Self {
        Self {
            content,
            style,
            hyperlink,
        }
    }
    pub fn content(&self) -> &CellContent {
        &self.content
    }
    pub const fn style(&self) -> CellStyle {
        self.style
    }
    pub const fn hyperlink(&self) -> Option<HyperlinkId> {
        self.hyperlink
    }
}
/// A complete physical row or a contiguous, revision-fenced fragment of one.
/// Every fragment repeats the full row width and soft-wrap fact. Clients assemble
/// only matching bases and row revisions, and publish no incomplete row.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ContentRow {
    id: RowId,
    revision: u64,
    soft_wrapped: bool,
    columns: u16,
    start_column: u16,
    cells: Vec<StyledCell>,
}
impl ContentRow {
    /// Construct a complete row. Page widths must agree with its cell count.
    pub fn new(
        id: RowId,
        revision: u64,
        soft_wrapped: bool,
        cells: Vec<StyledCell>,
    ) -> Result<Self, ContentError> {
        let columns = u16::try_from(cells.len()).map_err(|_| ContentError::InvalidRow)?;
        Self::fragment(id, revision, soft_wrapped, columns, 0, cells)
    }
    /// Construct a nonempty fragment within the full physical width. A boundary
    /// cannot separate a Wide cell from its SpacerTail or split a grapheme.
    pub fn fragment(
        id: RowId,
        revision: u64,
        soft_wrapped: bool,
        columns: u16,
        start_column: u16,
        cells: Vec<StyledCell>,
    ) -> Result<Self, ContentError> {
        let count = u16::try_from(cells.len()).map_err(|_| ContentError::InvalidRow)?;
        if count == 0 || columns == 0 || u32::from(columns) > SUPPORTED_GRID_LIMITS.max_columns() {
            return Err(ContentError::InvalidRow);
        }
        let end_column = start_column
            .checked_add(count)
            .filter(|end| *end <= columns)
            .ok_or(ContentError::InvalidRowFragment)?;
        for (index, cell) in cells.iter().enumerate() {
            match cell.content.width {
                CellWidth::Wide
                    if cells
                        .get(index + 1)
                        .is_none_or(|next| next.content.width != CellWidth::SpacerTail) =>
                {
                    return Err(ContentError::InvalidSpacer);
                }
                CellWidth::SpacerTail
                    if index == 0 || cells[index - 1].content.width != CellWidth::Wide =>
                {
                    return Err(ContentError::InvalidSpacer);
                }
                CellWidth::SpacerHead
                    if end_column != columns || index + 1 != cells.len() || !soft_wrapped =>
                {
                    return Err(ContentError::InvalidSpacer);
                }
                _ => {}
            }
        }
        Ok(Self {
            id,
            revision,
            soft_wrapped,
            columns,
            start_column,
            cells,
        })
    }
    pub const fn id(&self) -> RowId {
        self.id
    }
    pub const fn revision(&self) -> u64 {
        self.revision
    }
    pub const fn soft_wrapped(&self) -> bool {
        self.soft_wrapped
    }
    pub const fn columns(&self) -> u16 {
        self.columns
    }
    pub const fn start_column(&self) -> u16 {
        self.start_column
    }
    /// Exclusive end, bounded by the constructor to the full physical row width.
    pub fn end_column(&self) -> u16 {
        self.start_column + self.cells.len() as u16
    }
    pub fn cells(&self) -> &[StyledCell] {
        &self.cells
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum CursorShape {
    Block,
    Underline,
    Bar,
}
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Cursor {
    pub row: RowId,
    pub column: u16,
    pub shape: CursorShape,
    pub visible: bool,
    pub blinking: bool,
}
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub enum MouseTracking {
    #[default]
    None,
    Press,
    Button,
    Any,
}
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub enum MouseEncoding {
    #[default]
    X10,
    Utf8,
    Sgr,
    Urxvt,
    SgrPixels,
}
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct TerminalModes {
    pub application_cursor: bool,
    pub application_keypad: bool,
    pub bracketed_paste: bool,
    pub focus_reporting: bool,
    pub mouse_tracking: MouseTracking,
    pub mouse_encoding: MouseEncoding,
    pub synchronized_output: bool,
    pub origin: bool,
    pub insert: bool,
    pub wraparound: bool,
    pub reverse_colors: bool,
    pub hidden_input: bool,
}
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PaletteOverride {
    pub index: u8,
    pub color: Rgb,
}
/// Program overrides only. Client fonts, themes and graphical objects are absent.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct ProgramColorOverrides {
    pub foreground: Option<Rgb>,
    pub background: Option<Rgb>,
    pub cursor: Option<Rgb>,
    pub palette: Vec<PaletteOverride>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct GraphicsResourceId(NonZeroU64);
impl GraphicsResourceId {
    pub fn new(value: u64) -> Result<Self, ContentError> {
        Ok(Self(
            NonZeroU64::new(value).ok_or(ContentError::InvalidGraphics)?,
        ))
    }
    pub const fn get(self) -> u64 {
        self.0.get()
    }
}
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct GraphicsResourceKey {
    pub session: SessionKey,
    pub id: GraphicsResourceId,
    pub generation: u64,
}
/// References carry no pixels or process-local reservations. Transfers and decoded
/// residency are separately admitted by terminal, transport and client composition.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct GraphicsResourceRef {
    pub key: GraphicsResourceKey,
    pub width: u32,
    pub height: u32,
    pub rgba_bytes: u32,
    pub digest: [u8; 32],
}
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ImageRect {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct GraphicsPlacement {
    pub resource: GraphicsResourceKey,
    pub placement_id: u64,
    /// The stable anchor can be outside this page when the image spans pages.
    pub anchor: RowId,
    /// Signed column preserves left-clipped placements without moving the image.
    pub column: i32,
    pub columns: u16,
    pub rows: u16,
    pub pixel_offset_x: u32,
    pub pixel_offset_y: u32,
    pub source: ImageRect,
    pub destination_width: u32,
    pub destination_height: u32,
    pub unicode_placeholder: bool,
    pub z_index: i32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ContentPageParts {
    pub base: ContentBase,
    pub columns: u16,
    /// At most one fragment per physical row. Only the first row may start after
    /// column zero; only the last row may end before the full width. A partial
    /// last row requires an exact WithinRow continuation.
    pub rows: Vec<ContentRow>,
    pub hyperlinks: Vec<HyperlinkDescriptor>,
    pub cursor: Option<Cursor>,
    pub modes: TerminalModes,
    pub colors: ProgramColorOverrides,
    pub resources: Vec<GraphicsResourceRef>,
    pub placements: Vec<GraphicsPlacement>,
    pub continuation: Option<PageContinuation>,
}
impl ContentPageParts {
    pub fn new(base: ContentBase, columns: u16) -> Self {
        Self {
            base,
            columns,
            rows: Vec::new(),
            hyperlinks: Vec::new(),
            cursor: None,
            modes: TerminalModes::default(),
            colors: ProgramColorOverrides::default(),
            resources: Vec::new(),
            placements: Vec::new(),
            continuation: None,
        }
    }
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ContentPage {
    parts: ContentPageParts,
    accounted_bytes: usize,
}
impl ContentPage {
    pub fn new(parts: ContentPageParts) -> Result<Self, ContentError> {
        if parts.rows.len() > MAX_PAGE_ROWS {
            return Err(ContentError::TooManyRows {
                count: parts.rows.len(),
            });
        }
        if parts.columns == 0 || u32::from(parts.columns) > SUPPORTED_GRID_LIMITS.max_columns() {
            return Err(ContentError::InvalidRow);
        }
        let cells = parts
            .rows
            .iter()
            .try_fold(0usize, |count, row| count.checked_add(row.cells.len()))
            .ok_or(ContentError::TooManyBytes)?;
        if cells > MAX_PAGE_CELLS {
            return Err(ContentError::TooManyCells { count: cells });
        }
        if parts.hyperlinks.len() > MAX_PAGE_HYPERLINKS
            || parts.resources.len() > MAX_PAGE_RESOURCES
            || parts.placements.len() > MAX_PAGE_PLACEMENTS
            || parts.colors.palette.len() > 256
        {
            return Err(ContentError::TooManyDescriptors);
        }
        let mut palette = BTreeSet::new();
        if parts
            .colors
            .palette
            .iter()
            .any(|value| !palette.insert(value.index))
        {
            return Err(ContentError::DuplicatePalette);
        }
        let mut links = BTreeSet::new();
        if parts.hyperlinks.iter().any(|value| !links.insert(value.id)) {
            return Err(ContentError::InvalidHyperlink);
        }
        let mut row_ids = BTreeSet::new();
        let mut bytes = 4096usize;
        for (index, row) in parts.rows.iter().enumerate() {
            if !parts.base.contains(row.id) {
                return Err(ContentError::WrongScreen);
            }
            if !row_ids.insert(row.id) {
                return Err(ContentError::DuplicateRow);
            }
            if row.columns != parts.columns {
                return Err(ContentError::WrongRowWidth {
                    count: usize::from(row.columns),
                });
            }
            if (index != 0 && row.start_column != 0)
                || (index + 1 != parts.rows.len() && row.end_column() != parts.columns)
            {
                return Err(ContentError::InvalidRowFragment);
            }
            bytes = add_bytes(bytes, 64)?;
            for cell in &row.cells {
                if cell.hyperlink.is_some_and(|id| !links.contains(&id)) {
                    return Err(ContentError::InvalidHyperlink);
                }
                bytes = add_bytes(bytes, 96 + 6 * cell.content.text.len())?;
            }
        }
        if parts.cursor.is_some_and(|cursor| {
            !parts.base.contains(cursor.row) || cursor.column >= parts.columns
        }) {
            return Err(ContentError::InvalidCursor);
        }
        for link in &parts.hyperlinks {
            bytes = add_bytes(bytes, 32 + 6 * link.target.len())?;
        }
        let mut resources = BTreeMap::new();
        for resource in &parts.resources {
            let expected = resource
                .width
                .checked_mul(resource.height)
                .and_then(|pixels| pixels.checked_mul(4));
            if resource.key.session != parts.base.session
                || resource.width == 0
                || resource.height == 0
                || expected != Some(resource.rgba_bytes)
                || resource.rgba_bytes > MAX_GRAPHICS_RESOURCE_BYTES
                || resources.insert(resource.key, resource).is_some()
            {
                return Err(ContentError::InvalidGraphics);
            }
            bytes = add_bytes(bytes, 128)?;
        }
        let mut placements = BTreeSet::new();
        for placement in &parts.placements {
            let resource = resources
                .get(&placement.resource)
                .ok_or(ContentError::InvalidGraphics)?;
            if !parts.base.contains(placement.anchor)
                || placement
                    .column
                    .checked_add(i32::from(placement.columns))
                    .is_none()
                || placement.columns == 0
                || placement.rows == 0
                || placement.destination_width == 0
                || placement.destination_height == 0
                || placement.source.width == 0
                || placement.source.height == 0
                || placement
                    .source
                    .x
                    .checked_add(placement.source.width)
                    .is_none_or(|end| end > resource.width)
                || placement
                    .source
                    .y
                    .checked_add(placement.source.height)
                    .is_none_or(|end| end > resource.height)
                || !placements.insert((placement.resource, placement.placement_id))
            {
                return Err(ContentError::InvalidGraphics);
            }
            bytes = add_bytes(bytes, 128)?;
        }
        match (parts.rows.last(), parts.continuation) {
            (Some(row), Some(continuation)) => {
                if continuation != PageContinuation::new(parts.base, row)? {
                    return Err(ContentError::InvalidContinuation);
                }
            }
            (Some(row), None) if row.end_column() != parts.columns => {
                return Err(ContentError::InvalidContinuation);
            }
            (None, Some(_)) => return Err(ContentError::InvalidContinuation),
            _ => {}
        }
        Ok(Self {
            parts,
            accounted_bytes: bytes,
        })
    }
    pub fn parts(&self) -> &ContentPageParts {
        &self.parts
    }
    pub const fn accounted_bytes(&self) -> usize {
        self.accounted_bytes
    }
    pub fn into_parts(self) -> ContentPageParts {
        self.parts
    }
}
fn add_bytes(current: usize, additional: usize) -> Result<usize, ContentError> {
    current
        .checked_add(additional)
        .filter(|bytes| *bytes <= MAX_PAGE_BYTES)
        .ok_or(ContentError::TooManyBytes)
}
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum ContentError {
    #[error("semantic page has too many rows ({count})")]
    TooManyRows { count: usize },
    #[error("semantic page has too many cells ({count})")]
    TooManyCells { count: usize },
    #[error("semantic page exceeds byte limit")]
    TooManyBytes,
    #[error("semantic page has too many descriptors")]
    TooManyDescriptors,
    #[error("semantic cell does not contain one complete bounded grapheme")]
    InvalidGrapheme,
    #[error("semantic row is empty or exceeds column limit")]
    InvalidRow,
    #[error("semantic row fragment has an invalid range or page position")]
    InvalidRowFragment,
    #[error("semantic row has invalid width spacers")]
    InvalidSpacer,
    #[error("semantic row width is inconsistent ({count})")]
    WrongRowWidth { count: usize },
    #[error("semantic page repeats a stable row identity")]
    DuplicateRow,
    #[error("semantic row belongs to another screen generation")]
    WrongScreen,
    #[error("semantic page has an invalid hyperlink descriptor or reference")]
    InvalidHyperlink,
    #[error("semantic page has an invalid cursor")]
    InvalidCursor,
    #[error("semantic page repeats a program palette override")]
    DuplicatePalette,
    #[error("semantic page has an invalid graphics reference or placement")]
    InvalidGraphics,
    #[error("semantic continuation has a stale or missing base")]
    InvalidContinuation,
}
