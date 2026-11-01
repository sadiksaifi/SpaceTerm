//! Encodes screen snapshots as row style runs against the frame the client holds.
//!
//! The emulator keeps an unchanged row's `Arc` across snapshots, so pointer equality with the
//! baseline is the per-row damage. A row that moved is a new `Arc` with the old cells, so each
//! snapshot row is hashed and a scroll is found by matching those hashes against the baseline's
//! before any row is encoded. A row is encoded only when its cells differ from the baseline row
//! the client shows at its position after the scroll.
use super::*;
use spaceterm_server::pane::effects::Hyperlink;
use spaceterm_server::protocol::limits;
use spaceterm_server::protocol::screen::{
    self as wire, Cells, Colors, Cursor, CursorPosition, Geometry, History, LinkDefinition, Modes,
    Page, Run, ScreenUpdate, ScrollShift, Style, StyleDefinition, StyleRef, flags,
};
use std::cell::OnceCell;
use std::collections::HashMap;
use std::hash::{DefaultHasher, Hash, Hasher};

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

/// One row as encoded, with the snapshot row it came from and that row's [`Fingerprint`] hash.
struct Encoded {
    source: RowSnapshot,
    hash: u64,
    line: Arc<Line>,
}

/// One snapshot row before encoding.
struct Pending<'a> {
    source: &'a RowSnapshot,
    wrapped: bool,
    fingerprint: OnceCell<Fingerprint>,
}

/// A hash of a row's cell text and styles, and whether every cell is blank. Links and cell
/// metadata are left out of the hash, so equal hashes only suggest equal rows.
#[derive(Clone, Copy)]
struct Fingerprint {
    hash: u64,
    blank: bool,
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
        let pending = screen
            .rows
            .iter()
            .enumerate()
            .map(|(index, source)| Pending {
                source,
                wrapped: screen.row_soft_wrapped.get(index).copied().unwrap_or(false),
                fingerprint: OnceCell::new(),
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
            let (line, hash) = match shown {
                Some(old) if new.shown_by(Some(old)) => (Arc::clone(&old.line), old.hash),
                None if new.shown_by(None) => (Arc::clone(&blank), new.fingerprint().hash),
                _ => {
                    #[cfg(test)]
                    {
                        self.encoded_rows += 1;
                    }
                    let line = Line::encode(new.source, new.wrapped, &mut self.tables);
                    (Arc::new(line), new.fingerprint().hash)
                }
            };
            changed.push(mode == Mode::Replace || line != *shown_line);
            rows.push(Encoded {
                source: Arc::clone(new.source),
                hash,
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
            && !row.fingerprint().blank
            && let Some(source) = positions.get(&row.fingerprint().hash)
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
    fn fingerprint(&self) -> Fingerprint {
        *self
            .fingerprint
            .get_or_init(|| Fingerprint::of(self.source))
    }

    /// Whether this row probably equals `old`, a blank row when `None`. Scroll detection uses
    /// this, so it hashes each row once and compares no cells.
    fn resembles(&self, old: Option<&Encoded>) -> bool {
        match old {
            Some(old) => {
                old.line.wrapped == self.wrapped
                    && (Arc::ptr_eq(&old.source, self.source)
                        || old.hash == self.fingerprint().hash)
            }
            None => !self.wrapped && self.fingerprint().blank,
        }
    }

    /// Whether the client already shows this row where it shows `old`, a blank row when `None`.
    fn shown_by(&self, old: Option<&Encoded>) -> bool {
        match old {
            Some(old) => {
                old.line.wrapped == self.wrapped
                    && (Arc::ptr_eq(&old.source, self.source)
                        || (old.hash == self.fingerprint().hash && old.source == *self.source))
            }
            None => self.resembles(None),
        }
    }
}

impl Fingerprint {
    fn of(cells: &[CellSnapshot]) -> Self {
        let mut hasher = DefaultHasher::new();
        let mut blank = true;
        for cell in cells {
            text(cell).hash(&mut hasher);
            style(cell).hash(&mut hasher);
            blank &= self::blank(cell);
        }
        Self {
            hash: hasher.finish(),
            blank,
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
        let end = cells
            .iter()
            .rposition(|cell| !blank(cell))
            .map_or(0, |last| last + 1);
        let mut runs = Vec::<Run>::new();
        let mut lengths = Vec::<u16>::new();
        let mut narrow = true;
        // Neighboring cells usually share a style and a link, so table lookups follow changes.
        let mut last_style: Option<(Style, StyleRef)> = None;
        let mut last_link: Option<(&Arc<HyperlinkTarget>, Option<u16>)> = None;
        for cell in &cells[..end] {
            let style = style(cell);
            let style_id = match last_style {
                Some((last, id)) if last == style => id,
                _ => tables.style(style),
            };
            last_style = Some((style, style_id));
            let link = match (&cell.hyperlink, last_link) {
                (None, _) => None,
                (Some(target), Some((last, id))) if Arc::ptr_eq(target, last) => id,
                (Some(target), _) => {
                    let id = tables.link(target);
                    last_link = Some((target, id));
                    id
                }
            };
            if runs
                .last()
                .is_none_or(|run| run.style != style_id || run.link != link)
            {
                finish(runs.last_mut(), &mut lengths, narrow);
                narrow = true;
                runs.push(Run {
                    style: style_id,
                    link,
                    text: String::new(),
                    cells: Cells::Narrow,
                });
            }
            let text = text(cell);
            narrow &= text.chars().nth(1).is_none() && !text.is_empty();
            lengths.push(text.len() as u16);
            if let Some(run) = runs.last_mut() {
                run.text.push_str(text);
            }
        }
        finish(runs.last_mut(), &mut lengths, narrow);
        Self { wrapped, runs }
    }
}

fn finish(run: Option<&mut Run>, lengths: &mut Vec<u16>, narrow: bool) {
    if let Some(run) = run
        && !narrow
    {
        run.cells = Cells::Explicit(std::mem::take(lengths));
    }
    lengths.clear();
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
        && style(cell) == Style::DEFAULT
}

fn style(cell: &CellSnapshot) -> Style {
    let flag = |set: bool, flag: u16| if set { flag } else { 0 };
    Style {
        foreground: color(cell.foreground_source),
        background: color(cell.background_source),
        underline_color: color(cell.underline_source),
        flags: flag(cell.inverse, flags::INVERSE)
            | flag(cell.bold, flags::BOLD)
            | flag(cell.faint, flags::FAINT)
            | flag(cell.italic, flags::ITALIC)
            | flag(cell.blinking, flags::BLINKING)
            | flag(cell.invisible, flags::INVISIBLE)
            | flag(cell.strikethrough, flags::STRIKETHROUGH)
            | flag(cell.overline, flags::OVERLINE)
            | flag(cell.selected, flags::SELECTED),
        underline: match cell.underline {
            TerminalUnderlineSnapshot::None => 0,
            TerminalUnderlineSnapshot::Single => 1,
            TerminalUnderlineSnapshot::Double => 2,
            TerminalUnderlineSnapshot::Curly => 3,
            TerminalUnderlineSnapshot::Dotted => 4,
            TerminalUnderlineSnapshot::Dashed => 5,
        },
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
