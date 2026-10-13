//! Bounded styled content copied from preserved native pages.
//! No read changes compression, selection, Find or the authoritative viewport.

pub use crate::accessibility::{RowId, Screen};
use crate::{
    Terminal,
    error::{Error, Result, from_result},
    ffi,
    style::Style,
};
use std::{fmt, ptr::NonNull};

/// Current engine comparison facts, independent of render dirtiness.
#[derive(Clone, Copy, Debug)]
pub struct Snapshot {
    /// Engine input mutation sequence.
    pub source_revision: u64,
    /// Physical topology fingerprint.
    pub topology_revision: u64,
    /// Active screen.
    pub screen: Screen,
    /// Active buffer generation.
    pub screen_generation: u64,
    /// Number of retained physical rows.
    pub total_rows: u32,
    /// First row of the active grid, not the viewport.
    pub active_start: u32,
    /// Physical row width.
    pub columns: u16,
    /// Content is inside a synchronized transaction.
    pub synchronized: bool,
    /// Stable cursor row.
    pub cursor: RowId,
    /// Cursor column.
    pub cursor_column: u16,
    /// Block, underline or bar, 0..=2.
    pub cursor_shape: u8,
}
/// Bounded scan position and output limits.
#[derive(Clone, Copy, Debug)]
pub struct Options {
    /// Document row offset.
    pub row: u32,
    /// First column of a partial row.
    pub column: u16,
    /// Exclusive final row, or zero for the whole document.
    pub end_row: u32,
    /// Emitted row limit.
    pub max_rows: u16,
    /// Emitted cell limit.
    pub max_cells: u32,
    /// Conservative semantic byte limit.
    pub max_bytes: usize,
    /// Seek to a stable identity instead of a document offset.
    pub seek: Option<(RowId, bool)>,
}
impl Default for Options {
    fn default() -> Self {
        Self {
            row: 0,
            column: 0,
            end_row: 0,
            max_rows: 256,
            max_cells: 16_384,
            max_bytes: 1024 * 1024,
            seek: None,
        }
    }
}
impl Options {
    fn raw(self) -> Result<ffi::ContentOptions> {
        if self.max_rows == 0
            || self.max_rows > 256
            || self.max_cells == 0
            || self.max_cells > 16_384
            || self.max_bytes < 4096
            || self.max_bytes > 1024 * 1024
        {
            return Err(Error::InvalidValue);
        }
        Ok(ffi::ContentOptions {
            size: std::mem::size_of::<ffi::ContentOptions>(),
            row: self.row,
            column: self.column,
            end_row: self.end_row,
            max_rows: self.max_rows,
            max_cells: self.max_cells,
            max_bytes: self.max_bytes,
            seek: self.seek.is_some(),
            seek_after: self.seek.is_some_and(|(_, after)| after),
            seek_row: self
                .seek
                .map_or_else(Default::default, |(row, _)| row.into()),
        })
    }
}
/// Complete grapheme or wide-cell spacer. Debug omits terminal text and links.
#[derive(Clone)]
pub struct Cell {
    /// Complete UTF-8 text; empty for a spacer.
    pub text: String,
    /// Native wide/spacer meaning, 0..=3.
    pub wide: u8,
    /// Output, input or prompt, 0..=2.
    pub semantic: u8,
    /// Distinguish a printed space from an empty or background-only cell.
    pub has_text: bool,
    /// Complete semantic style.
    pub style: Style,
    /// Untrusted hyperlink descriptor, without embedding userdata.
    pub hyperlink: Option<String>,
}
impl fmt::Debug for Cell {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Cell")
            .field("wide", &self.wide)
            .field("semantic", &self.semantic)
            .finish_non_exhaustive()
    }
}
/// One complete physical row or contiguous fragment.
#[derive(Clone, Debug)]
pub struct Row {
    /// Stable row identity.
    pub id: RowId,
    /// Conservative source revision, invalidated before render dirtiness is consumed.
    pub revision: u64,
    /// Position in retained document order.
    pub document_row: u32,
    /// Full physical width.
    pub columns: u16,
    /// Fragment start column.
    pub start_column: u16,
    /// Fragment cell count; topology reads omit cell payloads.
    pub emitted_columns: u16,
    /// Next row continues this logical line.
    pub soft_wrapped: bool,
    /// Owned cell payloads.
    pub cells: Vec<Cell>,
}
/// A bounded read and its next position.
#[derive(Debug)]
pub struct Page {
    /// Owned row fragments.
    pub rows: Vec<Row>,
    /// Next document row.
    pub next_row: u32,
    /// Next column, zero when advancing to another row.
    pub next_column: u16,
    /// Native cells inspected, independently bounded from emitted cells.
    pub inspected_cells: u32,
    /// Conservative semantic output bytes.
    pub accounted_bytes: usize,
    /// No more rows in the requested range.
    pub complete: bool,
}
/// Bounded progress preparing row-scoped graphics descriptors.
#[cfg(feature = "kitty-graphics")]
#[derive(Clone, Copy, Debug)]
pub struct PlacementWork {
    /// A cached scan advanced, even if its final quantum emitted no descriptor.
    pub progressed: bool,
    /// Placement metadata entries and placeholder cells inspected.
    pub inspected_cells: u32,
    /// Conservative work bytes, including placeholder and row inspection.
    pub accounted_bytes: usize,
}
/// Worker-local content reader. It retains no replacement topology vector.
#[derive(Debug)]
pub struct State {
    inner: NonNull<ffi::ContentState>,
}
impl State {
    /// Create an independent semantic observer.
    pub fn new() -> Result<Self> {
        let mut raw = std::ptr::null_mut();
        from_result(unsafe { ffi::ghostty_content_state_new(&raw mut raw) })?;
        Ok(Self {
            inner: NonNull::new(raw).ok_or(Error::InvalidValue)?,
        })
    }
    /// Read comparison facts without decoding history or consuming dirty state.
    pub fn capture(&self, terminal: &Terminal<'_, '_>) -> Result<Snapshot> {
        let mut out = ffi::ContentSnapshot {
            size: std::mem::size_of::<ffi::ContentSnapshot>(),
            ..Default::default()
        };
        from_result(unsafe {
            ffi::ghostty_content_state_capture(
                self.inner.as_ptr(),
                terminal.inner.as_raw(),
                &raw mut out,
            )
        })?;
        if !(0..=2).contains(&out.cursor_shape) {
            return Err(Error::InvalidValue);
        }
        Ok(Snapshot {
            source_revision: out.source_revision,
            topology_revision: out.topology_revision,
            screen: out.screen.try_into()?,
            screen_generation: out.screen_generation,
            total_rows: out.total_rows,
            active_start: out.active_start,
            columns: out.columns,
            synchronized: out.synchronized,
            cursor: out.cursor.try_into()?,
            cursor_column: out.cursor_column,
            cursor_shape: u8::try_from(out.cursor_shape).map_err(|_| Error::InvalidValue)?,
        })
    }
    /// Copy one bounded styled fragment. All callback pointers expire in this call.
    pub fn read_page(&self, terminal: &Terminal<'_, '_>, options: Options) -> Result<Page> {
        self.read(terminal, options, false)
    }
    /// Copy one bounded topology fragment, never assembling the full document.
    pub fn read_topology(&self, terminal: &Terminal<'_, '_>, options: Options) -> Result<Page> {
        self.read(terminal, options, true)
    }
    /// Advance one revision-exact placement scan without holding a whole
    /// document or borrowed preserved storage. Residency is eight bounded pages.
    #[cfg(feature = "kitty-graphics")]
    pub fn prepare_placements(&self, terminal: &Terminal<'_, '_>) -> Result<PlacementWork> {
        let mut progress = ffi::ContentProgress {
            size: std::mem::size_of::<ffi::ContentProgress>(),
            ..Default::default()
        };
        from_result(unsafe {
            ffi::ghostty_content_prepare_placements(
                self.inner.as_ptr(),
                terminal.inner.as_raw(),
                &raw mut progress,
            )
        })?;
        Ok(PlacementWork {
            progressed: progress.next_row != 0,
            inspected_cells: progress.inspected_cells,
            accounted_bytes: progress.accounted_bytes,
        })
    }
    /// Visit only descriptors intersecting a bounded document row interval.
    /// The native producer resolves tagged IDs, placeholders and relative chains
    /// against preserved storage. No viewport or native pointer escapes.
    #[cfg(feature = "kitty-graphics")]
    pub fn read_placements(
        &self,
        terminal: &Terminal<'_, '_>,
        first: u32,
        last: u32,
    ) -> Result<Vec<ffi::ContentPlacement>> {
        struct Context {
            values: Vec<ffi::ContentPlacement>,
            error: Option<Error>,
        }
        unsafe extern "C" fn callback(
            userdata: *mut std::ffi::c_void,
            value: *const ffi::ContentPlacement,
        ) -> bool {
            let context = unsafe { &mut *userdata.cast::<Context>() };
            let value = unsafe { &*value };
            if value.size < std::mem::size_of::<ffi::ContentPlacement>()
                || context.values.len() >= 1024
            {
                context.error = Some(Error::OutOfMemory);
                return false;
            }
            if context.values.try_reserve(1).is_err() {
                context.error = Some(Error::OutOfMemory);
                return false;
            }
            context.values.push(*value);
            true
        }
        let options = Options {
            row: first,
            end_row: last,
            ..Default::default()
        }
        .raw()?;
        let mut context = Context {
            values: Vec::new(),
            error: None,
        };
        let result = unsafe {
            ffi::ghostty_content_read_placements(
                self.inner.as_ptr(),
                terminal.inner.as_raw(),
                &raw const options,
                std::ptr::from_mut(&mut context).cast(),
                Some(callback),
            )
        };
        if let Some(error) = context.error {
            return Err(error);
        }
        from_result(result)?;
        Ok(context.values)
    }
    fn read(&self, terminal: &Terminal<'_, '_>, options: Options, topology: bool) -> Result<Page> {
        let options = options.raw()?;
        let mut out = ffi::ContentProgress {
            size: std::mem::size_of::<ffi::ContentProgress>(),
            ..Default::default()
        };
        let mut context = Context::default();
        let userdata = std::ptr::from_mut(&mut context).cast();
        // SAFETY: Both handles and the callback context outlive this synchronous
        // read. Callbacks copy borrowed preserved-page data before returning.
        let result = unsafe {
            if topology {
                ffi::ghostty_content_read_topology(
                    self.inner.as_ptr(),
                    terminal.inner.as_raw(),
                    &raw const options,
                    userdata,
                    Some(row_callback),
                    &raw mut out,
                )
            } else {
                ffi::ghostty_content_read_page(
                    self.inner.as_ptr(),
                    terminal.inner.as_raw(),
                    &raw const options,
                    userdata,
                    Some(row_callback),
                    Some(cell_callback),
                    &raw mut out,
                )
            }
        };
        if let Some(error) = context.error {
            return Err(error);
        }
        from_result(result)?;
        Ok(Page {
            rows: context.rows,
            next_row: out.next_row,
            next_column: out.next_column,
            inspected_cells: out.inspected_cells,
            accounted_bytes: out.accounted_bytes,
            complete: out.complete,
        })
    }
}
impl Drop for State {
    fn drop(&mut self) {
        unsafe { ffi::ghostty_content_state_free(self.inner.as_ptr()) };
    }
}
#[derive(Default)]
struct Context {
    rows: Vec<Row>,
    error: Option<Error>,
}
unsafe extern "C" fn row_callback(
    userdata: *mut std::ffi::c_void,
    raw: *const ffi::ContentRow,
) -> bool {
    let context = unsafe { &mut *userdata.cast::<Context>() };
    let result = (|| {
        let raw = unsafe { &*raw };
        if raw.size < std::mem::size_of::<ffi::ContentRow>()
            || raw.columns == 0
            || raw.emitted_columns == 0
        {
            return Err(Error::InvalidValue);
        }
        context
            .rows
            .try_reserve(1)
            .map_err(|_| Error::OutOfMemory)?;
        context.rows.push(Row {
            id: raw.id.try_into()?,
            revision: raw.revision,
            document_row: raw.document_row,
            columns: raw.columns,
            start_column: raw.start_column,
            emitted_columns: raw.emitted_columns,
            soft_wrapped: raw.soft_wrapped,
            cells: Vec::new(),
        });
        Ok(())
    })();
    if let Err(error) = result {
        context.error = Some(error);
        return false;
    }
    true
}
unsafe extern "C" fn cell_callback(
    userdata: *mut std::ffi::c_void,
    raw: *const ffi::ContentCell,
) -> bool {
    let context = unsafe { &mut *userdata.cast::<Context>() };
    let result = (|| {
        let raw = unsafe { &*raw };
        if raw.size < std::mem::size_of::<ffi::ContentCell>()
            || raw.extra_codepoints_len > 1023
            || raw.hyperlink_len > 4096
            || raw.wide > 3
            || raw.semantic < 0
            || raw.semantic > 2
        {
            return Err(Error::InvalidValue);
        }
        let mut text = String::new();
        text.try_reserve(4 * (1 + raw.extra_codepoints_len))
            .map_err(|_| Error::OutOfMemory)?;
        if raw.wide < 2 {
            text.push(char::from_u32(raw.codepoint).ok_or(Error::InvalidValue)?);
            if raw.extra_codepoints_len != 0 {
                if raw.extra_codepoints.is_null() {
                    return Err(Error::InvalidValue);
                }
                for &value in unsafe {
                    std::slice::from_raw_parts(raw.extra_codepoints, raw.extra_codepoints_len)
                } {
                    text.push(char::from_u32(value).ok_or(Error::InvalidValue)?);
                }
            }
        }
        let hyperlink = if raw.hyperlink_len == 0 {
            None
        } else {
            if raw.hyperlink.is_null() {
                return Err(Error::InvalidValue);
            }
            let bytes = unsafe { std::slice::from_raw_parts(raw.hyperlink, raw.hyperlink_len) };
            let value = std::str::from_utf8(bytes).map_err(|_| Error::InvalidValue)?;
            let mut owned = String::new();
            owned
                .try_reserve(value.len())
                .map_err(|_| Error::OutOfMemory)?;
            owned.push_str(value);
            Some(owned)
        };
        let row = context.rows.last_mut().ok_or(Error::InvalidValue)?;
        row.cells.try_reserve(1).map_err(|_| Error::OutOfMemory)?;
        row.cells.push(Cell {
            text,
            wide: u8::try_from(raw.wide).map_err(|_| Error::InvalidValue)?,
            semantic: u8::try_from(raw.semantic).map_err(|_| Error::InvalidValue)?,
            has_text: raw.has_text,
            style: raw.style.try_into()?,
            hyperlink,
        });
        Ok(())
    })();
    if let Err(error) = result {
        context.error = Some(error);
        return false;
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(feature = "kitty-graphics")]
    #[test]
    fn historical_placement_inspection_yields_before_finishing_the_document() {
        let mut terminal = Terminal::new(crate::TerminalOptions {
            cols: 8,
            rows: 3,
            max_scrollback: 10_000,
            max_scrollback_bytes: 50_000_000,
        })
        .unwrap();
        terminal.resize(8, 3, 10, 20).unwrap();
        terminal.vt_write(b"\x1b_Ga=T,t=d,f=32,i=1,s=1,v=1;AQIDBA==\x1b\\\r\n");
        for _ in 0..1025 {
            terminal.vt_write(b"\x1b_Ga=p,i=1\x1b\\\r\n");
        }
        terminal.vt_write(b"\r\n\r\n\r\n");
        let reader = State::new().unwrap();
        let snapshot = reader.capture(&terminal).unwrap();
        assert!(matches!(
            reader.read_placements(&terminal, snapshot.active_start, snapshot.total_rows),
            Err(Error::OutOfSpace { .. })
        ));
        let mut quanta = 0;
        loop {
            let work = reader.prepare_placements(&terminal).unwrap();
            assert!(work.inspected_cells <= 16_384);
            assert!(work.accounted_bytes <= 256 * 1024);
            if !work.progressed {
                break;
            }
            quanta += 1;
            assert!(quanta < 32);
        }
        assert!(quanta > 1);
        assert!(
            reader
                .read_placements(&terminal, snapshot.active_start, snapshot.total_rows)
                .unwrap()
                .is_empty()
        );
    }
    #[test]
    fn maximum_width_linked_rows_charge_only_inspected_fragment_work() {
        let mut terminal = Terminal::new(crate::TerminalOptions {
            cols: 1024,
            rows: 1,
            max_scrollback: 100,
            max_scrollback_bytes: 50_000_000,
        })
        .unwrap();
        // OSC's 2 KiB parser bound is lower than the semantic 4 KiB ceiling.
        let link = format!("https://example.invalid/{}", "x".repeat(2000));
        terminal.vt_write(format!("\x1b]8;;{link}\x1b\\{}", "a".repeat(1024)).as_bytes());
        let reader = State::new().unwrap();
        for column in [0, 1, 1023] {
            let page = reader
                .read_page(
                    &terminal,
                    Options {
                        column,
                        max_rows: 1,
                        max_cells: 1,
                        max_bytes: 32 * 1024,
                        ..Default::default()
                    },
                )
                .unwrap();
            assert_eq!(page.rows[0].cells.len(), 1);
            assert!(page.inspected_cells <= 1);
            assert!(page.accounted_bytes <= 32 * 1024);
            assert_eq!(
                page.rows[0].cells[0].hyperlink.as_deref(),
                Some(link.as_str())
            );
        }
    }
}
