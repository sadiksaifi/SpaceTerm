//! Predictive local echo for one claimed Pane, under ADR 0051.
//!
//! The Pane worker tells the [`Predictor`] what input it sent and under which serial, every
//! frame it applies and every line state the server pushes, in channel order. The predictor
//! places each printable key at the predicted cursor while the server reports the shell's line
//! editor, and keeps it until a frame whose `input` serial covers the key shows the server's
//! echo. Its [`Overlay`] marks the predictions the view draws over the server's screen; the
//! screen itself never changes.
//!
//! A prediction shows only once the server has confirmed its epoch: one prediction of the epoch
//! appeared in a frame where the cell held something else before. Enter, a control key and any
//! input that is not a printable key start a new epoch, as does every line state change. A
//! wrong prediction drops every prediction.
use super::ScreenSnapshot;
use crate::terminal::emulator::{
    ActiveScreenSnapshot, CursorPositionSnapshot, RowSnapshot, TerminalUnderlineSnapshot,
};
use spaceterm_server::pane::{InteractiveInput, Key, LineState, Request};
use std::collections::VecDeque;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Prediction turns on at this round trip, as in mosh and VS Code.
const ACTIVE_ROUND_TRIP: Duration = Duration::from_millis(30);
/// Prediction turns off below this round trip, so a round trip near the threshold does not
/// switch it on and off.
const INACTIVE_ROUND_TRIP: Duration = Duration::from_millis(20);
/// How many recent samples the round trip is the smallest of.
const ROUND_TRIP_SAMPLES: usize = 8;
/// How long a frame may cover a key without showing its echo before its prediction is wrong.
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
        let Some(estimate) = self.samples.iter().min().copied() else {
            return;
        };
        if estimate >= ACTIVE_ROUND_TRIP {
            self.active = true;
        } else if estimate < INACTIVE_ROUND_TRIP {
            self.active = false;
        }
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
    shown: bool,
    /// When a frame first covered the key without showing its echo.
    covered_at: Option<Instant>,
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
    /// Records `request`, sent with Input serials up to `serial`. A printable key is predicted
    /// when everything allows it; anything else suspends prediction.
    pub(super) fn sent(&mut self, request: &Request, serial: u32, now: Instant) {
        self.sent = self.sent.max(serial);
        typed(request, |typed| {
            if self.timed.back().is_none_or(|(timed, _)| *timed != serial) {
                if self.timed.len() == MAX_TIMED {
                    self.timed.pop_front();
                }
                self.timed.push_back((serial, now));
            }
            match typed {
                Typed::Printable(text) if self.predict(serial, text) => {}
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
        while let Some(prediction) = self.predictions.front_mut() {
            if prediction.serial > self.covered {
                break;
            }
            let cell = cell(screen, prediction.row, prediction.column);
            if cell.is_some_and(|cell| is(cell, prediction.text)) {
                if prediction.epoch == self.epoch && !is(&prediction.original, prediction.text) {
                    self.confirm();
                }
                self.predictions.pop_front();
                continue;
            }
            let covered_at = *prediction.covered_at.get_or_insert(now);
            if cell == Some(&*prediction.original) && now < covered_at + ECHO_GRACE {
                break;
            }
            self.reset();
            return;
        }
    }

    /// Drops every prediction once a frame has covered one for longer than the echo grace
    /// without showing its echo.
    pub(super) fn expire(&mut self, now: Instant) {
        if self.deadline().is_some_and(|deadline| now >= deadline) {
            self.reset();
        }
    }

    /// When [`Self::expire`] next has work.
    pub(super) fn deadline(&self) -> Option<Instant> {
        self.predictions
            .iter()
            .filter_map(|prediction| prediction.covered_at)
            .min()
            .map(|covered_at| covered_at + ECHO_GRACE)
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
    fn predict(&mut self, serial: u32, text: char) -> bool {
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
        self.predictions.push_back(Prediction {
            serial,
            epoch: self.epoch,
            row,
            column,
            text,
            original,
            shown: self.confirmed,
            covered_at: None,
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
    /// skipped.
    pub(super) fn apply(&self, screen: &mut ScreenSnapshot) {
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
            cell.text = text.to_string();
            cell.underline = TerminalUnderlineSnapshot::Single;
            cell.invisible = false;
            cell.spacer_tail = false;
            cell.hyperlink = None;
            *cells = edited.into();
            drawn = true;
        }
        if !drawn {
            return;
        }
        screen.rows = rows.into();
        let (row, column) = self.cursor;
        if let Some(position) = &mut screen.cursor.position {
            position.row = row;
            position.column = column;
            position.width_cells = 1;
        }
    }
}

#[cfg(test)]
#[path = "prediction_tests.rs"]
pub(super) mod tests;
