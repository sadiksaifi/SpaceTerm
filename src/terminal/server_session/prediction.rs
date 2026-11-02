//! Predictive local echo for one claimed Pane, under ADR 0051.
//!
//! The Pane worker tells the [`Predictor`] what input it sent and under which serial, every
//! frame it applies and every line state the server pushes, in channel order. The predictor
//! places each printable key at the predicted cursor while the server reports the shell's line
//! editor, and keeps it until a frame whose `input` serial covers the key shows the server's
//! echo. Its [`Overlay`] marks the predictions the view draws over the server's screen; the
//! screen itself never changes.
//!
//! A prediction shows only once the server has confirmed its epoch: a frame showed one prediction
//! of the epoch echoed in a cell that held something else before, with the cursor right after it
//! and the row to its left as it was predicted. A program that reads silently in the line
//! editor's mode, such as bash's `read -rsn4` or zsh's `read -sk4`, never echoes. Each
//! prediction's echo is due one round trip estimate and [`ECHO_GRACE`] after its key was sent,
//! with or without a frame; an overdue echo drops every prediction, so keys read silently after
//! a confirmed echo show only until then. Enter, a control key and any input that is not a
//! printable key start a new epoch, as does every line state change. A wrong prediction or a
//! discarded key drops every prediction.
use super::ScreenSnapshot;
use crate::terminal::emulator::{
    ActiveScreenSnapshot, CellText, CursorPositionSnapshot, RowSnapshot, TerminalUnderlineSnapshot,
};
use spaceterm_server::pane::{InteractiveInput, Key, LineState, Request};
use std::collections::VecDeque;
use std::hash::{Hash, Hasher};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Prediction turns on at this round trip, as in mosh and VS Code.
const ACTIVE_ROUND_TRIP: Duration = Duration::from_millis(30);
/// Prediction turns off below this round trip, so a round trip near the threshold does not
/// switch it on and off.
const INACTIVE_ROUND_TRIP: Duration = Duration::from_millis(20);
/// How many recent samples the round trip is the smallest of.
const ROUND_TRIP_SAMPLES: usize = 8;
/// How much longer than the round trip estimate a key may go without its echo before its
/// prediction is wrong.
const ECHO_GRACE: Duration = Duration::from_millis(100);
/// Keys whose round trip is not measured yet. Older ones are forgotten.
const MAX_TIMED: usize = 64;
/// Predictions outstanding at once. A row is never wider than this.
const MAX_PREDICTIONS: usize = spaceterm_server::pane::MAX_COLUMNS as usize;

/// Modifier bits of [`Key::modifiers`] that make a key a command rather than text: Alt, Control
/// and the platform key, left and right.
const COMMAND_MODIFIERS: u16 = (1 << 1) | (1 << 2) | (1 << 3) | (1 << 7) | (1 << 8) | (1 << 9);
const KEY_RELEASE: u8 = 2;

/// What one sent request wrote to the PTY, as prediction sees it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Typed {
    /// A printable ASCII character with no command modifier.
    Printable(char),
    /// Anything else that reaches the program: Enter, a control or navigation key, a paste,
    /// pointer input, a resize. The program may move the cursor or stop reading its line.
    Other,
}

fn typed(request: &Request, mut each: impl FnMut(Typed)) {
    match request {
        Request::Keys { keys, .. } => keys
            .iter()
            .filter(|key| key.action != KEY_RELEASE)
            .for_each(|key| each(classify(key))),
        Request::Input { input, .. } => match input {
            InteractiveInput::Key(key) if key.action == KEY_RELEASE => {}
            InteractiveInput::Key(key) => each(classify(key)),
            _ => each(Typed::Other),
        },
        Request::Resize { .. } | Request::ClearScreenAndScrollback { .. } => each(Typed::Other),
        _ => {}
    }
}

fn classify(key: &Key) -> Typed {
    let mut chars = key.text.as_deref().unwrap_or_default().chars();
    match (chars.next(), chars.next()) {
        (Some(character @ ' '..='~'), None) if key.modifiers & COMMAND_MODIFIERS == 0 => {
            Typed::Printable(character)
        }
        _ => Typed::Other,
    }
}

/// The round trip from sending a key to the first frame that covers it. Each sample is at least
/// one network round trip, so the smallest recent sample is the closest estimate; a key that
/// changed nothing on screen gives a long sample, which the minimum ignores.
#[derive(Default)]
struct RoundTrip {
    samples: VecDeque<Duration>,
    active: bool,
}

impl RoundTrip {
    fn sample(&mut self, sample: Duration) {
        if self.samples.len() == ROUND_TRIP_SAMPLES {
            self.samples.pop_front();
        }
        self.samples.push_back(sample);
        let Some(estimate) = self.estimate() else {
            return;
        };
        if estimate >= ACTIVE_ROUND_TRIP {
            self.active = true;
        } else if estimate < INACTIVE_ROUND_TRIP {
            self.active = false;
        }
    }

    fn estimate(&self) -> Option<Duration> {
        self.samples.iter().min().copied()
    }
}

struct Prediction {
    serial: u32,
    epoch: u64,
    row: u16,
    column: u16,
    text: char,
    /// What the server's cell held when the prediction was made.
    original: Box<str>,
    /// The row left of the prediction as its echo will show it: the server's cells with the
    /// earlier predictions on the row in place.
    prefix: u64,
    shown: bool,
    /// When the echo is overdue: one round trip estimate and [`ECHO_GRACE`] after the key was
    /// sent. A server sends no frame for a key that changes nothing on screen, so this holds
    /// whether or not frames arrive.
    due: Instant,
}

#[derive(Default)]
pub(super) struct Predictor {
    round_trip: RoundTrip,
    line: Option<LineState>,
    /// Sent keys waiting for a frame to measure the round trip, oldest first.
    timed: VecDeque<(u32, Instant)>,
    /// The highest Input serial sent.
    sent: u32,
    /// The highest Input serial a frame reflected.
    covered: u32,
    /// Prediction waits until a frame reflects this serial, so it starts from a screen that
    /// shows the input that suspended it.
    resume: Option<u32>,
    predictions: VecDeque<Prediction>,
    epoch: u64,
    /// The server's echo confirmed a prediction of the current epoch.
    confirmed: bool,
    screen: Option<Arc<ScreenSnapshot>>,
}

impl Predictor {
    /// Records `request`, which the worker began sending at `sent_at`. `serial` is the highest
    /// Input serial it took, or `None` when it took none, as a Scrollback clear does. A printable
    /// key is predicted when everything allows it; anything else suspends prediction.
    pub(super) fn sent(&mut self, request: &Request, serial: Option<u32>, sent_at: Instant) {
        if let Some(serial) = serial {
            self.sent = self.sent.max(serial);
        }
        typed(request, |typed| {
            let Some(serial) = serial else {
                // Input without a serial changes the screen in a way no frame identifies.
                self.reset();
                return;
            };
            if self.timed.back().is_none_or(|(timed, _)| *timed != serial) {
                if self.timed.len() == MAX_TIMED {
                    self.timed.pop_front();
                }
                self.timed.push_back((serial, sent_at));
            }
            match typed {
                Typed::Printable(text) if self.predict(serial, text, sent_at) => {}
                Typed::Printable(_) | Typed::Other => self.suspend(serial),
            }
        });
    }

    /// Applies a pushed line state. Any change drops the outstanding predictions.
    pub(super) fn line(&mut self, line: LineState) {
        let changed = self.line.is_some_and(|previous| previous != line);
        self.line = Some(line);
        if changed || !line.mode.line_editor() {
            self.reset();
        }
    }

    /// Applies the server screen of a frame that reflects Input serials up to `input`, and
    /// judges every prediction the frame covers.
    pub(super) fn frame(&mut self, screen: &Arc<ScreenSnapshot>, input: u32, now: Instant) {
        let mut newest = None;
        while let Some(&(serial, sent_at)) = self.timed.front()
            && serial <= input
        {
            newest = Some(sent_at);
            self.timed.pop_front();
        }
        if let Some(sent_at) = newest {
            let was_active = self.round_trip.active;
            self.round_trip
                .sample(now.saturating_duration_since(sent_at));
            if was_active && !self.round_trip.active {
                self.reset();
            }
        }
        self.covered = self.covered.max(input);
        self.screen = Some(Arc::clone(screen));
        if cursor(screen).is_none() {
            self.reset();
        }
        if self.resume.is_some_and(|serial| self.covered >= serial) {
            self.resume = None;
        }
        // Where the newest echoed prediction of the current epoch sits, if it may confirm it.
        let mut credit = None;
        while let Some(prediction) = self.predictions.front() {
            if prediction.serial > self.covered {
                break;
            }
            let cell = cell(screen, prediction.row, prediction.column);
            if cell.is_some_and(|cell| is(cell, prediction.text)) {
                credit = (prediction.epoch == self.epoch
                    && !is(&prediction.original, prediction.text)
                    && prefix(screen, prediction.row, prediction.column, &self.predictions)
                        == Some(prediction.prefix))
                .then_some((prediction.row, prediction.column));
                self.predictions.pop_front();
                continue;
            }
            // The echo may follow in a later frame until it is due.
            if cell == Some(&*prediction.original) && now < prediction.due {
                break;
            }
            self.reset();
            return;
        }
        // A prompt that happens to print the typed character where it was predicted leaves the
        // cursor after the prompt, and changes the row to its left.
        if let Some((row, column)) = credit
            && cursor(screen).is_some_and(|cursor| (cursor.row, cursor.column) == (row, column + 1))
        {
            self.confirm();
        }
    }

    /// Drops every prediction once one is overdue. A key that shows and never echoes may belong
    /// to a program that reads silently in the line editor's mode, so the new epoch shows
    /// nothing until the server's echo confirms it again.
    pub(super) fn expire(&mut self, now: Instant) {
        if self.deadline().is_some_and(|deadline| now >= deadline) {
            self.reset();
        }
    }

    /// When [`Self::expire`] next has work: only while predictions wait for their echo.
    pub(super) fn deadline(&self) -> Option<Instant> {
        self.predictions
            .iter()
            .map(|prediction| prediction.due)
            .min()
    }

    /// The server discarded Input `serial`. A prediction of it never echoes, so every
    /// prediction goes.
    pub(super) fn discarded(&mut self, serial: u32) {
        if self
            .predictions
            .iter()
            .any(|prediction| prediction.serial >= serial)
        {
            self.reset();
        }
    }

    /// The predictions the view draws, or `None` when none shows.
    pub(super) fn overlay(&self) -> Option<Overlay> {
        if !self.round_trip.active {
            return None;
        }
        let cells: Vec<_> = self
            .predictions
            .iter()
            .filter(|prediction| prediction.shown)
            .map(|prediction| (prediction.row, prediction.column, prediction.text))
            .collect();
        let &(row, column, _) = cells.last()?;
        Some(Overlay {
            cells,
            cursor: (row, column + 1),
        })
    }

    /// Places a printable key after the newest prediction, or at the server's cursor. Returns
    /// whether it was placed.
    fn predict(&mut self, serial: u32, text: char, sent_at: Instant) -> bool {
        let Some(estimate) = self.round_trip.estimate() else {
            return false;
        };
        if !self.round_trip.active
            || self.resume.is_some()
            || !self.line.is_some_and(|line| line.mode.line_editor())
            || self.predictions.len() >= MAX_PREDICTIONS
        {
            return false;
        }
        let Some(screen) = &self.screen else {
            return false;
        };
        let (row, column) = match self.predictions.back() {
            Some(last) => (last.row, last.column + 1),
            None => match cursor(screen) {
                Some(cursor) => (cursor.row, cursor.column),
                None => return false,
            },
        };
        // The last column wraps, which the line editor draws in its own way.
        let Some(original) = cell(screen, row, column + 1).and(cell(screen, row, column)) else {
            return false;
        };
        let original = original.into();
        let Some(prefix) = prefix(screen, row, column, &self.predictions) else {
            return false;
        };
        self.predictions.push_back(Prediction {
            serial,
            epoch: self.epoch,
            row,
            column,
            text,
            original,
            prefix,
            shown: self.confirmed,
            due: sent_at + estimate + ECHO_GRACE,
        });
        true
    }

    /// Starts a new epoch that shows nothing until the server confirms it, and waits for a
    /// frame that reflects `serial`.
    fn suspend(&mut self, serial: u32) {
        self.epoch += 1;
        self.confirmed = false;
        let serial = serial.max(self.resume.unwrap_or(0));
        self.resume = (serial > self.covered).then_some(serial);
    }

    fn confirm(&mut self) {
        self.confirmed = true;
        let epoch = self.epoch;
        for prediction in &mut self.predictions {
            prediction.shown |= prediction.epoch == epoch;
        }
    }

    /// Drops every prediction and suspends until a frame reflects all sent input.
    fn reset(&mut self) {
        self.predictions.clear();
        self.suspend(self.sent);
    }
}

/// The server's cursor, when a prediction can start there: on the primary screen, visible.
fn cursor(screen: &ScreenSnapshot) -> Option<CursorPositionSnapshot> {
    (screen.active_screen == ActiveScreenSnapshot::Primary && screen.cursor.visible)
        .then_some(screen.cursor.position)
        .flatten()
}

/// A digest of the row left of `column`: the server's cells, with the outstanding predictions
/// on the row in place of the cells they cover. A blank cell counts as a space.
fn prefix(
    screen: &ScreenSnapshot,
    row: u16,
    column: u16,
    predictions: &VecDeque<Prediction>,
) -> Option<u64> {
    let cells = screen.rows.get(usize::from(row))?;
    let mut digest = std::collections::hash_map::DefaultHasher::new();
    for (index, cell) in cells.iter().take(usize::from(column)).enumerate() {
        let predicted = predictions
            .iter()
            .find(|prediction| prediction.row == row && usize::from(prediction.column) == index);
        let mut buffer = [0; 4];
        let text = match predicted {
            Some(prediction) => &*prediction.text.encode_utf8(&mut buffer),
            None if cell.spacer_tail => continue,
            None if cell.text.is_empty() => " ",
            None => cell.text.as_str(),
        };
        text.hash(&mut digest);
    }
    Some(digest.finish())
}

fn cell(screen: &ScreenSnapshot, row: u16, column: u16) -> Option<&str> {
    let cell = screen
        .rows
        .get(usize::from(row))?
        .get(usize::from(column))?;
    (!cell.spacer_tail).then_some(cell.text.as_str())
}

/// Whether a cell's text is `text`. A blank cell may hold no text or a space.
fn is(cell: &str, text: char) -> bool {
    let mut chars = cell.chars();
    chars.next() == Some(text) && chars.next().is_none()
}

/// The predictions a view draws over the server's screen, underlined until confirmed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Overlay {
    /// Row, column and character of each shown prediction, in typing order.
    cells: Vec<(u16, u16, char)>,
    /// Where the cursor shows: after the newest shown prediction.
    cursor: (u16, u16),
}

impl Overlay {
    /// Draws the predictions into `screen`, a copy the view presents. Cells off the screen are
    /// skipped. Returns whether it drew any.
    pub(super) fn apply(&self, screen: &mut ScreenSnapshot) -> bool {
        let mut rows: Vec<RowSnapshot> = screen.rows.to_vec();
        let mut drawn = false;
        for &(row, column, text) in &self.cells {
            let Some(cells) = rows.get_mut(usize::from(row)) else {
                continue;
            };
            let mut edited = cells.to_vec();
            let Some(cell) = edited.get_mut(usize::from(column)) else {
                continue;
            };
            cell.text = CellText::new(text.encode_utf8(&mut [0; 4]));
            cell.underline = TerminalUnderlineSnapshot::Single;
            cell.invisible = false;
            cell.spacer_tail = false;
            cell.hyperlink = None;
            *cells = edited.into();
            drawn = true;
        }
        if !drawn {
            return false;
        }
        screen.rows = rows.into();
        let (row, column) = self.cursor;
        if let Some(position) = &mut screen.cursor.position {
            position.row = row;
            position.column = column;
            position.width_cells = 1;
        }
        true
    }
}

#[cfg(test)]
#[path = "prediction_tests.rs"]
mod tests;

#[cfg(all(
    test,
    feature = "native-tests",
    any(target_os = "macos", target_os = "linux")
))]
#[path = "prediction_unix_tests.rs"]
mod unix_tests;
