//! Revision-exact, nonmutating styled pages and physical topology.
use libghostty_vt::{
    Terminal, content as vt,
    style::{StyleColor, Underline as VtUnderline},
    terminal::Mode,
};
use spaceterm_core::{
    authority::{ContentRevision, SessionKey},
    content::*,
    content_queries::*,
};

pub(crate) struct SemanticState {
    pub reader: vt::State,
    #[cfg(feature = "kitty-graphics")]
    pub graphics: crate::content::graphics::GraphicsCatalog,
    pub session: Option<SessionKey>,
    pub revision: ContentRevision,
    exhausted: bool,
    pub hidden_input: bool,
    pub observation_tick: Option<spaceterm_core::observations::MonotonicTick>,
    pub confirmed_directory: Option<spaceterm_core::observations::ConfirmedDirectoryObservation>,
    pub observation_sequence: u64,
    pub observation_failure: Option<ContentReadFailure>,
}
impl SemanticState {
    pub fn new() -> Result<Self, libghostty_vt::Error> {
        Ok(Self {
            reader: vt::State::new()?,
            #[cfg(feature = "kitty-graphics")]
            graphics: Default::default(),
            session: None,
            revision: ContentRevision::new(1),
            exhausted: false,
            hidden_input: false,
            observation_tick: None,
            confirmed_directory: None,
            observation_sequence: 0,
            observation_failure: None,
        })
    }
    pub fn advance(&mut self) -> Result<(), ContentReadFailure> {
        if self.session.is_some() {
            self.revision = self.revision.checked_successor().ok_or_else(|| {
                self.exhausted = true;
                ContentReadFailure::Exhausted
            })?;
        }
        Ok(())
    }
    pub fn base(&self, terminal: &Terminal<'_, '_>) -> Result<ContentBase, ContentReadFailure> {
        if self.exhausted {
            return Err(ContentReadFailure::Exhausted);
        }
        let session = self.session.ok_or(ContentReadFailure::InvalidRange)?;
        let snapshot = self.reader.capture(terminal).map_err(native_failure)?;
        Ok(ContentBase {
            session,
            revision: self.revision,
            screen: screen(snapshot.screen),
            screen_generation: snapshot.screen_generation,
            topology_revision: snapshot.topology_revision,
        })
    }
    pub fn check(
        &self,
        terminal: &Terminal<'_, '_>,
        stamp: QueryStamp,
    ) -> Result<(), ContentReadFailure> {
        if self.base(terminal)? != stamp.base() {
            return Err(ContentReadFailure::StaleAuthority);
        }
        if self
            .reader
            .capture(terminal)
            .map_err(native_failure)?
            .synchronized
        {
            return Err(ContentReadFailure::Cancelled);
        }
        Ok(())
    }
    pub fn resolve(
        &self,
        terminal: &Terminal<'_, '_>,
        id: RowId,
    ) -> Result<vt::Row, ContentReadFailure> {
        let page = self
            .reader
            .read_topology(
                terminal,
                vt::Options {
                    seek: Some((native_row(id), false)),
                    max_rows: 1,
                    ..Default::default()
                },
            )
            .map_err(native_failure)?;
        page.rows
            .into_iter()
            .next()
            .ok_or(ContentReadFailure::InvalidRange)
    }
    pub fn resolve_cell(
        &self,
        terminal: &Terminal<'_, '_>,
        cell: CellRef,
    ) -> Result<vt::Row, ContentReadFailure> {
        if Some(cell.session) != self.session {
            return Err(ContentReadFailure::InvalidRange);
        }
        let row = self.resolve(terminal, cell.row)?;
        if row.revision != cell.row_revision || cell.column >= row.columns {
            return Err(ContentReadFailure::InvalidRange);
        }
        Ok(row)
    }
}
pub(crate) fn native_failure(error: libghostty_vt::Error) -> ContentReadFailure {
    match error {
        libghostty_vt::Error::OutOfMemory | libghostty_vt::Error::OutOfSpace { .. } => {
            ContentReadFailure::Capacity
        }
        _ => ContentReadFailure::InvalidRange,
    }
}
pub(crate) fn screen(value: vt::Screen) -> Screen {
    match value {
        vt::Screen::Primary => Screen::Primary,
        vt::Screen::Alternate => Screen::Alternate,
    }
}
pub(crate) fn row_id(value: vt::RowId) -> RowId {
    RowId {
        screen: screen(value.screen),
        screen_generation: value.screen_generation,
        node_serial: value.node_serial,
        page_row: value.page_row,
    }
}
pub(crate) fn native_row(value: RowId) -> vt::RowId {
    vt::RowId {
        screen: match value.screen {
            Screen::Primary => vt::Screen::Primary,
            Screen::Alternate => vt::Screen::Alternate,
        },
        screen_generation: value.screen_generation,
        node_serial: value.node_serial,
        page_row: value.page_row,
    }
}
fn color(value: StyleColor) -> TerminalColor {
    match value {
        StyleColor::None => TerminalColor::Default,
        StyleColor::Palette(index) => TerminalColor::Palette(index.0),
        StyleColor::Rgb(value) => TerminalColor::Rgb(Rgb {
            r: value.r,
            g: value.g,
            b: value.b,
        }),
    }
}
pub(crate) fn style(value: &vt::Cell) -> CellStyle {
    let native = value.style;
    CellStyle {
        foreground: color(native.fg_color),
        background: color(native.bg_color),
        underline_color: color(native.underline_color),
        underline: match native.underline {
            VtUnderline::None => Underline::None,
            VtUnderline::Single => Underline::Single,
            VtUnderline::Double => Underline::Double,
            VtUnderline::Curly => Underline::Curly,
            VtUnderline::Dotted => Underline::Dotted,
            VtUnderline::Dashed => Underline::Dashed,
            _ => Underline::None,
        },
        bold: native.bold,
        faint: native.faint,
        italic: native.italic,
        blinking: native.blink,
        inverse: native.inverse,
        invisible: native.invisible,
        strikethrough: native.strikethrough,
        overline: native.overline,
        semantic: match value.semantic {
            1 => SemanticContent::Input,
            2 => SemanticContent::Prompt,
            _ => SemanticContent::Output,
        },
    }
}
fn convert_row(
    row: vt::Row,
    links: &mut Vec<HyperlinkDescriptor>,
) -> Result<ContentRow, ContentReadFailure> {
    let mut cells = Vec::new();
    cells
        .try_reserve(row.cells.len())
        .map_err(|_| ContentReadFailure::Capacity)?;
    for cell in row.cells {
        let cell_style = style(&cell);
        let content = match cell.wide {
            0 => CellContent::grapheme(cell.text, CellWidth::Single),
            1 => CellContent::grapheme(cell.text, CellWidth::Wide),
            2 => Ok(CellContent::spacer_tail()),
            3 => Ok(CellContent::spacer_head()),
            _ => return Err(ContentReadFailure::InvalidRange),
        }
        .map_err(|_| ContentReadFailure::Capacity)?;
        let hyperlink = if let Some(target) = cell.hyperlink {
            if let Some(existing) = links.iter().find(|value| value.target() == target) {
                Some(existing.id())
            } else {
                if links.len() >= MAX_PAGE_HYPERLINKS {
                    return Err(ContentReadFailure::Capacity);
                }
                let id = HyperlinkId::new(
                    u32::try_from(links.len() + 1).map_err(|_| ContentReadFailure::Exhausted)?,
                )
                .map_err(|_| ContentReadFailure::Capacity)?;
                let descriptor = HyperlinkDescriptor::new(id, target)
                    .map_err(|_| ContentReadFailure::Capacity)?;
                links
                    .try_reserve(1)
                    .map_err(|_| ContentReadFailure::Capacity)?;
                links.push(descriptor);
                Some(id)
            }
        } else {
            None
        };
        cells.push(StyledCell::new(content, cell_style, hyperlink));
    }
    ContentRow::fragment(
        row_id(row.id),
        row.revision,
        row.soft_wrapped,
        row.columns,
        row.start_column,
        cells,
    )
    .map_err(|_| ContentReadFailure::InvalidRange)
}
fn options(limits: PageLimits) -> vt::Options {
    vt::Options {
        max_rows: limits.rows(),
        max_cells: limits.cells(),
        max_bytes: limits.bytes() as usize,
        ..Default::default()
    }
}
fn rejected<T>(stamp: QueryStamp, reason: ContentReadFailure) -> ContentReadResult<T> {
    ContentReadResult::Rejected {
        request: stamp,
        reason,
    }
}
fn topology_row(row: &vt::Row) -> Result<TopologyRow, ContentReadFailure> {
    TopologyRow::new(row_id(row.id), row.revision, row.columns, row.soft_wrapped)
        .map_err(|_| ContentReadFailure::InvalidRange)
}

impl crate::TerminalEmulator {
    /// Bind authoritative content once. Legacy sessions need no synthetic identity.
    pub fn set_content_session(&mut self, session: SessionKey) -> Result<(), ContentReadFailure> {
        if session.epoch.get() == 0
            || session.generation.get() == 0
            || self.semantic_content.session.is_some()
        {
            return Err(ContentReadFailure::InvalidRange);
        }
        self.semantic_content.session = Some(session);
        Ok(())
    }
    pub fn content_base(&self) -> Result<ContentBase, ContentReadFailure> {
        self.semantic_content.base(&self.terminal)
    }
    pub fn content_revision(&self) -> ContentRevision {
        self.semantic_content.revision
    }
    /// The PTY producer supplies hidden-input facts; shell markers cannot grant them.
    pub fn set_content_hidden_input(&mut self, hidden: bool) -> Result<bool, ContentReadFailure> {
        if hidden == self.semantic_content.hidden_input {
            return Ok(false);
        }
        self.semantic_content.advance()?;
        self.semantic_content.hidden_input = hidden;
        Ok(true)
    }
    pub fn content_page(
        &mut self,
        request: &ContentPageRequest,
    ) -> Result<ContentPageResult, ContentReadFailure> {
        let stamp = request.stamp();
        let current = self.content_base()?;
        if current != stamp.base() {
            return Ok(ContentReadResult::ResyncRequired {
                request: stamp,
                current,
            });
        }
        let result = self.read_semantic_page(request);
        Ok(match result {
            Ok(page) => ContentReadResult::Ready(page),
            Err(reason) => rejected(stamp, reason),
        })
    }
    fn read_semantic_page(
        &self,
        request: &ContentPageRequest,
    ) -> Result<ContentPageReply, ContentReadFailure> {
        self.semantic_content
            .check(&self.terminal, request.stamp())?;
        let snapshot = self
            .semantic_content
            .reader
            .capture(&self.terminal)
            .map_err(native_failure)?;
        let mut options = options(request.limits());
        let (first, last) = match request.range() {
            ContentRange::ActiveGrid => (snapshot.active_start, snapshot.total_rows),
            ContentRange::History { first, last } => (
                first
                    .map(|id| {
                        self.semantic_content
                            .resolve(&self.terminal, id)
                            .map(|row| row.document_row)
                    })
                    .transpose()?
                    .unwrap_or(0),
                last.map(|id| {
                    self.semantic_content
                        .resolve(&self.terminal, id)
                        .and_then(|row| {
                            row.document_row
                                .checked_add(1)
                                .ok_or(ContentReadFailure::Exhausted)
                        })
                })
                .transpose()?
                .unwrap_or(snapshot.total_rows),
            ),
        };
        if first >= last {
            return Err(ContentReadFailure::InvalidRange);
        }
        options.row = first;
        options.end_row = last;
        if let Some(continuation) = request.continuation() {
            let position = continuation.page().position;
            let row = self
                .semantic_content
                .resolve(&self.terminal, position.row())?;
            if row.document_row < first || row.document_row >= last {
                return Err(ContentReadFailure::InvalidRange);
            }
            match position {
                PagePosition::AfterRow { row_revision, .. } => {
                    if row_revision != row.revision {
                        return Err(ContentReadFailure::InvalidRange);
                    }
                    options.row = row
                        .document_row
                        .checked_add(1)
                        .ok_or(ContentReadFailure::Exhausted)?;
                }
                PagePosition::WithinRow {
                    row_revision,
                    columns,
                    next_column,
                    ..
                } => {
                    if row_revision != row.revision || columns != row.columns {
                        return Err(ContentReadFailure::InvalidRange);
                    }
                    options.row = row.document_row;
                    options.column = next_column;
                }
            }
        }
        if options.row >= last {
            return Err(ContentReadFailure::InvalidRange);
        }
        for _ in 0..2 {
            let native = self
                .semantic_content
                .reader
                .read_page(&self.terminal, options)
                .map_err(native_failure)?;
            let page_first = native.rows.first().map_or(first, |row| row.document_row);
            let page_last = native.rows.last().map_or(last, |row| row.document_row + 1);
            let mut parts = ContentPageParts::new(request.stamp().base(), snapshot.columns);
            parts
                .rows
                .try_reserve(native.rows.len())
                .map_err(|_| ContentReadFailure::Capacity)?;
            for row in native.rows {
                parts.rows.push(convert_row(row, &mut parts.hyperlinks)?);
            }
            if !native.complete {
                parts.continuation = parts
                    .rows
                    .last()
                    .map(|row| PageContinuation::new(parts.base, row))
                    .transpose()
                    .map_err(|_| ContentReadFailure::InvalidRange)?;
            }
            parts.cursor = Some(Cursor {
                row: row_id(snapshot.cursor),
                column: snapshot.cursor_column,
                shape: match snapshot.cursor_shape {
                    1 => CursorShape::Underline,
                    2 => CursorShape::Bar,
                    _ => CursorShape::Block,
                },
                visible: self
                    .terminal
                    .mode(Mode::CURSOR_VISIBLE)
                    .map_err(native_failure)?,
                blinking: self
                    .terminal
                    .mode(Mode::CURSOR_BLINKING)
                    .map_err(native_failure)?,
            });
            parts.modes = self.semantic_modes()?;
            parts.colors = self.semantic_colors()?;
            self.semantic_graphics(&mut parts, page_first, page_last)?;
            let page = ContentPage::new(parts).map_err(|_| ContentReadFailure::Capacity)?;
            if page.accounted_bytes() > request.limits().bytes() as usize {
                // Native text accounting cannot predict row-scoped graphics. Reduce
                // the emitted text, not the graphics or the requested logical range.
                // The next fragment gets the normal revision-exact continuation.
                let overage = page.accounted_bytes() - request.limits().bytes() as usize;
                options.max_bytes = options
                    .max_bytes
                    .checked_sub(
                        overage
                            .checked_add(1024)
                            .ok_or(ContentReadFailure::Exhausted)?,
                    )
                    .filter(|bytes| *bytes >= 4096)
                    .ok_or(ContentReadFailure::Capacity)?;
                continue;
            }
            return ContentPageReply::new(request, page).map_err(|_| ContentReadFailure::Capacity);
        }
        Err(ContentReadFailure::Capacity)
    }
    pub fn topology_page(
        &mut self,
        request: &TopologyPageRequest,
    ) -> Result<TopologyPageResult, ContentReadFailure> {
        let current = self.content_base()?;
        let stamp = request.stamp();
        if current != stamp.base() {
            return Ok(ContentReadResult::ResyncRequired {
                request: stamp,
                current,
            });
        }
        let result = (|| {
            self.semantic_content.check(&self.terminal, stamp)?;
            let mut options = options(request.limits());
            if let Some(continuation) = request.continuation() {
                let previous = continuation.after();
                let row = self
                    .semantic_content
                    .resolve(&self.terminal, previous.row())?;
                if topology_row(&row)? != previous {
                    return Err(ContentReadFailure::InvalidRange);
                }
                options.seek = Some((native_row(previous.row()), true));
            }
            let native = self
                .semantic_content
                .reader
                .read_topology(&self.terminal, options)
                .map_err(native_failure)?;
            let rows = native
                .rows
                .iter()
                .map(topology_row)
                .collect::<Result<Vec<_>, _>>()?;
            let continuation = if native.complete {
                None
            } else {
                rows.last()
                    .copied()
                    .map(|row| TopologyContinuation::new(stamp, row))
                    .transpose()
                    .map_err(|_| ContentReadFailure::InvalidRange)?
            };
            TopologyPage::new(request, rows, continuation).map_err(|_| ContentReadFailure::Capacity)
        })();
        Ok(match result {
            Ok(page) => ContentReadResult::Ready(page),
            Err(reason) => rejected(stamp, reason),
        })
    }
    fn semantic_modes(&self) -> Result<TerminalModes, ContentReadFailure> {
        let mode = |value| self.terminal.mode(value).map_err(native_failure);
        Ok(TerminalModes {
            application_cursor: mode(Mode::DECCKM)?,
            application_keypad: mode(Mode::KEYPAD_KEYS)?,
            bracketed_paste: mode(Mode::BRACKETED_PASTE)?,
            focus_reporting: mode(Mode::FOCUS_EVENT)?,
            synchronized_output: mode(Mode::SYNC_OUTPUT)?,
            origin: mode(Mode::ORIGIN)?,
            insert: mode(Mode::INSERT)?,
            wraparound: mode(Mode::WRAPAROUND)?,
            reverse_colors: mode(Mode::REVERSE_COLORS)?,
            hidden_input: self.semantic_content.hidden_input,
            mouse_tracking: if mode(Mode::ANY_MOUSE)? {
                MouseTracking::Any
            } else if mode(Mode::BUTTON_MOUSE)? {
                MouseTracking::Button
            } else if mode(Mode::NORMAL_MOUSE)? || mode(Mode::X10_MOUSE)? {
                MouseTracking::Press
            } else {
                MouseTracking::None
            },
            mouse_encoding: if mode(Mode::SGR_PIXELS_MOUSE)? {
                MouseEncoding::SgrPixels
            } else if mode(Mode::SGR_MOUSE)? {
                MouseEncoding::Sgr
            } else if mode(Mode::URXVT_MOUSE)? {
                MouseEncoding::Urxvt
            } else if mode(Mode::UTF8_MOUSE)? {
                MouseEncoding::Utf8
            } else {
                MouseEncoding::X10
            },
        })
    }
    fn semantic_colors(&self) -> Result<ProgramColorOverrides, ContentReadFailure> {
        let rgb = |value: libghostty_vt::style::RgbColor| Rgb {
            r: value.r,
            g: value.g,
            b: value.b,
        };
        let mut colors = ProgramColorOverrides {
            foreground: self
                .terminal
                .fg_color_overridden()
                .map_err(native_failure)?
                .then(|| self.terminal.fg_color())
                .transpose()
                .map_err(native_failure)?
                .flatten()
                .map(rgb),
            background: self
                .terminal
                .bg_color_overridden()
                .map_err(native_failure)?
                .then(|| self.terminal.bg_color())
                .transpose()
                .map_err(native_failure)?
                .flatten()
                .map(rgb),
            cursor: if self
                .terminal
                .cursor_color_overridden()
                .map_err(native_failure)?
            {
                self.terminal
                    .cursor_color()
                    .map_err(native_failure)?
                    .map(rgb)
            } else {
                None
            },
            palette: Vec::new(),
        };
        let mask = self
            .terminal
            .color_palette_overrides()
            .map_err(native_failure)?;
        let palette = self.terminal.color_palette().map_err(native_failure)?;
        for index in 0..=255u8 {
            if mask.is_set(libghostty_vt::style::PaletteIndex(index)) {
                colors.palette.push(PaletteOverride {
                    index,
                    color: rgb(palette.0[index as usize]),
                });
            }
        }
        Ok(colors)
    }
}

/// A worker quantum, independent of logical Copy length and client residency.
#[derive(Clone, Copy, Debug)]
pub struct ContentScanBudget {
    pub rows: u16,
    pub cells: u32,
    pub bytes: usize,
}
impl Default for ContentScanBudget {
    fn default() -> Self {
        Self {
            rows: 256,
            cells: 16_384,
            bytes: MAX_RESOURCE_CHUNK_BYTES,
        }
    }
}
impl ContentScanBudget {
    pub(crate) fn options(
        self,
        row: u32,
        column: u16,
        end_row: u32,
    ) -> Result<vt::Options, ContentReadFailure> {
        if self.rows == 0 || self.cells == 0 || self.bytes < 8192 {
            return Err(ContentReadFailure::Capacity);
        }
        Ok(vt::Options {
            row,
            column,
            end_row,
            max_rows: self.rows.min(1),
            max_cells: self.cells.min(64),
            // A maximum grapheme plus hyperlink must fit one atomic quantum.
            max_bytes: self.bytes.min(64 * 1024),
            seek: None,
        })
    }
}

impl crate::TerminalEmulator {
    /// The worker stamps output at its source. Polling a retained confirmation
    /// cannot assign a newer tick to an older prompt commit.
    pub fn feed_observed(
        &mut self,
        bytes: &[u8],
        observed_at: spaceterm_core::observations::MonotonicTick,
    ) -> Result<(), ContentReadFailure> {
        self.feed_observed_at(bytes, observed_at, std::time::Instant::now())
    }
    pub(crate) fn feed_observed_at(
        &mut self,
        bytes: &[u8],
        observed_at: spaceterm_core::observations::MonotonicTick,
        now: std::time::Instant,
    ) -> Result<(), ContentReadFailure> {
        self.content_base()?;
        if let Some(reason) = self.semantic_content.observation_failure {
            return Err(reason);
        }
        self.semantic_content.observation_tick = Some(observed_at);
        self.feed_at(bytes, now);
        self.semantic_content.observation_tick = None;
        self.content_base()?;
        if let Some(reason) = self.semantic_content.observation_failure {
            return Err(reason);
        }
        Ok(())
    }
    pub(crate) fn capture_confirmed_directory(&mut self, directory: &str) {
        use spaceterm_core::{observations::*, organization::ServerDirectory};
        let Some(session) = self.semantic_content.session else {
            return;
        };
        let Some(observed_at) = self.semantic_content.observation_tick else {
            return;
        };
        let result = (|| {
            let next = self
                .semantic_content
                .observation_sequence
                .checked_add(1)
                .ok_or(ContentReadFailure::Exhausted)?;
            let sequence =
                RuntimeObservationSequence::new(next).map_err(|_| ContentReadFailure::Exhausted)?;
            if directory.len() > spaceterm_core::organization::MAX_DIRECTORY_BYTES {
                return Err(ContentReadFailure::Capacity);
            }
            let mut bytes = Vec::new();
            bytes
                .try_reserve(directory.len())
                .map_err(|_| ContentReadFailure::Capacity)?;
            bytes.extend_from_slice(directory.as_bytes());
            let directory = ServerDirectory::new(session.pane.server(), bytes)
                .map_err(|_| ContentReadFailure::InvalidRange)?;
            let observation =
                ConfirmedDirectoryObservation::new(session, sequence, observed_at, directory)
                    .map_err(|_| ContentReadFailure::InvalidRange)?;
            self.semantic_content.observation_sequence = next;
            self.semantic_content.confirmed_directory = Some(observation);
            Ok(())
        })();
        if let Err(reason) = result {
            self.semantic_content.observation_failure = Some(reason);
        }
    }
    pub fn take_confirmed_directory(
        &mut self,
    ) -> Option<spaceterm_core::observations::ConfirmedDirectoryObservation> {
        self.semantic_content.confirmed_directory.take()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{FindQueryGeneration, TerminalEmulator, geometry::*};
    use libghostty_vt::terminal::{CompressionResult, ScrollViewport};
    use spaceterm_core::{authority::*, identity::*};

    #[test]
    fn source_feed_exhaustion_is_an_explicit_failure_before_native_output() {
        let mut emulator = TerminalEmulator::new(TerminalGeometry::from_grid(
            CellGridSize::new(8, 2),
            LogicalCellSize::new(10.0, 20.0),
            BackingScale::ONE,
        ))
        .unwrap();
        let session = SessionKey {
            pane: PaneId::new(ServerId::new([12; 16]), 1).unwrap(),
            epoch: ServerEpoch::new(1),
            generation: SessionGeneration::new(1),
        };
        emulator.set_content_session(session).unwrap();
        let before = emulator
            .semantic_content
            .reader
            .capture(&emulator.terminal)
            .unwrap();
        emulator.semantic_content.revision = ContentRevision::new(u64::MAX);
        assert_eq!(
            emulator.feed_observed(
                b"unapplied",
                spaceterm_core::observations::MonotonicTick::new(1)
            ),
            Err(ContentReadFailure::Exhausted)
        );
        assert_eq!(emulator.content_base(), Err(ContentReadFailure::Exhausted));
        assert_eq!(
            emulator
                .semantic_content
                .reader
                .capture(&emulator.terminal)
                .unwrap()
                .source_revision,
            before.source_revision
        );
    }

    #[test]
    fn content_reads_leave_installed_selection_find_and_viewport_unchanged() {
        let mut emulator = TerminalEmulator::new(TerminalGeometry::from_grid(
            CellGridSize::new(80, 4),
            LogicalCellSize::new(10.0, 20.0),
            BackingScale::ONE,
        ))
        .unwrap();
        let server = ServerId::new([11; 16]);
        let session = SessionKey {
            pane: PaneId::new(server, 1).unwrap(),
            epoch: ServerEpoch::new(1),
            generation: SessionGeneration::new(1),
        };
        emulator.set_content_session(session).unwrap();
        for index in 0..500 {
            emulator.feed(format!("{index:04} retained needle\r\n").as_bytes());
        }
        let selection = emulator.terminal.select_all().unwrap().unwrap();
        emulator.terminal.set_selection(Some(&selection)).unwrap();
        emulator.set_find_query(FindQueryGeneration::test(1), "needle".into());
        emulator.terminal.scroll_viewport(ScrollViewport::Top);
        let before = emulator.snapshot().unwrap().unwrap();
        let selected = emulator.selection_text().unwrap();
        for _ in 0..128 {
            if emulator.compress_scrollback().unwrap() != CompressionResult::Pending {
                break;
            }
        }
        let activity = emulator.compression_activity().unwrap();
        let base = emulator.content_base().unwrap();
        let client = ClientId::new(server, 1).unwrap();
        let fence = EffectFence::new(
            session,
            ControlGrant::new(session.pane, client, ControlGeneration::new(1)).unwrap(),
            ConnectionBinding::new(client, ConnectionGeneration::new(1)),
            ContextGeneration::new(1),
            Some(base.revision),
        )
        .unwrap();
        let stamp = QueryStamp::new(
            fence,
            base,
            ContentRequestId::new(1).unwrap(),
            RequestGeneration::new(1),
        )
        .unwrap();
        let request = ContentPageRequest::new(
            stamp,
            ContentRange::History {
                first: None,
                last: None,
            },
            None,
            PageLimits::new(256, 16_384, 1024 * 1024).unwrap(),
        )
        .unwrap();
        assert!(matches!(
            emulator.content_page(&request).unwrap(),
            ContentReadResult::Ready(_)
        ));
        let topology = TopologyPageRequest::new(stamp, None, request.limits()).unwrap();
        assert!(matches!(
            emulator.topology_page(&topology).unwrap(),
            ContentReadResult::Ready(_)
        ));
        assert_eq!(emulator.content_base().unwrap(), base);
        assert_eq!(emulator.compression_activity().unwrap(), activity);
        assert_eq!(emulator.selection_text().unwrap(), selected);
        if let Some(after) = emulator.snapshot().unwrap() {
            assert_eq!(after.viewport, before.viewport);
            assert_eq!(after.find, before.find);
            assert_eq!(after.selection_present, before.selection_present);
        }
    }
}
