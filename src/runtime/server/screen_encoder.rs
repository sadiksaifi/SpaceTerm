//! Encodes screen snapshots as row style runs against the frame the client holds.
//!
//! The emulator keeps an unchanged row's `Arc` across snapshots, so pointer equality with the
//! baseline is the per-row damage. A row that moved is a new `Arc` with the old cells, so each
//! changed row is scanned once into its runs and a hash of what they send, and a scroll is found
//! by matching those hashes against the baseline's before any row is encoded. A row is encoded,
//! from its scan, only when its cells differ from the baseline row the client shows at its
//! position after the scroll.
use super::*;
use spaceterm_server::pane::effects::Hyperlink;
use spaceterm_server::protocol::limits;
use spaceterm_server::protocol::screen::{
    self as wire, Cells, Colors, Cursor, CursorPosition, Geometry, History, LinkDefinition, Modes,
    Page, Run, ScreenUpdate, ScrollShift, Style, StyleDefinition, StyleRef, flags,
};
use std::collections::HashMap;
use std::ops::Range;

/// A Pane's screen state, captured under the `PaneService` lock and encoded outside it.
#[derive(Clone)]
pub struct ScreenCapture {
    pub(crate) screen: Arc<ScreenSnapshot>,
    /// Fences pointer and wheel coordinates.
    pub(crate) generation: u64,
    pub(crate) accessibility_revision: u64,
}

/// One channel's encoder. It holds the last frame it emitted and the style and link tables the
/// client holds.
#[derive(Default)]
pub(crate) struct ScreenEncoder {
    tables: Tables,
    baseline: Option<Baseline>,
    /// Rows the last `encode` call encoded.
    #[cfg(test)]
    encoded_rows: usize,
}

struct Baseline {
    frame: u64,
    rows: Vec<Encoded>,
    cursor: Cursor,
    history: History,
    geometry: Geometry,
    modes: Modes,
    colors: Colors,
    graphics: GraphicsSnapshot,
    generation: u64,
    accessibility_revision: u64,
}

/// One row as encoded, with the snapshot row it came from and that row's [`Scan`] hash.
struct Encoded {
    source: RowSnapshot,
    hash: u64,
    line: Arc<Line>,
}

/// One snapshot row before encoding.
struct Pending<'a> {
    source: &'a RowSnapshot,
    wrapped: bool,
    /// The row's [`Scan`] hash.
    hash: u64,
    /// The row sends no cell.
    blank: bool,
    /// `None` for the baseline row at the same index, whose hash and blankness it shares.
    scan: Option<Scan>,
}

/// A row's cells up to the last one it sends, split where a style field or the sent link
/// changes, and a hash of the sent text, styles and links. Cell metadata the client never receives is left
/// out, so equal hashes only suggest equal rows.
#[derive(Clone)]
struct Scan {
    /// The cells after `end` are blank.
    end: usize,
    hash: u64,
    /// This row's [`RunStart`]s in the frame's list.
    starts: Range<usize>,
}

/// The first cell of a run, with the style and link every cell of the run sends.
struct RunStart<'a> {
    cell: usize,
    style: Style,
    link: Option<&'a HyperlinkTarget>,
}

/// One row's content without its index. Style and link ids refer to the encoder's tables.
#[derive(Debug, PartialEq, Eq)]
struct Line {
    wrapped: bool,
    runs: Vec<Run>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    /// Changes against the client's copy of the baseline.
    Delta,
    /// Everything, without blank rows. The client starts from blank rows and empty tables.
    Keyframe,
    /// Everything, with every row, against a base this encoder does not hold.
    Replace,
}

impl ScreenEncoder {
    #[cfg(test)]
    pub(crate) fn encoded_rows(&self) -> usize {
        self.encoded_rows
    }

    /// Encodes `capture` as frame `frame` against the client's frame `base`, or as a keyframe when
    /// `base` is `None`. Returns `None` when the capture equals the baseline; the baseline then
    /// stays as it was. A base other than the last emitted frame receives every row.
    pub(crate) fn encode(
        &mut self,
        capture: &ScreenCapture,
        frame: u64,
        base: Option<u64>,
    ) -> Option<ScreenUpdate> {
        #[cfg(test)]
        {
            self.encoded_rows = 0;
        }
        let previous = self.baseline.take();
        let mode = match (&previous, base) {
            (_, None) => Mode::Keyframe,
            (Some(previous), Some(base)) if previous.frame == base => Mode::Delta,
            (_, Some(_)) => Mode::Replace,
        };
        let known = previous.as_ref().filter(|_| mode == Mode::Delta);
        let encoded = match self.build(capture, known, mode) {
            Some(built) => Some(built),
            None if self.tables.overflowed => self.build(capture, None, Mode::Replace),
            None => None,
        };
        match encoded {
            Some((update, built)) => {
                self.baseline = Some(Baseline::new(capture, frame, built, &update));
                Some(update)
            }
            None => {
                self.baseline = previous;
                None
            }
        }
    }

    /// `None` when nothing changed, or when the link table overflowed in `Delta` mode.
    fn build(
        &mut self,
        capture: &ScreenCapture,
        previous: Option<&Baseline>,
        mode: Mode,
    ) -> Option<(ScreenUpdate, Built)> {
        if mode != Mode::Delta {
            self.tables = Tables::default();
        }
        self.tables.overflowed = false;
        let screen = capture.screen.as_ref();
        let geometry = geometry(screen);
        let resized = previous.is_some_and(|previous| {
            (previous.geometry.columns, previous.geometry.rows) != (geometry.columns, geometry.rows)
        });
        let mut starts = Vec::new();
        let pending = screen
            .rows
            .iter()
            .enumerate()
            .map(|(index, source)| {
                let wrapped = screen.row_soft_wrapped.get(index).copied().unwrap_or(false);
                let same = previous
                    .filter(|_| !resized)
                    .and_then(|previous| previous.rows.get(index))
                    .filter(|old| Arc::ptr_eq(&old.source, source));
                match same {
                    Some(old) => Pending {
                        source,
                        wrapped,
                        hash: old.hash,
                        blank: old.line.runs.is_empty(),
                        scan: None,
                    },
                    None => {
                        let scan = Scan::of(source, &mut starts);
                        Pending {
                            source,
                            wrapped,
                            hash: scan.hash,
                            blank: scan.end == 0,
                            scan: Some(scan),
                        }
                    }
                }
            })
            .collect::<Vec<_>>();
        // The baseline row the client shows at each position once `shift` applies. `None` is a
        // blank row: a keyframe starts from blank rows, a resize sends every row that is not
        // blank, and a scroll moves blank rows in.
        let (shift, shown) = match previous.filter(|_| !resized) {
            Some(previous) => match find_shift(&previous.rows, &pending) {
                Some((shift, shown)) => (Some(shift), shown),
                None => (None, previous.rows.iter().map(Some).collect()),
            },
            None => (None, Vec::new()),
        };
        let blank = Arc::new(Line::blank());
        let mut changed = Vec::with_capacity(pending.len());
        let mut rows = Vec::with_capacity(pending.len());
        for (index, new) in pending.iter().enumerate() {
            let shown = shown.get(index).copied().flatten();
            let shown_line = shown.map_or(&blank, |old| &old.line);
            let line = match shown {
                Some(old) if new.shown_by(Some(old)) => Arc::clone(&old.line),
                // Each blank row gets its own `Arc`: rows sharing one would serialize every
                // frame's reference counting on one address.
                None if new.shown_by(None) => Arc::new(Line::blank()),
                _ => {
                    #[cfg(test)]
                    {
                        self.encoded_rows += 1;
                    }
                    let scan = match &new.scan {
                        Some(scan) => scan.clone(),
                        None => Scan::of(new.source, &mut starts),
                    };
                    Arc::new(Line::build(
                        new.source,
                        new.wrapped,
                        &scan,
                        &starts,
                        &mut self.tables,
                    ))
                }
            };
            changed.push(mode == Mode::Replace || line != *shown_line);
            rows.push(Encoded {
                source: Arc::clone(new.source),
                hash: new.hash,
                line,
            });
        }
        if mode == Mode::Delta && self.tables.overflowed {
            return None;
        }
        let changed_rows = rows
            .iter()
            .zip(changed)
            .enumerate()
            .filter(|(_, (_, changed))| *changed)
            .map(|(index, (row, _))| wire::Row {
                index: index as u16,
                wrapped: row.line.wrapped,
                runs: row.line.runs.clone(),
            })
            .collect::<Vec<_>>();
        let (styles, links) = self.tables.take_definitions();
        let modes = modes(screen);
        let colors = colors(screen);
        let colors_changed = previous.is_none_or(|previous| previous.colors != colors);
        let update = ScreenUpdate {
            cursor: cursor(screen),
            history: History {
                epoch: screen.history.epoch,
                oldest: screen.history.oldest,
                total: screen.history.total,
            },
            geometry: previous
                .is_none_or(|previous| previous.geometry != geometry)
                .then_some(geometry),
            modes: previous
                .is_none_or(|previous| previous.modes != modes)
                .then_some(modes),
            colors: colors_changed.then(|| colors.clone()),
            shift,
            styles,
            links,
            rows: changed_rows,
            graphics: previous
                .is_none_or(|previous| previous.graphics != screen.graphics)
                .then(|| super::graphics::encode(&screen.graphics)),
            accessibility_revision: capture.accessibility_revision,
            generation: capture.generation,
        };
        if let Some(previous) = previous
            && update.rows.is_empty()
            && update.shift.is_none()
            && update.styles.is_empty()
            && update.links.is_empty()
            && update.geometry.is_none()
            && update.modes.is_none()
            && update.colors.is_none()
            && update.graphics.is_none()
            && update.cursor == previous.cursor
            && update.history == previous.history
            && update.generation == previous.generation
            && update.accessibility_revision == previous.accessibility_revision
        {
            return None;
        }
        Some((update, Built { rows, colors }))
    }
}

/// What a built frame leaves for its [`Baseline`].
struct Built {
    rows: Vec<Encoded>,
    colors: Colors,
}

impl Baseline {
    fn new(capture: &ScreenCapture, frame: u64, built: Built, update: &ScreenUpdate) -> Self {
        let screen = capture.screen.as_ref();
        Self {
            frame,
            rows: built.rows,
            cursor: update.cursor,
            history: update.history,
            geometry: geometry(screen),
            modes: modes(screen),
            colors: built.colors,
            graphics: screen.graphics.clone(),
            generation: update.generation,
            accessibility_revision: update.accessibility_revision,
        }
    }
}

/// Encodes Scrollback rows with page-local tables, so a page never depends on frame order.
pub(crate) fn encode_page<'a>(
    epoch: u64,
    start: u64,
    rows: impl IntoIterator<Item = (&'a [CellSnapshot], bool)>,
) -> Page {
    let mut tables = Tables::default();
    let rows = rows
        .into_iter()
        .take(usize::from(limits::MAX_PAGE_ROWS))
        .enumerate()
        .map(|(index, (cells, wrapped))| {
            let line = Line::encode(cells, wrapped, &mut tables);
            wire::Row {
                index: index as u16,
                wrapped: line.wrapped,
                runs: line.runs,
            }
        })
        .collect();
    let (styles, links) = tables.take_definitions();
    Page {
        epoch,
        start,
        rows,
        styles,
        links,
    }
}

/// Finds the scroll that leaves the fewest rows to encode, comparing row hashes. It returns the
/// shift and the baseline rows the client shows after it, `None` for a row that moved in blank.
/// It returns `None` when no shift saves a row.
fn find_shift<'a>(
    old: &'a [Encoded],
    new: &[Pending],
) -> Option<(ScrollShift, Vec<Option<&'a Encoded>>)> {
    let changed = new
        .iter()
        .zip(old)
        .filter(|(new, old)| !new.resembles(Some(old)))
        .count();
    if changed < 2 {
        return None;
    }
    let mut positions = HashMap::<u64, usize>::with_capacity(old.len());
    for (index, line) in old.iter().enumerate().rev() {
        if !line.line.runs.is_empty() {
            positions.insert(line.hash, index);
        }
    }
    let mut votes = HashMap::<isize, usize>::new();
    for (index, (row, old)) in new.iter().zip(old).enumerate() {
        if !row.resembles(Some(old))
            && !row.blank
            && let Some(source) = positions.get(&row.hash)
        {
            *votes.entry(*source as isize - index as isize).or_default() += 1;
        }
    }
    let mut candidates = votes.into_iter().collect::<Vec<_>>();
    candidates.sort_unstable_by(|a, b| {
        b.1.cmp(&a.1)
            .then(a.0.unsigned_abs().cmp(&b.0.unsigned_abs()))
    });
    let mut best: Option<(usize, ScrollShift, Vec<Option<&Encoded>>)> = None;
    for (lines, _) in candidates.into_iter().take(3) {
        let Some(shift) = shift_region(old, new, lines) else {
            continue;
        };
        let mut shifted = old.iter().map(Some).collect::<Vec<_>>();
        if shift.apply(&mut shifted, || None).is_none() {
            continue;
        }
        let remaining = new
            .iter()
            .zip(&shifted)
            .filter(|(new, old)| !new.resembles(**old))
            .count();
        if best
            .as_ref()
            .is_none_or(|(fewest, _, _)| remaining < *fewest)
        {
            best = Some((remaining, shift, shifted));
        }
    }
    best.filter(|(remaining, _, _)| *remaining < changed)
        .map(|(_, shift, shifted)| (shift, shifted))
}

/// The region of the longest stretch of rows where `new[i]` resembles `old[i + lines]`.
fn shift_region(old: &[Encoded], new: &[Pending], lines: isize) -> Option<ScrollShift> {
    let mut longest = 0..0;
    let mut start = None;
    for index in 0..=new.len() {
        let matched = index < new.len()
            && index
                .checked_add_signed(lines)
                .and_then(|source| old.get(source))
                .is_some_and(|old| new[index].resembles(Some(old)));
        match (matched, start) {
            (true, None) => start = Some(index),
            (false, Some(first)) => {
                if index - first > longest.len() {
                    longest = first..index;
                }
                start = None;
            }
            _ => {}
        }
    }
    if longest.is_empty() {
        return None;
    }
    let (top, bottom) = if lines > 0 {
        (longest.start, longest.end + lines as usize)
    } else {
        (longest.start - lines.unsigned_abs(), longest.end)
    };
    Some(ScrollShift {
        top: u16::try_from(top).ok()?,
        bottom: u16::try_from(bottom).ok()?,
        lines: i16::try_from(lines).ok()?,
    })
}

impl Pending<'_> {
    /// Whether this row probably equals `old`, a blank row when `None`. Scroll detection uses
    /// this, so it compares hashes and no cells.
    fn resembles(&self, old: Option<&Encoded>) -> bool {
        match old {
            Some(old) => {
                old.line.wrapped == self.wrapped
                    && (Arc::ptr_eq(&old.source, self.source) || old.hash == self.hash)
            }
            None => !self.wrapped && self.blank,
        }
    }

    /// Whether the client already shows this row where it shows `old`, a blank row when `None`.
    fn shown_by(&self, old: Option<&Encoded>) -> bool {
        match old {
            Some(old) => {
                old.line.wrapped == self.wrapped
                    && (Arc::ptr_eq(&old.source, self.source)
                        || (old.hash == self.hash && same_cells(&old.source, self.source)))
            }
            None => self.resembles(None),
        }
    }
}

impl Scan {
    /// Scans `cells`, appending its run starts to `starts`. Trailing blank cells are skipped
    /// first, as the encoding omits them.
    fn of<'a>(cells: &'a [CellSnapshot], starts: &mut Vec<RunStart<'a>>) -> Self {
        let end = cells
            .iter()
            .rposition(|cell| !blank(cell))
            .map_or(0, |last| last + 1);
        let first = starts.len();
        let mut hash = RowHasher::default();
        // Equal fields give equal styles, so styles are made only where the fields change.
        let mut current: Option<(StyleFields, Option<&HyperlinkTarget>)> = None;
        for (index, cell) in cells[..end].iter().enumerate() {
            let fields = StyleFields::of(cell);
            let link = sent_link(cell);
            if current.is_none_or(|(last, last_link)| last != fields || !same_link(last_link, link))
            {
                let style = fields.style();
                hash.style(style);
                hash.link(link);
                starts.push(RunStart {
                    cell: index,
                    style,
                    link,
                });
                current = Some((fields, link));
            }
            hash.text(text(cell));
        }
        Self {
            end,
            hash: hash.0,
            starts: first..starts.len(),
        }
    }
}

/// Whether every field of every cell is equal, as `==` decides. It compares the inline fields
/// first, and one-byte text without a call.
fn same_cells(a: &[CellSnapshot], b: &[CellSnapshot]) -> bool {
    a.len() == b.len() && a.iter().zip(b).all(|(a, b)| same_cell(a, b))
}

fn same_cell(a: &CellSnapshot, b: &CellSnapshot) -> bool {
    // Naming every field fails the build when a field is added.
    let CellSnapshot {
        text,
        foreground_source,
        background_source,
        inverse,
        bold,
        faint,
        italic,
        blinking,
        invisible,
        underline,
        underline_source,
        strikethrough,
        overline,
        selected,
        spacer_tail,
        semantic_content,
        hyperlink,
    } = a;
    let flags = [
        *inverse,
        *bold,
        *faint,
        *italic,
        *blinking,
        *invisible,
        *strikethrough,
        *overline,
        *selected,
        *spacer_tail,
    ] == [
        b.inverse,
        b.bold,
        b.faint,
        b.italic,
        b.blinking,
        b.invisible,
        b.strikethrough,
        b.overline,
        b.selected,
        b.spacer_tail,
    ];
    flags
        && *foreground_source == b.foreground_source
        && *background_source == b.background_source
        && *underline_source == b.underline_source
        && *underline == b.underline
        && *semantic_content == b.semantic_content
        && match (text.as_bytes(), b.text.as_bytes()) {
            ([a], [b]) => a == b,
            (a, b) => a == b,
        }
        && *hyperlink == b.hyperlink
}

/// The link the client receives for `cell`: none when the target exceeds
/// [`limits::MAX_LINK_BYTES`].
fn sent_link(cell: &CellSnapshot) -> Option<&HyperlinkTarget> {
    cell.hyperlink
        .as_deref()
        .filter(|target| target.value.len() <= limits::MAX_LINK_BYTES)
}

/// Whether two links reach the client as the same table entry.
fn same_link(a: Option<&HyperlinkTarget>, b: Option<&HyperlinkTarget>) -> bool {
    match (a, b) {
        (None, None) => true,
        (Some(a), Some(b)) => std::ptr::eq(a, b) || (a.kind == b.kind && a.value == b.value),
        _ => false,
    }
}

/// A fast, non-cryptographic hash of a row's sent content, one multiply per short cell. Collisions
/// only cost scroll detection a match, because reuse compares cells.
#[derive(Default)]
struct RowHasher(u64);

impl RowHasher {
    const RUN: u64 = u64::MAX;

    fn word(&mut self, word: u64) {
        self.0 = (self.0.rotate_left(5) ^ word).wrapping_mul(0x517c_c1b7_2722_0a95);
    }

    /// Mixes `text` with its length, so cell boundaries count.
    fn text(&mut self, text: &str) {
        let bytes = text.as_bytes();
        if let [byte] = bytes {
            self.word(u64::from(*byte) | 1 << 56);
            return;
        }
        let mut chunks = bytes.chunks_exact(7);
        for chunk in &mut chunks {
            let mut word = [0xff; 8];
            word[..7].copy_from_slice(chunk);
            self.word(u64::from_le_bytes(word));
        }
        let rest = chunks.remainder();
        let mut word = [0; 8];
        word[..rest.len()].copy_from_slice(rest);
        word[7] = rest.len() as u8;
        self.word(u64::from_le_bytes(word));
    }

    fn style(&mut self, style: Style) {
        let color = |color: pane::Color| match color {
            pane::Color::Default => 0,
            pane::Color::Palette(index) => 1 << 24 | u64::from(index),
            pane::Color::Rgb([red, green, blue]) => {
                2 << 24 | u64::from(red) << 16 | u64::from(green) << 8 | u64::from(blue)
            }
        };
        self.word(Self::RUN);
        self.word(color(style.foreground) | color(style.background) << 32);
        self.word(
            color(style.underline_color)
                | u64::from(style.flags) << 32
                | u64::from(style.underline) << 48,
        );
    }

    fn link(&mut self, link: Option<&HyperlinkTarget>) {
        match link {
            None => self.word(0),
            Some(target) => {
                self.word(match target.kind {
                    HyperlinkKind::Url => 1,
                    HyperlinkKind::LocalPath => 2,
                });
                self.text(&target.value);
            }
        }
    }
}

impl Line {
    fn blank() -> Self {
        Self {
            wrapped: false,
            runs: Vec::new(),
        }
    }

    /// Encodes `cells` without trailing blank cells. A style that does not fit the table travels
    /// inline. A link that does not fit loses its target and sets `overflowed`.
    fn encode(cells: &[CellSnapshot], wrapped: bool, tables: &mut Tables) -> Self {
        let mut starts = Vec::new();
        let scan = Scan::of(cells, &mut starts);
        Self::build(cells, wrapped, &scan, &starts, tables)
    }

    /// Encodes `cells` from their `scan`, whose run starts are in `starts`. Runs whose style and
    /// link ids are equal, because a link lost its target, merge.
    fn build(
        cells: &[CellSnapshot],
        wrapped: bool,
        scan: &Scan,
        starts: &[RunStart],
        tables: &mut Tables,
    ) -> Self {
        let starts = &starts[scan.starts.clone()];
        let mut runs = Vec::<Run>::with_capacity(starts.len());
        // The first cell of the last run, and whether each of its cells holds one character.
        let mut first = 0;
        let mut narrow = true;
        let mut last_style: Option<(Style, StyleRef)> = None;
        let mut last_link: Option<(&HyperlinkTarget, Option<u16>)> = None;
        for (index, start) in starts.iter().enumerate() {
            let stop = starts.get(index + 1).map_or(scan.end, |next| next.cell);
            let style = match last_style {
                Some((last, id)) if last == start.style => id,
                _ => tables.style(start.style),
            };
            last_style = Some((start.style, style));
            let link = match (start.link, last_link) {
                (None, _) => None,
                (Some(target), Some((last, id))) if std::ptr::eq(target, last) => id,
                (Some(target), _) => {
                    let id = tables.link(target);
                    last_link = Some((target, id));
                    id
                }
            };
            if runs
                .last()
                .is_none_or(|run| run.style != style || run.link != link)
            {
                finish(runs.last_mut(), &cells[first..start.cell], narrow);
                first = start.cell;
                narrow = true;
                runs.push(Run {
                    style,
                    link,
                    text: String::with_capacity(stop - start.cell),
                    cells: Cells::Narrow,
                });
            }
            if let Some(run) = runs.last_mut() {
                for cell in &cells[start.cell..stop] {
                    let text = text(cell);
                    narrow &= text.chars().nth(1).is_none() && !text.is_empty();
                    run.text.push_str(text);
                }
            }
        }
        finish(runs.last_mut(), &cells[first..scan.end], narrow);
        Self { wrapped, runs }
    }
}

/// Gives `run`, made of `cells`, explicit cell lengths unless each cell holds one character.
fn finish(run: Option<&mut Run>, cells: &[CellSnapshot], narrow: bool) {
    if let Some(run) = run
        && !narrow
    {
        run.cells = Cells::Explicit(cells.iter().map(|cell| text(cell).len() as u16).collect());
    }
}

/// The cell's text as sent: empty for a wide character's spacer, at most
/// [`limits::MAX_CELL_BYTES`] otherwise.
fn text(cell: &CellSnapshot) -> &str {
    if cell.spacer_tail {
        return "";
    }
    let text = cell.text.as_str();
    if text.is_empty() {
        return " ";
    }
    let mut end = text.len().min(limits::MAX_CELL_BYTES);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

/// A cell a row may omit after its last run.
fn blank(cell: &CellSnapshot) -> bool {
    cell.text == " "
        && !cell.spacer_tail
        && cell.hyperlink.is_none()
        && StyleFields::of(cell) == StyleFields::DEFAULT
}

fn style(cell: &CellSnapshot) -> Style {
    StyleFields::of(cell).style()
}

/// The cell fields its sent [`Style`] is made of, cheaper to compare than the style.
#[derive(Clone, Copy, PartialEq)]
struct StyleFields {
    foreground: TerminalColor,
    background: TerminalColor,
    underline_color: TerminalColor,
    /// In the order of `FLAGS`.
    flags: [bool; 9],
    underline: TerminalUnderlineSnapshot,
}

impl StyleFields {
    const FLAGS: [u16; 9] = [
        flags::INVERSE,
        flags::BOLD,
        flags::FAINT,
        flags::ITALIC,
        flags::BLINKING,
        flags::INVISIBLE,
        flags::STRIKETHROUGH,
        flags::OVERLINE,
        flags::SELECTED,
    ];

    /// The fields of [`Style::DEFAULT`].
    const DEFAULT: Self = Self {
        foreground: TerminalColor::Default,
        background: TerminalColor::Default,
        underline_color: TerminalColor::Default,
        flags: [false; 9],
        underline: TerminalUnderlineSnapshot::None,
    };

    fn of(cell: &CellSnapshot) -> Self {
        Self {
            foreground: cell.foreground_source,
            background: cell.background_source,
            underline_color: cell.underline_source,
            flags: [
                cell.inverse,
                cell.bold,
                cell.faint,
                cell.italic,
                cell.blinking,
                cell.invisible,
                cell.strikethrough,
                cell.overline,
                cell.selected,
            ],
            underline: cell.underline,
        }
    }

    fn style(self) -> Style {
        Style {
            foreground: color(self.foreground),
            background: color(self.background),
            underline_color: color(self.underline_color),
            flags: self
                .flags
                .iter()
                .zip(Self::FLAGS)
                .filter(|(set, _)| **set)
                .fold(0, |flags, (_, flag)| flags | flag),
            underline: match self.underline {
                TerminalUnderlineSnapshot::None => 0,
                TerminalUnderlineSnapshot::Single => 1,
                TerminalUnderlineSnapshot::Double => 2,
                TerminalUnderlineSnapshot::Curly => 3,
                TerminalUnderlineSnapshot::Dotted => 4,
                TerminalUnderlineSnapshot::Dashed => 5,
            },
        }
    }
}

fn color(color: TerminalColor) -> pane::Color {
    match color {
        TerminalColor::Default => pane::Color::Default,
        TerminalColor::Palette(index) => pane::Color::Palette(index),
        TerminalColor::Rgb(color) => pane::Color::Rgb(rgb(color)),
    }
}

fn rgb(color: crate::appearance::Color) -> [u8; 3] {
    [color.r, color.g, color.b]
}

/// Style and link ids the client holds for one channel or one page.
#[derive(Default)]
struct Tables {
    styles: HashMap<Style, u16>,
    links: HashMap<Hyperlink, u16>,
    new_styles: Vec<StyleDefinition>,
    new_links: Vec<LinkDefinition>,
    /// A link did not fit its table.
    overflowed: bool,
}

impl Tables {
    /// The table's id for `style`, or the style itself once the table is full.
    fn style(&mut self, style: Style) -> StyleRef {
        if style == Style::DEFAULT {
            return StyleRef::Default;
        }
        if let Some(id) = self.styles.get(&style) {
            return StyleRef::Id(*id);
        }
        if self.styles.len() >= limits::MAX_STYLES {
            return StyleRef::Inline(style);
        }
        let id = self.styles.len() as u16 + 1;
        self.styles.insert(style, id);
        self.new_styles.push(StyleDefinition { id, style });
        StyleRef::Id(id)
    }

    fn link(&mut self, target: &HyperlinkTarget) -> Option<u16> {
        if target.value.len() > limits::MAX_LINK_BYTES {
            return None;
        }
        let link = match target.kind {
            HyperlinkKind::Url => Hyperlink::Url(target.value.clone()),
            HyperlinkKind::LocalPath => Hyperlink::File(target.value.clone()),
        };
        if let Some(id) = self.links.get(&link) {
            return Some(*id);
        }
        if self.links.len() >= limits::MAX_LINKS {
            self.overflowed = true;
            return None;
        }
        let id = self.links.len() as u16;
        self.links.insert(link.clone(), id);
        self.new_links.push(LinkDefinition { id, link });
        Some(id)
    }

    fn take_definitions(&mut self) -> (Vec<StyleDefinition>, Vec<LinkDefinition>) {
        (
            std::mem::take(&mut self.new_styles),
            std::mem::take(&mut self.new_links),
        )
    }
}

fn geometry(screen: &ScreenSnapshot) -> Geometry {
    let pixels = |value: u32| {
        u16::try_from(value)
            .ok()
            .filter(|value| (1..=256).contains(value))
    };
    Geometry {
        columns: screen.size.cols,
        rows: screen.rows.len() as u16,
        cell_pixels: pixels(screen.cell_pixels.0).zip(pixels(screen.cell_pixels.1)),
    }
}

fn modes(screen: &ScreenSnapshot) -> Modes {
    Modes {
        alternate_screen: screen.active_screen == ActiveScreenSnapshot::Alternate,
        mouse_tracking: screen.mouse_tracking,
        alternate_scroll: screen.alternate_scroll,
        reversed: screen.colors.reversed,
    }
}

fn colors(screen: &ScreenSnapshot) -> Colors {
    let overridden = |source| source == TerminalDefaultColorSource::ProgramOverride;
    let mut palette_overrides = [0u64; 4];
    for (index, overridden) in screen.colors.palette_overrides.iter().enumerate() {
        palette_overrides[index / 64] |= u64::from(*overridden) << (index % 64);
    }
    Colors {
        foreground: rgb(screen.colors.foreground),
        background: rgb(screen.colors.background),
        cursor: rgb(screen.cursor.color),
        cursor_text: rgb(screen.cursor.text_color),
        palette: screen.colors.palette.iter().copied().map(rgb).collect(),
        foreground_overridden: overridden(screen.colors.foreground_source),
        background_overridden: overridden(screen.colors.background_source),
        cursor_overridden: overridden(screen.colors.cursor_source),
        palette_overrides,
    }
}

fn cursor(screen: &ScreenSnapshot) -> Cursor {
    Cursor {
        position: screen.cursor.position.map(|position| CursorPosition {
            column: position.column,
            row: position.row,
            width: position.width_cells,
        }),
        visible: screen.cursor.visible,
        blinking: screen.cursor.blinking,
        shape: match screen.cursor.shape {
            CursorShapeSnapshot::Block => 0,
            CursorShapeSnapshot::Bar => 1,
            CursorShapeSnapshot::Underline => 2,
            CursorShapeSnapshot::BlockHollow => 3,
        },
    }
}
