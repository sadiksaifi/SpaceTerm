//! Deterministic tests for predictive local echo. Each test drives the predictor with the
//! requests, frames and line states a Pane worker would see, at fixed instants, and checks what
//! the view would show.
use super::*;
use crate::terminal::PresentationGeneration;
use crate::terminal::emulator::CellSemanticSnapshot;
use crate::terminal::server_session::reading::Reading;
use crate::terminal::{CellSnapshot, TerminalColor};
use spaceterm_server::pane::{self, Lease, LineMode};

const COLUMNS: u16 = 12;
const ROUND_TRIP: Duration = Duration::from_millis(80);
pub(super) const LEASE: Lease = Lease {
    server: [1; 32],
    session: [2; 32],
    control: [3; 32],
    connection: [4; 32],
};
const SHELL: LineMode = LineMode {
    canonical: false,
    echo: false,
    signals: true,
    shell_foreground: true,
};
const PASSWORD: LineMode = LineMode {
    canonical: true,
    echo: false,
    signals: true,
    shell_foreground: false,
};
const RAW: LineMode = LineMode {
    canonical: false,
    echo: false,
    signals: false,
    shell_foreground: false,
};
const JOB: LineMode = LineMode {
    shell_foreground: false,
    ..SHELL
};
const CONTROL: u16 = 1 << 2;

fn blank() -> CellSnapshot {
    CellSnapshot {
        text: " ".to_owned(),
        foreground_source: TerminalColor::Default,
        background_source: TerminalColor::Default,
        inverse: false,
        bold: false,
        faint: false,
        italic: false,
        blinking: false,
        invisible: false,
        underline: TerminalUnderlineSnapshot::None,
        underline_source: TerminalColor::Default,
        strikethrough: false,
        overline: false,
        selected: false,
        spacer_tail: false,
        semantic_content: CellSemanticSnapshot::Output,
        hyperlink: None,
    }
}

/// A primary screen with one row per line and a visible cursor after the last line's text.
fn screen(lines: &[&str]) -> Arc<ScreenSnapshot> {
    let mut screen = (*ScreenSnapshot::empty(crate::local_path::LocalPathSemantics::Posix)).clone();
    screen.rows = lines
        .iter()
        .map(|line| {
            let mut cells: Vec<_> = line
                .chars()
                .map(|character| CellSnapshot {
                    text: character.to_string(),
                    ..blank()
                })
                .collect();
            cells.resize(usize::from(COLUMNS), blank());
            RowSnapshot::from(cells)
        })
        .collect();
    let row = lines.len().saturating_sub(1);
    screen.active_screen = ActiveScreenSnapshot::Primary;
    screen.cursor.visible = true;
    screen.cursor.position = Some(CursorPositionSnapshot {
        row: row as u16,
        column: lines.last().map_or(0, |line| line.chars().count()) as u16,
        width_cells: 1,
    });
    Arc::new(screen)
}

/// The characters the view draws as predictions over `screen`, in reading order.
pub(super) fn shown(predictor: &Predictor, screen: &ScreenSnapshot) -> String {
    let mut presented = screen.clone();
    let Some(overlay) = predictor.overlay() else {
        return String::new();
    };
    overlay.apply(&mut presented);
    presented
        .rows
        .iter()
        .flat_map(|row| row.iter())
        .filter(|cell| cell.underline == TerminalUnderlineSnapshot::Single)
        .map(|cell| cell.text.as_str())
        .collect()
}

pub(super) fn key(text: &str, modifiers: u16) -> pane::Key {
    pane::Key {
        physical: 0,
        action: 0,
        logical: "text-input".into(),
        text: Some(text.into()),
        unshifted: None,
        modifiers,
        consumed: 0,
        option_as_alt: true,
    }
}

pub(super) fn keys(keys: Vec<pane::Key>) -> Request {
    Request::Keys {
        lease: LEASE,
        generation: 1,
        keys,
    }
}

fn typed(text: &str) -> Request {
    keys(vec![key(text, 0)])
}

fn line(epoch: u64, mode: LineMode) -> LineState {
    LineState { epoch, mode }
}

/// Drives a predictor and its clock the way a Pane worker does.
struct Worker {
    predictor: Predictor,
    now: Instant,
    serial: u32,
    /// What the server shows.
    lines: Vec<String>,
}

impl Worker {
    /// A shell prompt at `$ ` whose frames arrive `round_trip` after each key.
    fn at_prompt(round_trip: Duration) -> Self {
        let mut worker = Self {
            predictor: Predictor::default(),
            now: Instant::now(),
            serial: 0,
            lines: vec!["$ ".into()],
        };
        worker.predictor.line(line(1, SHELL));
        worker.server_frame(0);
        // One echoed key measures the round trip.
        worker.send(typed("e"));
        worker.echo("e", round_trip);
        worker
    }

    /// A prompt with an active, confirmed epoch: the next printable key shows at once.
    fn confirmed() -> Self {
        let mut worker = Self::at_prompt(ROUND_TRIP);
        worker.send(typed("c"));
        assert_eq!(worker.shown(), "", "a new epoch shows nothing yet");
        worker.echo("c", ROUND_TRIP);
        worker
    }

    fn send(&mut self, request: Request) -> u32 {
        self.serial += 1;
        self.predictor.sent(&request, Some(self.serial), self.now);
        self.serial
    }

    fn screen(&self) -> Arc<ScreenSnapshot> {
        let lines: Vec<&str> = self.lines.iter().map(String::as_str).collect();
        screen(&lines)
    }

    fn server_frame(&mut self, input: u32) {
        self.predictor.frame(&self.screen(), input, self.now);
    }

    /// The server echoes `text` on its last line and the frame arrives `after` the key.
    fn echo(&mut self, text: &str, after: Duration) {
        self.now += after;
        self.lines.last_mut().unwrap().push_str(text);
        self.server_frame(self.serial);
    }

    /// The server starts a new line, and the frame arrives one round trip later.
    fn new_line(&mut self, text: &str) {
        self.lines.push(text.into());
        self.now += ROUND_TRIP;
        self.server_frame(self.serial);
    }

    fn shown(&self) -> String {
        shown(&self.predictor, &self.screen())
    }
}

#[test]
fn a_printable_key_at_the_shell_prompt_shows_before_its_echo_once_the_epoch_is_confirmed() {
    let mut worker = Worker::at_prompt(ROUND_TRIP);

    // The first prediction of an epoch waits for the server's echo.
    worker.send(typed("l"));
    assert_eq!(worker.shown(), "");
    worker.echo("l", ROUND_TRIP);

    // Later keys show at once, underlined, with the cursor after them.
    worker.send(typed("s"));
    worker.send(typed(" "));
    assert_eq!(worker.shown(), "s ");
    let mut presented = ScreenSnapshot::clone(&worker.screen());
    worker.predictor.overlay().unwrap().apply(&mut presented);
    assert_eq!(
        presented.cursor.position.map(|cursor| cursor.column),
        Some(6)
    );

    // The frame that reflects both keys and shows their echo confirms them.
    worker.echo("s ", ROUND_TRIP);
    assert_eq!(worker.shown(), "");
}

#[test]
fn no_prediction_appears_below_the_round_trip_threshold() {
    let fast = Duration::from_millis(29);
    let mut worker = Worker::at_prompt(fast);
    for text in ["a", "b", "c", "d"] {
        worker.send(typed(text));
        assert_eq!(worker.shown(), "");
        worker.echo(text, fast);
    }

    // Prediction turns off when the round trip falls well under the threshold.
    let mut worker = Worker::confirmed();
    for _ in 0..10 {
        worker.send(typed("x"));
        worker.echo("x", Duration::from_millis(5));
    }
    worker.send(typed("y"));
    assert_eq!(worker.shown(), "");
}

#[test]
fn password_prompts_raw_mode_and_foreground_jobs_never_predict() {
    for mode in [PASSWORD, RAW, JOB] {
        let mut worker = Worker::confirmed();
        worker.predictor.line(line(2, mode));
        for text in ["p", "w", "d"] {
            worker.send(typed(text));
            assert_eq!(worker.shown(), "", "{mode:?} predicted {text}");
        }
        // The program's frames change nothing.
        worker.now += ROUND_TRIP;
        worker.server_frame(worker.serial);
        worker.send(typed("x"));
        assert_eq!(worker.shown(), "", "{mode:?} predicted after a frame");
    }
}

#[test]
fn enter_and_control_keys_suspend_until_a_frame_reflects_them_and_the_echo_confirms_again() {
    for suspending in [
        key("\r", 0),
        key("c", CONTROL),
        key("u", 1 << 8),
        key("\u{7f}", 0),
        key("a", 1 << 1),
        key("v", 1 << 3),
    ] {
        let mut worker = Worker::confirmed();
        worker.send(keys(vec![suspending.clone()]));
        worker.send(typed("a"));
        assert_eq!(worker.shown(), "", "{suspending:?} did not suspend");

        // A frame reflects the suspending key: the next key starts an unconfirmed epoch.
        worker.new_line("$ ");
        worker.send(typed("b"));
        assert_eq!(worker.shown(), "");

        // Its echo confirms the epoch again.
        worker.echo("b", ROUND_TRIP);
        worker.send(typed("c"));
        assert_eq!(worker.shown(), "c");
    }
}

#[test]
fn only_unmodified_printable_ascii_shows() {
    let mut worker = Worker::confirmed();
    worker.send(keys(vec![key("A", 1)]));
    assert_eq!(worker.shown(), "A", "Shift types text");

    for text in ["é", "ab", "\t"] {
        let mut worker = Worker::confirmed();
        worker.send(typed(text));
        worker.send(typed("z"));
        assert_eq!(worker.shown(), "", "{text:?} did not suspend");
    }

    // A key release writes nothing that moves the cursor.
    let mut worker = Worker::confirmed();
    let mut release = key("a", 0);
    release.action = KEY_RELEASE;
    worker.send(keys(vec![key("a", 0), release]));
    worker.send(typed("b"));
    assert_eq!(worker.shown(), "ab");

    let mut worker = Worker::confirmed();
    worker.send(Request::Input {
        lease: LEASE,
        generation: 1,
        input: InteractiveInput::Paste { text: "a".into() },
    });
    worker.send(typed("b"));
    assert_eq!(worker.shown(), "", "a paste did not suspend");
}

#[test]
fn a_password_typed_before_the_prompt_appears_never_shows() {
    let mut worker = Worker::confirmed();
    worker.send(typed("\r"));
    // The frame reflecting Enter arrives before the program reads it, so the line state still
    // shows the line editor.
    worker.new_line("");

    // The first password key lands where the prompt will start, as a hidden prediction.
    worker.send(typed("P"));
    assert_eq!(worker.shown(), "");

    // The server pushes the line state it read with the prompt before the prompt's frame.
    worker.predictor.line(line(2, PASSWORD));
    worker.echo("Password:", ROUND_TRIP);
    for text in ["a", "s", "s"] {
        worker.send(typed(text));
        assert_eq!(worker.shown(), "");
    }
}

#[test]
fn a_silent_read_in_the_line_editors_mode_never_shows_its_keys() {
    // bash's `read -rsn4 -p 'PIN: '` reads in the line editor's mode, so only the echo
    // confirmation protects it.
    let mut worker = Worker::confirmed();
    worker.send(typed("\r"));
    worker.new_line("");
    worker.send(typed("P"));
    assert_eq!(worker.shown(), "");

    // The prompt starts with the typed character where it was predicted, but leaves the cursor
    // after itself.
    worker.echo("PIN: ", ROUND_TRIP);
    for text in ["1", "2", "3"] {
        worker.send(typed(text));
        assert_eq!(worker.shown(), "", "{text} showed");
        worker.now += ROUND_TRIP;
        worker.server_frame(worker.serial);
    }

    // The same character where it was predicted, with the cursor after it, still does not
    // confirm the epoch when the row to its left changed.
    let mut worker = Worker::confirmed();
    worker.send(typed("\r"));
    worker.new_line("> ");
    worker.send(typed("x"));
    worker.lines.pop();
    worker.new_line("$ x");
    worker.send(typed("y"));
    assert_eq!(worker.shown(), "");
}

#[test]
fn an_unechoed_key_never_shows_and_expires() {
    let mut worker = Worker::confirmed();
    worker.send(typed("\r"));
    worker.new_line("");
    worker.send(typed("s"));

    // A program that turned echo off reads the key; its frame shows nothing at the cell.
    worker.now += ROUND_TRIP;
    worker.server_frame(worker.serial);
    assert_eq!(worker.shown(), "");
    assert!(worker.predictor.deadline().is_some());
    worker.predictor.expire(worker.now + Duration::from_secs(1));
    assert_eq!(worker.predictor.deadline(), None);
    worker.send(typed("t"));
    assert_eq!(worker.shown(), "");
}

#[test]
fn a_frame_that_reflects_a_key_before_its_echo_keeps_the_shown_prediction() {
    let mut worker = Worker::confirmed();
    worker.send(typed("a"));
    worker.send(typed("b"));
    // The frame of `a`'s echo already reflects `b`.
    worker.echo("a", ROUND_TRIP);
    assert_eq!(worker.shown(), "b");
    worker.echo("b", Duration::from_millis(1));
    assert_eq!(worker.shown(), "");
    worker.send(typed("c"));
    assert_eq!(worker.shown(), "c", "the epoch stays confirmed");
}

#[test]
fn a_mismatch_rolls_back_the_whole_epoch_and_restores_the_server_screen() {
    let mut worker = Worker::confirmed();
    let server = worker.screen();
    worker.send(typed("a"));
    worker.send(typed("b"));
    assert_eq!(worker.shown(), "ab");
    assert_eq!(
        server.rows[0][4].text, " ",
        "the server's screen is untouched"
    );

    // The program echoed something else: every prediction goes.
    worker.echo("ax", ROUND_TRIP);
    assert_eq!(worker.shown(), "");
    worker.send(typed("c"));
    assert_eq!(worker.shown(), "");
}

#[test]
fn the_view_shows_predictions_over_the_server_screen_and_exactly_that_screen_after_rollback() {
    let mut worker = Worker::confirmed();
    let mut reading = Reading::default();
    reading.scrollback.screen(worker.screen());
    worker.send(typed("a"));
    assert!(reading.predict(worker.predictor.overlay()));
    let predicted = reading.presented(PresentationGeneration::test(1)).unwrap();
    assert_eq!(predicted.rows[0][4].text, "a");
    assert_eq!(
        predicted.rows[0][4].underline,
        TerminalUnderlineSnapshot::Single
    );

    // The server echoed something else. The view shows exactly the server's rows and cursor.
    worker.echo("x", ROUND_TRIP);
    let server = worker.screen();
    reading.scrollback.screen(Arc::clone(&server));
    assert!(reading.predict(worker.predictor.overlay()));
    let presented = reading.presented(PresentationGeneration::test(2)).unwrap();
    assert_eq!(presented.rows, server.rows);
    assert_eq!(presented.cursor, server.cursor);
}

#[test]
fn a_lost_connection_presents_the_frozen_screen_without_predictions_once() {
    let mut worker = Worker::confirmed();
    let server = worker.screen();
    let mut reading = Reading::default();
    reading.scrollback.screen(Arc::clone(&server));
    assert_eq!(reading.withdraw(PresentationGeneration::test(1)), None);

    worker.send(typed("a"));
    reading.predict(worker.predictor.overlay());
    let predicted = reading.presented(PresentationGeneration::test(2)).unwrap();
    assert_ne!(predicted.rows, server.rows);

    reading.disconnect();
    let frozen = reading
        .withdraw(PresentationGeneration::test(3))
        .expect("the view still shows predictions");
    assert_eq!(frozen.rows, server.rows);
    assert_eq!(frozen.cursor, server.cursor);
    assert_eq!(reading.withdraw(PresentationGeneration::test(4)), None);
}

#[test]
fn a_paste_measures_its_round_trip_from_before_its_send() {
    let mut worker = Worker::confirmed();
    // A paste's send waits for its outcome, so its frame can arrive before the send returns.
    // The round trip starts when the send began.
    let sent_at = worker.now;
    worker.serial += 1;
    let paste = Request::Input {
        lease: LEASE,
        generation: 1,
        input: InteractiveInput::Paste { text: "ab".into() },
    };
    worker.predictor.sent(&paste, Some(worker.serial), sent_at);
    worker.echo("ab", ROUND_TRIP);

    worker.send(typed("c"));
    worker.echo("c", ROUND_TRIP);
    worker.send(typed("d"));
    assert_eq!(worker.shown(), "d", "the paste turned prediction off");
}

#[test]
fn a_scrollback_clear_takes_no_sample_and_drops_predictions() {
    let mut worker = Worker::confirmed();
    worker.send(typed("a"));
    assert_eq!(worker.shown(), "a");

    // The clear takes no Input serial, and its reply arrives one round trip after `a`.
    worker.now += ROUND_TRIP / 2;
    let clear = Request::ClearScreenAndScrollback { lease: LEASE };
    worker.predictor.sent(&clear, None, worker.now);
    assert_eq!(worker.shown(), "");
    worker.lines = vec!["$ eca".into()];
    worker.now += ROUND_TRIP / 2;
    worker.server_frame(worker.serial);
    // A frame drained right after the clear's reply must not count as a round trip.
    worker.now += Duration::from_millis(1);
    worker.server_frame(worker.serial);

    worker.send(typed("b"));
    worker.echo("b", ROUND_TRIP);
    worker.send(typed("c"));
    assert_eq!(worker.shown(), "c", "the clear turned prediction off");
}

#[test]
fn a_line_state_change_drops_shown_predictions() {
    let mut worker = Worker::confirmed();
    worker.send(typed("a"));
    assert_eq!(worker.shown(), "a");
    worker.predictor.line(line(2, SHELL));
    assert_eq!(worker.shown(), "");
}

#[test]
fn the_alternate_screen_a_hidden_cursor_and_the_last_column_stop_prediction() {
    let mut worker = Worker::confirmed();
    let mut alternate = ScreenSnapshot::clone(&worker.screen());
    alternate.active_screen = ActiveScreenSnapshot::Alternate;
    worker
        .predictor
        .frame(&Arc::new(alternate), worker.serial, worker.now);
    worker.send(typed("a"));
    assert_eq!(worker.shown(), "");

    let mut worker = Worker::confirmed();
    let mut hidden = ScreenSnapshot::clone(&worker.screen());
    hidden.cursor.visible = false;
    worker
        .predictor
        .frame(&Arc::new(hidden), worker.serial, worker.now);
    worker.send(typed("a"));
    assert_eq!(worker.shown(), "");

    // `$ ec` leaves 8 of 12 columns, and the last one is never predicted.
    let mut worker = Worker::confirmed();
    for text in "abcdefgh".chars() {
        worker.send(typed(&text.to_string()));
    }
    assert_eq!(worker.shown(), "abcdefg");
}
