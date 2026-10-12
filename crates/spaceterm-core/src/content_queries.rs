//! Canonical engine-free, revision-exact terminal reads and resource completions.
//!
//! Requests and continuations are comparison data, never authority. Producers
//! validate current grant/connection/context, actual topology and row revisions
//! at admission and during work. Wire adapters enforce their own byte limits.
//! No personal viewport, persistent selection, Find navigation or native proof is
//! installed in the authoritative terminal by any record in this module.

use std::collections::BTreeSet;
use std::fmt;
use std::num::NonZeroU64;

use sha2::{Digest, Sha256};

use crate::authority::EffectFence;
use crate::content::{
    CellRef, ContentBase, ContentPage, GraphicsResourceRef, PageContinuation, RowId,
};
use crate::content::{
    MAX_GRAPHEME_BYTES, MAX_GRAPHICS_RESOURCE_BYTES, MAX_HYPERLINK_BYTES, MAX_PAGE_CELLS,
    MAX_PAGE_ROWS,
};
use crate::geometry::SUPPORTED_GRID_LIMITS;

/// Runtime serialized-content admission; this does not change MAX_PAGE_BYTES.
pub const MAX_QUERY_PAGE_BYTES: usize = 1024 * 1024;
pub const MAX_RESOURCE_CHUNK_BYTES: usize = 256 * 1024;
pub const MAX_FIND_QUERY_BYTES: usize = 4096;
pub const MAX_FIND_CHUNK_MATCHES: usize = 256;
pub const MAX_FIND_CHUNK_BYTES: usize = 64 * 1024;
pub const SUPPORTED_SCROLLBACK_ROWS: u64 = 10_000;
/// Conservative per-cell opening/closing style markup for the pinned formatter.
/// Producer tests must check this against every supported style combination.
pub const COPY_STYLE_MARKUP_BYTES_PER_CELL: u64 = 1024;
pub const COPY_LINK_MARKUP_BYTES_PER_CELL: u64 = 64;
pub const COPY_DOCUMENT_MARKUP_BYTES: u64 = 64 * 1024;

/// Closed failure metadata. No query, terminal contents or native errors escape.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum QueryError {
    #[error("content query binding is inconsistent")]
    InvalidBinding,
    #[error("content query range is invalid")]
    InvalidRange,
    #[error("content query exceeds its bound")]
    Limit,
    #[error("content query counter or length exhausted")]
    Exhausted,
    #[error("content chunk is not valid UTF-8")]
    InvalidEncoding,
}

macro_rules! query_id {
    ($name:ident, $doc:literal) => {
        #[doc = $doc]
        #[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
        pub struct $name(NonZeroU64);
        impl $name {
            pub fn new(value: u64) -> Result<Self, QueryError> {
                NonZeroU64::new(value)
                    .map(Self)
                    .ok_or(QueryError::InvalidBinding)
            }
            pub const fn get(self) -> u64 {
                self.0.get()
            }
            pub fn checked_successor(self) -> Result<Self, QueryError> {
                Self::new(self.get().checked_add(1).ok_or(QueryError::Exhausted)?)
            }
        }
    };
}
query_id!(
    ContentRequestId,
    "Connection-scoped read identity, distinct from durable OperationId."
);
query_id!(
    ResourceTransferId,
    "Connection-scoped resource transfer identity; never reused within its connection."
);

/// Checked client request generation. Zero is a valid initial generation.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct RequestGeneration(u64);
impl RequestGeneration {
    pub const fn new(value: u64) -> Self {
        Self(value)
    }
    pub const fn get(self) -> u64 {
        self.0
    }
    pub fn checked_successor(self) -> Result<Self, QueryError> {
        self.0.checked_add(1).map(Self).ok_or(QueryError::Exhausted)
    }
}

/// Exact Session/content/screen/topology plus original effect and request binding.
/// The fence must explicitly capture the same content revision as the base.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct QueryStamp {
    fence: EffectFence,
    base: ContentBase,
    request: ContentRequestId,
    generation: RequestGeneration,
}
impl QueryStamp {
    pub fn new(
        fence: EffectFence,
        base: ContentBase,
        request: ContentRequestId,
        generation: RequestGeneration,
    ) -> Result<Self, QueryError> {
        if fence.session() != base.session || fence.content() != Some(base.revision) {
            return Err(QueryError::InvalidBinding);
        }
        Ok(Self {
            fence,
            base,
            request,
            generation,
        })
    }
    pub const fn fence(self) -> EffectFence {
        self.fence
    }
    pub const fn base(self) -> ContentBase {
        self.base
    }
    pub const fn request(self) -> ContentRequestId {
        self.request
    }
    pub const fn generation(self) -> RequestGeneration {
        self.generation
    }
}

fn check_cell(stamp: QueryStamp, cell: CellRef) -> Result<(), QueryError> {
    if cell.session != stamp.base().session || !stamp.base().contains(cell.row) {
        return Err(QueryError::InvalidBinding);
    }
    if u32::from(cell.column) >= SUPPORTED_GRID_LIMITS.max_columns() {
        return Err(QueryError::InvalidRange);
    }
    Ok(())
}

/// Checked per-page bounds. A small byte bound may produce a typed resource
/// rejection when a complete grapheme cannot fit; it never permits truncation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PageLimits {
    rows: u16,
    cells: u32,
    bytes: u32,
}
impl PageLimits {
    pub fn new(rows: u16, cells: u32, bytes: u32) -> Result<Self, QueryError> {
        if rows == 0
            || cells == 0
            || bytes == 0
            || usize::from(rows) > MAX_PAGE_ROWS
            || u64::from(cells) > MAX_PAGE_CELLS as u64
            || u64::from(bytes) > MAX_QUERY_PAGE_BYTES as u64
        {
            return Err(QueryError::Limit);
        }
        Ok(Self { rows, cells, bytes })
    }
    pub const fn rows(self) -> u16 {
        self.rows
    }
    pub const fn cells(self) -> u32 {
        self.cells
    }
    pub const fn bytes(self) -> u32 {
        self.bytes
    }
}

/// Range refers to committed content, never to a mutable server viewport.
/// History endpoints are inclusive physical rows; topology order is checked by
/// the producer rather than inferred from RowId's lexical representation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ContentRange {
    ActiveGrid,
    History {
        first: Option<RowId>,
        last: Option<RowId>,
    },
}

/// Adds the original request binding to Wave 1's exact fragment continuation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ContentContinuation {
    stamp: QueryStamp,
    page: PageContinuation,
}
impl ContentContinuation {
    pub fn new(stamp: QueryStamp, page: PageContinuation) -> Result<Self, QueryError> {
        if !page.matches(stamp.base()) {
            return Err(QueryError::InvalidBinding);
        }
        Ok(Self { stamp, page })
    }
    pub const fn stamp(self) -> QueryStamp {
        self.stamp
    }
    pub const fn page(self) -> PageContinuation {
        self.page
    }
}

/// Immutable bounded page request. Continuation reuse cannot change the stamp.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ContentPageRequest {
    stamp: QueryStamp,
    range: ContentRange,
    continuation: Option<ContentContinuation>,
    limits: PageLimits,
}
impl ContentPageRequest {
    pub fn new(
        stamp: QueryStamp,
        range: ContentRange,
        continuation: Option<ContentContinuation>,
        limits: PageLimits,
    ) -> Result<Self, QueryError> {
        if let ContentRange::History { first, last } = range
            && (first.is_some_and(|row| !stamp.base().contains(row))
                || last.is_some_and(|row| !stamp.base().contains(row)))
        {
            return Err(QueryError::InvalidBinding);
        }
        if continuation.is_some_and(|value| value.stamp() != stamp) {
            return Err(QueryError::InvalidBinding);
        }
        Ok(Self {
            stamp,
            range,
            continuation,
            limits,
        })
    }
    pub const fn stamp(self) -> QueryStamp {
        self.stamp
    }
    pub const fn range(self) -> ContentRange {
        self.range
    }
    pub const fn continuation(self) -> Option<ContentContinuation> {
        self.continuation
    }
    pub const fn limits(self) -> PageLimits {
        self.limits
    }
}

/// Validated semantic page with the request stamp echoed unchanged.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ContentPageReply {
    stamp: QueryStamp,
    page: ContentPage,
}
impl ContentPageReply {
    pub fn new(request: &ContentPageRequest, page: ContentPage) -> Result<Self, QueryError> {
        let parts = page.parts();
        let cells = parts
            .rows
            .iter()
            .try_fold(0usize, |n, row| n.checked_add(row.cells().len()))
            .ok_or(QueryError::Exhausted)?;
        if parts.base != request.stamp().base() {
            return Err(QueryError::InvalidBinding);
        }
        if parts.rows.len() > usize::from(request.limits().rows())
            || cells as u64 > u64::from(request.limits().cells())
            || page.accounted_bytes() as u64 > u64::from(request.limits().bytes())
        {
            return Err(QueryError::Limit);
        }
        Ok(Self {
            stamp: request.stamp(),
            page,
        })
    }
    pub const fn stamp(&self) -> QueryStamp {
        self.stamp
    }
    pub fn page(&self) -> &ContentPage {
        &self.page
    }
    pub fn continuation(&self) -> Result<Option<ContentContinuation>, QueryError> {
        self.page
            .parts()
            .continuation
            .map(|value| ContentContinuation::new(self.stamp, value))
            .transpose()
    }
}

/// Fixed-size physical row facts for incremental topology reads.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TopologyRow {
    row: RowId,
    revision: u64,
    columns: u16,
    soft_wrapped: bool,
}
impl TopologyRow {
    pub fn new(
        row: RowId,
        revision: u64,
        columns: u16,
        soft_wrapped: bool,
    ) -> Result<Self, QueryError> {
        if columns == 0 || u32::from(columns) > SUPPORTED_GRID_LIMITS.max_columns() {
            return Err(QueryError::InvalidRange);
        }
        Ok(Self {
            row,
            revision,
            columns,
            soft_wrapped,
        })
    }
    pub const fn row(self) -> RowId {
        self.row
    }
    pub const fn revision(self) -> u64 {
        self.revision
    }
    pub const fn columns(self) -> u16 {
        self.columns
    }
    pub const fn soft_wrapped(self) -> bool {
        self.soft_wrapped
    }
}

/// Resume after this exact topology row under the unchanged request stamp.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TopologyContinuation {
    stamp: QueryStamp,
    after: TopologyRow,
}
impl TopologyContinuation {
    pub fn new(stamp: QueryStamp, after: TopologyRow) -> Result<Self, QueryError> {
        if !stamp.base().contains(after.row()) {
            return Err(QueryError::InvalidBinding);
        }
        Ok(Self { stamp, after })
    }
    pub const fn stamp(self) -> QueryStamp {
        self.stamp
    }
    pub const fn after(self) -> TopologyRow {
        self.after
    }
}

/// Topology pagination does not allocate a complete replacement topology vector.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TopologyPageRequest {
    stamp: QueryStamp,
    continuation: Option<TopologyContinuation>,
    limits: PageLimits,
}
impl TopologyPageRequest {
    pub fn new(
        stamp: QueryStamp,
        continuation: Option<TopologyContinuation>,
        limits: PageLimits,
    ) -> Result<Self, QueryError> {
        if continuation.is_some_and(|value| value.stamp() != stamp) {
            return Err(QueryError::InvalidBinding);
        }
        Ok(Self {
            stamp,
            continuation,
            limits,
        })
    }
    pub const fn stamp(self) -> QueryStamp {
        self.stamp
    }
    pub const fn continuation(self) -> Option<TopologyContinuation> {
        self.continuation
    }
    pub const fn limits(self) -> PageLimits {
        self.limits
    }
}

/// Bounded, duplicate-free topology fragment; terminal order is producer-owned.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TopologyPage {
    stamp: QueryStamp,
    rows: Vec<TopologyRow>,
    continuation: Option<TopologyContinuation>,
}
impl TopologyPage {
    pub fn new(
        request: &TopologyPageRequest,
        rows: Vec<TopologyRow>,
        continuation: Option<TopologyContinuation>,
    ) -> Result<Self, QueryError> {
        let bytes = rows
            .len()
            .checked_mul(64)
            .and_then(|n| n.checked_add(4096))
            .ok_or(QueryError::Exhausted)?;
        if rows.len() > usize::from(request.limits().rows())
            || bytes as u64 > u64::from(request.limits().bytes())
        {
            return Err(QueryError::Limit);
        }
        let mut seen = BTreeSet::new();
        for row in &rows {
            if !request.stamp().base().contains(row.row()) || !seen.insert(row.row()) {
                return Err(QueryError::InvalidBinding);
            }
        }
        if let Some(value) = continuation
            && (value.stamp() != request.stamp() || rows.last().copied() != Some(value.after()))
        {
            return Err(QueryError::InvalidBinding);
        }
        Ok(Self {
            stamp: request.stamp(),
            rows,
            continuation,
        })
    }
    pub const fn stamp(&self) -> QueryStamp {
        self.stamp
    }
    pub fn rows(&self) -> &[TopologyRow] {
        &self.rows
    }
    pub const fn continuation(&self) -> Option<TopologyContinuation> {
        self.continuation
    }
}

/// Explicit inclusive endpoints. The producer validates row existence/revisions,
/// topology order and complete wide/grapheme endpoints before formatting.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ContentSelection {
    start: CellRef,
    end: CellRef,
    rectangle: bool,
}
impl ContentSelection {
    pub fn new(
        stamp: QueryStamp,
        start: CellRef,
        end: CellRef,
        rectangle: bool,
    ) -> Result<Self, QueryError> {
        check_cell(stamp, start)?;
        check_cell(stamp, end)?;
        Ok(Self {
            start,
            end,
            rectangle,
        })
    }
    pub const fn start(self) -> CellRef {
        self.start
    }
    pub const fn end(self) -> CellRef {
        self.end
    }
    pub const fn rectangle(self) -> bool {
        self.rectangle
    }
}

/// Canonical server formatter choices. Legacy SelectionCopyOptions is adapted.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CopyOptions {
    pub unwrap_soft_wraps: bool,
    pub trim_trailing_spaces: bool,
    pub include_html: bool,
}

/// Does not mutate an emulator selection or personal viewport.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CopyRequest {
    stamp: QueryStamp,
    selection: ContentSelection,
    options: CopyOptions,
}
impl CopyRequest {
    pub fn new(
        stamp: QueryStamp,
        selection: ContentSelection,
        options: CopyOptions,
    ) -> Result<Self, QueryError> {
        check_cell(stamp, selection.start())?;
        check_cell(stamp, selection.end())?;
        Ok(Self {
            stamp,
            selection,
            options,
        })
    }
    pub const fn stamp(self) -> QueryStamp {
        self.stamp
    }
    pub const fn selection(self) -> ContentSelection {
        self.selection
    }
    pub const fn options(self) -> CopyOptions {
        self.options
    }
}

/// Logical full-selection bounds, not resident-cache or allocation claims.
/// Includes supported history plus active rows at the core maximum width,
/// complete bounded graphemes, worst-case sixfold escaping, per-cell styles and
/// hyperlinks, row markup and fixed document overhead. Never use paste/cache
/// ceilings here. Oversized source descriptors fail explicitly, not by shortening.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CopyLogicalLimits {
    plain: u64,
    html: u64,
}
impl CopyLogicalLimits {
    pub fn supported() -> Result<Self, QueryError> {
        let rows = SUPPORTED_SCROLLBACK_ROWS
            .checked_add(u64::from(SUPPORTED_GRID_LIMITS.max_rows()))
            .ok_or(QueryError::Exhausted)?;
        let cells = rows
            .checked_mul(u64::from(SUPPORTED_GRID_LIMITS.max_columns()))
            .ok_or(QueryError::Exhausted)?;
        let plain = cells
            .checked_mul(MAX_GRAPHEME_BYTES as u64)
            .and_then(|n| n.checked_add(rows))
            .ok_or(QueryError::Exhausted)?;
        let per_cell = (MAX_GRAPHEME_BYTES as u64)
            .checked_mul(6)
            .and_then(|n| n.checked_add((MAX_HYPERLINK_BYTES as u64).checked_mul(6)?))
            .and_then(|n| n.checked_add(COPY_STYLE_MARKUP_BYTES_PER_CELL))
            .and_then(|n| n.checked_add(COPY_LINK_MARKUP_BYTES_PER_CELL))
            .ok_or(QueryError::Exhausted)?;
        let html = cells
            .checked_mul(per_cell)
            .and_then(|n| n.checked_add(rows.checked_mul(64)?))
            .and_then(|n| n.checked_add(COPY_DOCUMENT_MARKUP_BYTES))
            .ok_or(QueryError::Exhausted)?;
        Ok(Self { plain, html })
    }
    pub const fn plain(self) -> u64 {
        self.plain
    }
    pub const fn html(self) -> u64 {
        self.html
    }
}

/// Plain and HTML are separate ordered byte streams in the same transfer.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CopyPart {
    Plain,
    Html,
}

/// A complete UTF-8 fragment. Debug reports no text. Offsets and logical totals
/// use u64 so full rich-copy output is not constrained to one frame or allocation.
#[derive(Clone, Eq, PartialEq)]
pub struct CopyChunk {
    stamp: QueryStamp,
    transfer: ResourceTransferId,
    part: CopyPart,
    offset: u64,
    total: u64,
    bytes: Vec<u8>,
}
impl CopyChunk {
    pub fn new(
        request: &CopyRequest,
        transfer: ResourceTransferId,
        part: CopyPart,
        offset: u64,
        total: u64,
        bytes: Vec<u8>,
    ) -> Result<Self, QueryError> {
        if part == CopyPart::Html && !request.options().include_html {
            return Err(QueryError::InvalidBinding);
        }
        let limits = CopyLogicalLimits::supported()?;
        let maximum = match part {
            CopyPart::Plain => limits.plain(),
            CopyPart::Html => limits.html(),
        };
        if bytes.is_empty()
            || bytes.len() > MAX_RESOURCE_CHUNK_BYTES
            || total > maximum
            || offset
                .checked_add(bytes.len() as u64)
                .is_none_or(|end| end > total)
        {
            return Err(QueryError::Limit);
        }
        std::str::from_utf8(&bytes).map_err(|_| QueryError::InvalidEncoding)?;
        Ok(Self {
            stamp: request.stamp(),
            transfer,
            part,
            offset,
            total,
            bytes,
        })
    }
    pub const fn stamp(&self) -> QueryStamp {
        self.stamp
    }
    pub const fn transfer(&self) -> ResourceTransferId {
        self.transfer
    }
    pub const fn part(&self) -> CopyPart {
        self.part
    }
    pub const fn offset(&self) -> u64 {
        self.offset
    }
    pub const fn total(&self) -> u64 {
        self.total
    }
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }
}
impl fmt::Debug for CopyChunk {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CopyChunk")
            .field("stamp", &self.stamp)
            .field("transfer", &self.transfer)
            .field("part", &self.part)
            .field("offset", &self.offset)
            .field("total", &self.total)
            .field("bytes", &"[redacted]")
            .finish()
    }
}

/// Terminal completion only. Clients require contiguous complete chunks and
/// verify SHA-256 of each exact UTF-8 byte stream before exposing Copy/Services.
/// Aborted/partial transfers never construct an eligible prepared result.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CopyCompletion {
    stamp: QueryStamp,
    transfer: ResourceTransferId,
    plain_bytes: u64,
    plain_digest: [u8; 32],
    html: Option<(u64, [u8; 32])>,
}
impl CopyCompletion {
    pub fn new(
        request: &CopyRequest,
        transfer: ResourceTransferId,
        plain_bytes: u64,
        plain_digest: [u8; 32],
        html: Option<(u64, [u8; 32])>,
    ) -> Result<Self, QueryError> {
        let limits = CopyLogicalLimits::supported()?;
        if html.is_some() != request.options().include_html {
            return Err(QueryError::InvalidBinding);
        }
        if plain_bytes > limits.plain() || html.is_some_and(|(bytes, _)| bytes > limits.html()) {
            return Err(QueryError::Limit);
        }
        Ok(Self {
            stamp: request.stamp(),
            transfer,
            plain_bytes,
            plain_digest,
            html,
        })
    }
    pub const fn stamp(self) -> QueryStamp {
        self.stamp
    }
    pub const fn transfer(self) -> ResourceTransferId {
        self.transfer
    }
    pub const fn plain_bytes(self) -> u64 {
        self.plain_bytes
    }
    pub const fn plain_digest(self) -> [u8; 32] {
        self.plain_digest
    }
    pub const fn html(self) -> Option<(u64, [u8; 32])> {
        self.html
    }
}

/// Ordered Copy stream item. Failure is explicit; absence of Complete is not success.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CopyStreamItem {
    Chunk(CopyChunk),
    Complete(CopyCompletion),
    Failed {
        stamp: QueryStamp,
        reason: ContentReadFailure,
    },
}

/// Literal Find query. Empty/newline-containing values remain representable and
/// produce no matches, preserving existing matching semantics. Debug is redacted.
#[derive(Clone, Eq, PartialEq)]
pub struct BoundedFindQuery(String);
impl BoundedFindQuery {
    pub fn new(value: String) -> Result<Self, QueryError> {
        if value.len() > MAX_FIND_QUERY_BYTES {
            return Err(QueryError::Limit);
        }
        Ok(Self(value))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
    pub fn fingerprint(&self) -> [u8; 32] {
        let mut hash = Sha256::new();
        hash.update(b"spaceterm-find-literal-ascii-fold-v1\0");
        hash.update(self.0.as_bytes());
        hash.finalize().into()
    }
}
impl fmt::Debug for BoundedFindQuery {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("BoundedFindQuery([redacted])")
    }
}

/// Next candidate grapheme head, not a persistent Find cursor. Producer resumes
/// with bounded preceding context and preserves literal non-overlapping semantics.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FindContinuation {
    stamp: QueryStamp,
    query: [u8; 32],
    next: CellRef,
}
impl FindContinuation {
    pub fn new(
        stamp: QueryStamp,
        query: &BoundedFindQuery,
        next: CellRef,
    ) -> Result<Self, QueryError> {
        check_cell(stamp, next)?;
        Ok(Self {
            stamp,
            query: query.fingerprint(),
            next,
        })
    }
    pub const fn stamp(self) -> QueryStamp {
        self.stamp
    }
    pub const fn query_fingerprint(self) -> [u8; 32] {
        self.query
    }
    pub const fn next(self) -> CellRef {
        self.next
    }
}

/// Ephemeral connection-scoped Find scan. No server query/navigation state survives it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FindRequest {
    stamp: QueryStamp,
    query: BoundedFindQuery,
    continuation: Option<FindContinuation>,
}
impl FindRequest {
    pub fn new(
        stamp: QueryStamp,
        query: BoundedFindQuery,
        continuation: Option<FindContinuation>,
    ) -> Result<Self, QueryError> {
        if continuation.is_some_and(|value| {
            value.stamp() != stamp || value.query_fingerprint() != query.fingerprint()
        }) {
            return Err(QueryError::InvalidBinding);
        }
        Ok(Self {
            stamp,
            query,
            continuation,
        })
    }
    pub const fn stamp(&self) -> QueryStamp {
        self.stamp
    }
    pub fn query(&self) -> &BoundedFindQuery {
        &self.query
    }
    pub const fn continuation(&self) -> Option<FindContinuation> {
        self.continuation
    }
}

/// Inclusive complete-grapheme match endpoints; width is resolved by the producer.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FindMatch {
    start: CellRef,
    end: CellRef,
}
impl FindMatch {
    pub fn new(stamp: QueryStamp, start: CellRef, end: CellRef) -> Result<Self, QueryError> {
        check_cell(stamp, start)?;
        check_cell(stamp, end)?;
        Ok(Self { start, end })
    }
    pub const fn start(self) -> CellRef {
        self.start
    }
    pub const fn end(self) -> CellRef {
        self.end
    }
}

/// Bounded match chunk. Index is stream order, not a total-match admission ceiling.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FindChunk {
    stamp: QueryStamp,
    index: u64,
    matches: Vec<FindMatch>,
    continuation: Option<FindContinuation>,
}
impl FindChunk {
    pub fn new(
        request: &FindRequest,
        index: u64,
        matches: Vec<FindMatch>,
        continuation: Option<FindContinuation>,
    ) -> Result<Self, QueryError> {
        let bytes = matches
            .len()
            .checked_mul(128)
            .and_then(|n| n.checked_add(4096))
            .ok_or(QueryError::Exhausted)?;
        if matches.len() > MAX_FIND_CHUNK_MATCHES || bytes > MAX_FIND_CHUNK_BYTES {
            return Err(QueryError::Limit);
        }
        for value in &matches {
            check_cell(request.stamp(), value.start())?;
            check_cell(request.stamp(), value.end())?;
        }
        if continuation.is_some_and(|value| {
            value.stamp() != request.stamp()
                || value.query_fingerprint() != request.query().fingerprint()
        }) {
            return Err(QueryError::InvalidBinding);
        }
        Ok(Self {
            stamp: request.stamp(),
            index,
            matches,
            continuation,
        })
    }
    pub const fn stamp(&self) -> QueryStamp {
        self.stamp
    }
    pub const fn index(&self) -> u64 {
        self.index
    }
    pub fn matches(&self) -> &[FindMatch] {
        &self.matches
    }
    pub const fn continuation(&self) -> Option<FindContinuation> {
        self.continuation
    }
}

/// Typed Find termination; loss of a stream is never complete matching evidence.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FindStreamItem {
    Chunk(Box<FindChunk>),
    Complete {
        stamp: QueryStamp,
        chunks: u64,
    },
    Failed {
        stamp: QueryStamp,
        reason: ContentReadFailure,
    },
}

fn check_resource(stamp: QueryStamp, resource: GraphicsResourceRef) -> Result<(), QueryError> {
    if resource.key.session != stamp.base().session {
        return Err(QueryError::InvalidBinding);
    }
    if resource.width == 0
        || resource.height == 0
        || resource.rgba_bytes > MAX_GRAPHICS_RESOURCE_BYTES
        || resource
            .width
            .checked_mul(resource.height)
            .and_then(|n| n.checked_mul(4))
            != Some(resource.rgba_bytes)
    {
        return Err(QueryError::InvalidRange);
    }
    Ok(())
}

/// Immutable Session-qualified RGBA8 read, independently admitted from decoded residency.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GraphicsRequest {
    stamp: QueryStamp,
    transfer: ResourceTransferId,
    resource: GraphicsResourceRef,
    offset: u32,
}
impl GraphicsRequest {
    pub fn new(
        stamp: QueryStamp,
        transfer: ResourceTransferId,
        resource: GraphicsResourceRef,
        offset: u32,
    ) -> Result<Self, QueryError> {
        check_resource(stamp, resource)?;
        if offset >= resource.rgba_bytes {
            return Err(QueryError::InvalidRange);
        }
        Ok(Self {
            stamp,
            transfer,
            resource,
            offset,
        })
    }
    pub const fn stamp(self) -> QueryStamp {
        self.stamp
    }
    pub const fn transfer(self) -> ResourceTransferId {
        self.transfer
    }
    pub const fn resource(self) -> GraphicsResourceRef {
        self.resource
    }
    pub const fn offset(self) -> u32 {
        self.offset
    }
}

/// Bounded owned wire fragment, never a whole-image clone per page. Its immutable
/// asset lease remains producer-owned until transfer release. Debug is redacted.
#[derive(Clone, Eq, PartialEq)]
pub struct GraphicsChunk {
    request: GraphicsRequest,
    offset: u32,
    bytes: Vec<u8>,
    complete: bool,
}
impl GraphicsChunk {
    pub fn new(
        request: GraphicsRequest,
        offset: u32,
        bytes: Vec<u8>,
        complete: bool,
    ) -> Result<Self, QueryError> {
        if bytes.is_empty() || bytes.len() > MAX_RESOURCE_CHUNK_BYTES || offset < request.offset() {
            return Err(QueryError::Limit);
        }
        let count = u32::try_from(bytes.len()).map_err(|_| QueryError::Limit)?;
        let end = offset.checked_add(count).ok_or(QueryError::Exhausted)?;
        if end > request.resource().rgba_bytes || complete != (end == request.resource().rgba_bytes)
        {
            return Err(QueryError::InvalidRange);
        }
        Ok(Self {
            request,
            offset,
            bytes,
            complete,
        })
    }
    pub const fn request(&self) -> GraphicsRequest {
        self.request
    }
    pub const fn offset(&self) -> u32 {
        self.offset
    }
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }
    pub const fn complete(&self) -> bool {
        self.complete
    }
}
impl fmt::Debug for GraphicsChunk {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("GraphicsChunk")
            .field("request", &self.request)
            .field("offset", &self.offset)
            .field("bytes", &"[redacted]")
            .field("complete", &self.complete)
            .finish()
    }
}

/// Stale reads and missing assets are request-local, not implicit runtime death.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ContentReadFailure {
    StaleAuthority,
    Cancelled,
    Retired,
    Capacity,
    MissingResource,
    InvalidRange,
    Exhausted,
}

/// Revision mismatch returns bounded current facts, never a mixed-revision page.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ContentReadResult<T> {
    Ready(T),
    ResyncRequired {
        request: QueryStamp,
        current: ContentBase,
    },
    Rejected {
        request: QueryStamp,
        reason: ContentReadFailure,
    },
}
pub type ContentPageResult = ContentReadResult<ContentPageReply>;
pub type TopologyPageResult = ContentReadResult<TopologyPage>;
/// Accessibility uses exactly these stable semantic rows/cursor/modes/hidden-input
/// facts. Native accessibility documents and selections remain client-local.
pub type AccessibilityPageRequest = ContentPageRequest;
pub type AccessibilityPageResult = ContentPageResult;
