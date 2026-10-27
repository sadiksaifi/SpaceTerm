//! Selection gestures and copying over cached history and the visible screen.
//!
//! The gestures follow Ghostty's selection gesture, which the server ran before this cache: a
//! single press clears the Selection and a drag selects cells past a 60% threshold, a double
//! press selects words and a triple press selects lines, each extended by dragging. A drag past
//! the top or bottom edge scrolls one row per autoscroll tick. Rows that are not cached end words
//! and lines; copying waits until every selected row is cached.
use super::{CopyOutcome, Lines, Point, row::RowRef};
use crate::terminal::selection::{SelectionCopy, SelectionCopyOptions};
use crate::terminal::{ScreenSnapshot, TerminalColor};
use std::fmt::Write as _;
use std::ops::Range;
use std::time::{Duration, Instant};

const REPEAT_CLICK_DISTANCE: f32 = 5.0;
const REPEAT_CLICK_INTERVAL: Duration = Duration::from_millis(500);
/// Ghostty's default word boundaries for double-click selection.
const WORD_BOUNDARIES: [char; 20] = [
    '\0', ' ', '\t', '\'', '"', '│', '`', '|', ':', ';', ',', '(', ')', '[', ']', '{', '}', '<',
    '>', '$',
];

/// A left-button pointer event in surface pixels of the Pane's grid.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum PointerEvent {
    Press { x: f32, y: f32 },
    Drag { x: f32, y: f32 },
    Release,
}

/// The presented grid, in the same pixel units as [`PointerEvent`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Grid {
    pub(crate) columns: u16,
    pub(crate) rows: u16,
    pub(crate) cell_width: f32,
    pub(crate) cell_height: f32,
}

impl Grid {
    fn cell(self, x: f32, y: f32) -> (u16, u16) {
        let index = |position: f32, size: f32, count: u16| {
            if !position.is_finite() || size <= 0.0 || position <= 0.0 {
                return 0;
            }
            ((position / size) as u32).min(u32::from(count.saturating_sub(1))) as u16
        };
        (
            index(x, self.cell_width, self.columns),
            index(y, self.cell_height, self.rows),
        )
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Behavior {
    #[default]
    Cell,
    Word,
    Line,
}

#[derive(Default)]
struct Gesture {
    clicks: u8,
    behavior: Behavior,
    /// The cell of the press that started the click sequence.
    anchor: Option<Point>,
    press: (f32, f32),
    pressed_at: Option<Instant>,
    /// The button is down and the latest drag position, if it moved.
    held: bool,
    at: Option<(f32, f32)>,
    autoscroll: i64,
}

#[derive(Default)]
pub(super) struct LocalSelection {
    /// Ordered endpoints, both included.
    range: Option<(Point, Point)>,
    revision: u64,
    gesture: Gesture,
}

impl LocalSelection {
    pub(super) fn range(&self) -> Option<(Point, Point)> {
        self.range
    }

    /// Increases whenever the Selection changes.
    pub(super) fn revision(&self) -> u64 {
        self.revision
    }

    pub(super) fn clear(&mut self) -> bool {
        self.gesture = Gesture::default();
        self.set(None)
    }

    /// Keeps the Selection on readable lines after the oldest rows were discarded.
    pub(super) fn observe(&mut self, readable: Range<u64>) {
        let Some((start, end)) = self.range else {
            return;
        };
        if end.line < readable.start || readable.is_empty() {
            self.clear();
        } else if start.line < readable.start {
            self.set(Some((
                Point {
                    line: readable.start,
                    column: 0,
                },
                end,
            )));
        }
        if self
            .gesture
            .anchor
            .is_some_and(|anchor| anchor.line < readable.start)
        {
            self.gesture = Gesture::default();
        }
    }

    pub(super) fn pointer(
        &mut self,
        event: PointerEvent,
        grid: Grid,
        now: Instant,
        lines: Lines<'_>,
        top: u64,
    ) -> bool {
        match event {
            PointerEvent::Press { x, y } => self.press(x, y, grid, now, lines, top),
            PointerEvent::Drag { x, y } => {
                if !self.gesture.held {
                    return false;
                }
                self.gesture.at = Some((x, y));
                let height = f32::from(grid.rows) * grid.cell_height;
                self.gesture.autoscroll = if y <= 1.0 {
                    -1
                } else if y > height - 1.0 {
                    1
                } else {
                    0
                };
                self.continue_drag(grid, lines, top)
            }
            PointerEvent::Release => {
                self.gesture.held = false;
                self.gesture.at = None;
                self.gesture.autoscroll = 0;
                false
            }
        }
    }

    pub(super) fn cancel_gesture(&mut self) {
        self.gesture = Gesture::default();
    }

    pub(super) fn autoscrolling(&self) -> bool {
        self.gesture.held && self.gesture.autoscroll != 0 && self.gesture.at.is_some()
    }

    /// The rows one autoscroll tick moves the view.
    pub(super) fn autoscroll_lines(&self) -> Option<i64> {
        self.autoscrolling().then_some(self.gesture.autoscroll)
    }

    /// Extends the Selection to the latest drag position against the view's current top.
    pub(super) fn continue_drag(&mut self, grid: Grid, lines: Lines<'_>, top: u64) -> bool {
        let (Some(anchor), Some((x, y))) = (self.gesture.anchor, self.gesture.at) else {
            return false;
        };
        let Some(point) = point_at(lines, top, grid, x, y) else {
            return false;
        };
        let selection = match self.gesture.behavior {
            Behavior::Cell => drag_cells(lines, grid, anchor, point, self.gesture.press.0, x),
            Behavior::Word => drag_words(lines, anchor, point),
            Behavior::Line => drag_lines(lines, anchor, point),
        };
        self.set(selection)
    }

    fn press(
        &mut self,
        x: f32,
        y: f32,
        grid: Grid,
        now: Instant,
        lines: Lines<'_>,
        top: u64,
    ) -> bool {
        if self.gesture.held {
            return false;
        }
        let Some(point) = point_at(lines, top, grid, x, y) else {
            return false;
        };
        let gesture = &mut self.gesture;
        let repeat = gesture.clicks > 0
            && gesture.anchor.is_some()
            && gesture.pressed_at.is_some_and(|previous| {
                now.checked_duration_since(previous)
                    .is_some_and(|since| since <= REPEAT_CLICK_INTERVAL)
            })
            && (x - gesture.press.0).hypot(y - gesture.press.1) <= REPEAT_CLICK_DISTANCE;
        if repeat {
            gesture.clicks = (gesture.clicks + 1).min(3);
        } else {
            gesture.clicks = 1;
            gesture.anchor = Some(point);
            gesture.press = (x, y);
        }
        gesture.pressed_at = Some(now);
        gesture.held = true;
        gesture.at = None;
        gesture.autoscroll = 0;
        gesture.behavior = match gesture.clicks {
            1 => Behavior::Cell,
            2 => Behavior::Word,
            _ => Behavior::Line,
        };
        let selection = match gesture.behavior {
            Behavior::Cell => None,
            Behavior::Word => word(lines, point),
            Behavior::Line => line(lines, point, true),
        };
        self.set(selection)
    }

    fn set(&mut self, range: Option<(Point, Point)>) -> bool {
        let range = range.map(|(a, b)| (a.min(b), a.max(b)));
        if range == self.range {
            return false;
        }
        self.range = range;
        self.revision += 1;
        true
    }

    /// Copies the selected text as plain text and HTML.
    pub(super) fn copy(
        &self,
        lines: Lines<'_>,
        screen: &ScreenSnapshot,
        options: SelectionCopyOptions,
    ) -> CopyOutcome {
        let Some((start, end)) = self.range else {
            return CopyOutcome::Nothing;
        };
        if (start.line..=end.line).any(|line| lines.get(line).is_none()) {
            return CopyOutcome::Pending;
        }
        let mut plain = Text::default();
        let mut html = options.include_html.then(|| Html::new(screen));
        for line in start.line..=end.line {
            let Some(row) = lines.get(line) else {
                return CopyOutcome::Pending;
            };
            let first = if line == start.line { start.column } else { 0 };
            let last = if line == end.line {
                end.column
            } else {
                row.columns().saturating_sub(1)
            };
            for column in first..=last.min(row.columns().saturating_sub(1)) {
                if row.spacer_tail(column) {
                    continue;
                }
                if row.has_text(column) {
                    plain.text(row.text(column));
                    if let Some(html) = &mut html {
                        html.text(row, column);
                    }
                } else {
                    plain.blank();
                    if let Some(html) = &mut html {
                        html.blank();
                    }
                }
            }
            let joined = line != end.line && row.wrapped() && options.unwrap_soft_wraps;
            if !joined {
                plain.line_end();
                if let Some(html) = &mut html {
                    html.line_end();
                }
            }
        }
        if plain.output.is_empty() {
            return CopyOutcome::Nothing;
        }
        CopyOutcome::Ready(SelectionCopy {
            plain_text: plain.output.into(),
            html: html.map(Html::finish),
        })
    }
}

/// The cell under a pointer position. A wide character's spacer resolves to the character.
fn point_at(lines: Lines<'_>, top: u64, grid: Grid, x: f32, y: f32) -> Option<Point> {
    let (column, row) = grid.cell(x, y);
    let line = top + u64::from(row);
    let column = match lines.get(line) {
        Some(found) if found.spacer_tail(column) => column.saturating_sub(1),
        _ => column,
    };
    lines
        .range()
        .contains(&line)
        .then_some(Point { line, column })
}

/// Ghostty's cell drag: the pressed and current cells count once the pointer passes 60% of
/// their width in the drag's direction.
fn drag_cells(
    lines: Lines<'_>,
    grid: Grid,
    click: Point,
    drag: Point,
    click_x: f32,
    drag_x: f32,
) -> Option<(Point, Point)> {
    if grid.columns == 0 || grid.cell_width <= 0.0 {
        return None;
    }
    let threshold = (grid.cell_width * 0.6).round();
    let max_x = f32::from(grid.columns) * grid.cell_width - 1.0;
    let fraction = |x: f32| x.clamp(0.0, max_x).floor() % grid.cell_width.floor().max(1.0);
    let (click_fraction, drag_fraction) = (fraction(click_x), fraction(drag_x));
    let same = click == drag;
    let backwards = if same {
        drag_fraction < click_fraction
    } else {
        drag < click
    };
    let include_click = if backwards {
        click_fraction >= threshold
    } else {
        click_fraction < threshold
    };
    let include_drag = if backwards {
        drag_fraction < threshold
    } else {
        drag_fraction >= threshold
    };
    let left = |point: Point| step_left(lines, grid, point).unwrap_or(point);
    let right = |point: Point| step_right(lines, grid, point).unwrap_or(point);
    let start = match (include_click, backwards) {
        (true, _) => click,
        (false, true) => left(click),
        (false, false) => right(click),
    };
    let end = match (include_drag, backwards) {
        (true, _) => drag,
        (false, true) => right(drag),
        (false, false) => left(drag),
    };
    if (!include_click && (same || end == click)) || (!include_drag && start == drag) {
        return None;
    }
    Some((start, end))
}

fn step_left(lines: Lines<'_>, grid: Grid, point: Point) -> Option<Point> {
    if point.column > 0 {
        return Some(Point {
            column: point.column - 1,
            ..point
        });
    }
    (point.line > lines.range().start).then(|| Point {
        line: point.line - 1,
        column: grid.columns.saturating_sub(1),
    })
}

fn step_right(lines: Lines<'_>, grid: Grid, point: Point) -> Option<Point> {
    if point.column + 1 < grid.columns {
        return Some(Point {
            column: point.column + 1,
            ..point
        });
    }
    (point.line + 1 < lines.range().end).then(|| Point {
        line: point.line + 1,
        column: 0,
    })
}

/// The next cell of the same logical line, skipping wide-character spacers.
fn next_cell(lines: Lines<'_>, point: Point) -> Option<Point> {
    let row = lines.get(point.line)?;
    let mut column = point.column + 1;
    while column < row.columns() && row.spacer_tail(column) {
        column += 1;
    }
    if column < row.columns() {
        return Some(Point { column, ..point });
    }
    let line = point.line + 1;
    (row.wrapped() && lines.get(line).is_some()).then_some(Point { line, column: 0 })
}

/// The previous cell of the same logical line, skipping wide-character spacers.
fn previous_cell(lines: Lines<'_>, point: Point) -> Option<Point> {
    let (line, mut column) = if point.column > 0 {
        (point.line, point.column - 1)
    } else {
        let line = point.line.checked_sub(1)?;
        let row = lines.get(line).filter(|row| row.wrapped())?;
        (line, row.columns().checked_sub(1)?)
    };
    let row = lines.get(line)?;
    while column > 0 && row.spacer_tail(column) {
        column -= 1;
    }
    Some(Point { line, column })
}

fn boundary(text: &str) -> bool {
    text.chars()
        .next()
        .is_some_and(|first| WORD_BOUNDARIES.contains(&first))
}

/// The run of word characters, or of boundary characters, under `point`.
fn word(lines: Lines<'_>, point: Point) -> Option<(Point, Point)> {
    let row = lines.get(point.line)?;
    if !row.has_text(point.column) {
        return None;
    }
    let class = boundary(row.text(point.column));
    let same = |candidate: Point| {
        lines.get(candidate.line).is_some_and(|row| {
            row.has_text(candidate.column) && boundary(row.text(candidate.column)) == class
        })
    };
    let mut end = point;
    while let Some(next) = next_cell(lines, end).filter(|next| same(*next)) {
        end = next;
    }
    let mut start = point;
    while let Some(previous) = previous_cell(lines, start).filter(|previous| same(*previous)) {
        start = previous;
    }
    Some((start, end))
}

/// The word nearest `from` among the cells from `from` toward `to`, both included.
fn word_between(lines: Lines<'_>, from: Point, to: Point) -> Option<(Point, Point)> {
    let mut point = from;
    loop {
        if let Some(found) = word(lines, point) {
            return Some(found);
        }
        if point == to {
            return None;
        }
        point = if from < to {
            next_cell(lines, point).or_else(|| {
                let line = point.line + 1;
                (line <= to.line).then_some(Point { line, column: 0 })
            })?
        } else {
            previous_cell(lines, point).or_else(|| {
                let line = point.line.checked_sub(1)?;
                (line >= to.line).then(|| Point {
                    line,
                    column: lines
                        .get(line)
                        .map_or(0, |row| row.columns().saturating_sub(1)),
                })
            })?
        };
        if (from < to && to < point) || (to < from && point < to) {
            return None;
        }
    }
}

fn drag_words(lines: Lines<'_>, click: Point, drag: Point) -> Option<(Point, Point)> {
    let at_click = word_between(lines, click, drag)?;
    let at_drag = word_between(lines, drag, click)?;
    Some(if drag < click {
        (at_drag.0, at_click.1)
    } else {
        (at_click.0, at_drag.1)
    })
}

/// The logical line under `point`, without its leading and trailing whitespace when `trim`.
fn line(lines: Lines<'_>, point: Point, trim: bool) -> Option<(Point, Point)> {
    lines.get(point.line)?;
    let mut first = point.line;
    while let Some(above) = first.checked_sub(1)
        && lines.get(above).is_some_and(|row| row.wrapped())
    {
        first = above;
    }
    let mut last = point.line;
    while lines.get(last).is_some_and(|row| row.wrapped()) && lines.get(last + 1).is_some() {
        last += 1;
    }
    let start = Point {
        line: first,
        column: 0,
    };
    let end = Point {
        line: last,
        column: lines.get(last)?.columns().saturating_sub(1),
    };
    if !trim {
        return Some((start, end));
    }
    let content = |candidate: &Point| {
        lines
            .get(candidate.line)
            .is_some_and(|row| row.has_text(candidate.column) && row.text(candidate.column) != "\t")
    };
    let cells = (first..=last).flat_map(|line| {
        let columns = lines.get(line).map_or(0, RowRef::columns);
        (0..columns).map(move |column| Point { line, column })
    });
    let trimmed_start = cells.clone().find(content)?;
    let trimmed_end = cells.clone().rfind(content)?;
    Some((trimmed_start, trimmed_end))
}

fn drag_lines(lines: Lines<'_>, click: Point, drag: Point) -> Option<(Point, Point)> {
    let current = line(lines, drag, true)?;
    let (start, end) = line(lines, click, true).or_else(|| line(lines, click, false))?;
    Some(if drag < click {
        (current.0, end)
    } else {
        (start, current.1)
    })
}

/// Plain text as Ghostty formats a Selection: blanks between text become spaces, trailing
/// blanks and trailing blank lines are dropped.
#[derive(Default)]
struct Text {
    output: String,
    blanks: usize,
    newlines: usize,
}

impl Text {
    fn text(&mut self, text: &str) {
        if !self.output.is_empty() || self.newlines > 0 {
            self.output.extend(std::iter::repeat_n('\n', self.newlines));
        }
        self.newlines = 0;
        self.output.extend(std::iter::repeat_n(' ', self.blanks));
        self.blanks = 0;
        self.output.push_str(text);
    }

    fn blank(&mut self) {
        self.blanks += 1;
    }

    fn line_end(&mut self) {
        self.blanks = 0;
        self.newlines += 1;
    }
}

/// HTML in the shape of Ghostty's formatter, with palette colors resolved to RGB.
struct Html<'a> {
    screen: &'a ScreenSnapshot,
    text: Text,
    style: Option<String>,
}

impl<'a> Html<'a> {
    fn new(screen: &'a ScreenSnapshot) -> Self {
        Self {
            screen,
            text: Text {
                output: String::from("<div style=\"font-family: monospace; white-space: pre;\">"),
                ..Text::default()
            },
            style: None,
        }
    }

    fn text(&mut self, row: RowRef<'_>, column: u16) {
        let css = row
            .style(column)
            .map(|cell| self.css(cell))
            .unwrap_or_default();
        let style = (!css.is_empty()).then_some(css);
        // Pending spaces and line breaks belong to the style already open.
        let mut pending = String::new();
        pending.extend(std::iter::repeat_n('\n', self.text.newlines));
        pending.extend(std::iter::repeat_n(' ', self.text.blanks));
        self.text.newlines = 0;
        self.text.blanks = 0;
        self.text.output.push_str(&pending);
        if style != self.style {
            if self.style.take().is_some() {
                self.text.output.push_str("</div>");
            }
            if let Some(css) = &style {
                let _ = write!(self.text.output, "<div style=\"display: inline;{css}\">");
            }
            self.style = style;
        }
        for character in row.text(column).chars() {
            match character {
                '<' => self.text.output.push_str("&lt;"),
                '>' => self.text.output.push_str("&gt;"),
                '&' => self.text.output.push_str("&amp;"),
                '"' => self.text.output.push_str("&quot;"),
                '\'' => self.text.output.push_str("&#39;"),
                character if character.is_ascii() => self.text.output.push(character),
                character => {
                    let _ = write!(self.text.output, "&#{};", u32::from(character));
                }
            }
        }
    }

    fn blank(&mut self) {
        self.text.blank();
    }

    fn line_end(&mut self) {
        self.text.line_end();
    }

    fn finish(mut self) -> std::sync::Arc<str> {
        if self.style.take().is_some() {
            self.text.output.push_str("</div>");
        }
        self.text.output.push_str("</div>");
        self.text.output.into()
    }

    fn css(&self, cell: &crate::terminal::CellSnapshot) -> String {
        use crate::terminal::TerminalUnderlineSnapshot as Underline;
        let mut css = String::new();
        let mut color = |property: &str, source: TerminalColor| {
            let color = match source {
                TerminalColor::Default => return,
                TerminalColor::Palette(index) => self.screen.colors.palette[usize::from(index)],
                TerminalColor::Rgb(color) => color,
            };
            let _ = write!(
                css,
                "{property}: rgb({}, {}, {});",
                color.r, color.g, color.b
            );
        };
        color("color", cell.foreground_source);
        color("background-color", cell.background_source);
        color("text-decoration-color", cell.underline_source);
        let underline = cell.underline != Underline::None;
        if underline || cell.strikethrough || cell.overline || cell.blinking {
            css.push_str("text-decoration-line:");
            for (present, name) in [
                (underline, " underline"),
                (cell.strikethrough, " line-through"),
                (cell.overline, " overline"),
                (cell.blinking, " blink"),
            ] {
                if present {
                    css.push_str(name);
                }
            }
            css.push(';');
        }
        css.push_str(match cell.underline {
            Underline::None => "",
            Underline::Single => "text-decoration-style: solid;",
            Underline::Double => "text-decoration-style: double;",
            Underline::Curly => "text-decoration-style: wavy;",
            Underline::Dotted => "text-decoration-style: dotted;",
            Underline::Dashed => "text-decoration-style: dashed;",
        });
        for (present, declaration) in [
            (cell.bold, "font-weight: bold;"),
            (cell.italic, "font-style: italic;"),
            (cell.faint, "opacity: 0.5;"),
            (cell.invisible, "visibility: hidden;"),
            (cell.inverse, "filter: invert(100%);"),
        ] {
            if present {
                css.push_str(declaration);
            }
        }
        css
    }
}
