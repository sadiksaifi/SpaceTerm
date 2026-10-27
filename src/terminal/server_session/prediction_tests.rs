//! Deterministic tests for predictive local echo. Each test drives the predictor with the
//! requests, frames and line states a Pane worker would see, at fixed instants.
use super::*;
use crate::terminal::emulator::CellSemanticSnapshot;
use crate::terminal::{CellSnapshot, TerminalColor};
use spaceterm_server::pane::{self, Lease, LineMode};

const COLUMNS: u16 = 12;
const LEASE: Lease = Lease {
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

fn key(text: &str, modifiers: u16) -> pane::Key {
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

fn keys(keys: Vec<pane::Key>) -> Request {
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
        Self::under(SHELL, round_trip)
    }

    /// A prompt read under `mode`, with frames arriving `round_trip` after each key.
    fn under(mode: LineMode, round_trip: Duration) -> Self {
        let mut worker = Self {
            predictor: Predictor::default(),
            now: Instant::now(),
            serial: 0,
            lines: vec!["$ ".into()],
        };
        worker.predictor.line(line(1, mode));
        worker.server_frame(0);
        // One echoed key measures the round trip.
        worker.send(typed("e"));
        worker.echo("e", round_trip);
        worker
    }

    /// A prompt with an active, confirmed epoch: the next printable key shows at once.
    fn confirmed() -> Self {
        let mut worker = Self::at_prompt(Duration::from_millis(80));
        worker.send(typed("c"));
        assert_eq!(
            worker.predictor.overlay(),
            None,
            "a new epoch shows nothing yet"
        );
        worker.echo("c", Duration::from_millis(80));
        worker
    }

    fn send(&mut self, request: Request) -> u32 {
        self.serial += 1;
        self.predictor.sent(&request, self.serial, self.now);
        self.serial
    }

    fn server_frame(&mut self, input: u32) {
        let lines: Vec<&str> = self.lines.iter().map(String::as_str).collect();
        self.predictor.frame(&screen(&lines), input, self.now);
    }

    /// The server echoes `text` on its last line and the frame arrives `after` the key.
    fn echo(&mut self, text: &str, after: Duration) {
        self.now += after;
        self.lines.last_mut().unwrap().push_str(text);
        self.server_frame(self.serial);
    }

    fn shown(&self) -> Vec<char> {
        self.predictor
            .overlay()
            .map(|overlay| overlay.cells.iter().map(|cell| cell.2).collect())
            .unwrap_or_default()
    }
}

/// Whether printable keys typed under `mode` at an 80 ms round trip ever show before their echo.
/// Real PTY tests pass the mode a program set.
pub(crate) fn shows_keys_under(mode: LineMode) -> bool {
    let mut worker = Worker::under(mode, Duration::from_millis(80));
    let mut shown = false;
    for text in ["a", "b", "c"] {
        worker.send(typed(text));
        shown |= !worker.shown().is_empty();
        worker.echo(text, Duration::from_millis(80));
    }
    shown
}

#[test]
fn a_printable_key_at_the_shell_prompt_shows_before_its_echo_once_the_epoch_is_confirmed() {
    let mut worker = Worker::at_prompt(Duration::from_millis(80));

    // The first prediction of an epoch waits for the server's echo.
    worker.send(typed("l"));
    assert!(worker.shown().is_empty());
    worker.echo("l", Duration::from_millis(80));

    // Later keys show at once, underlined at the predicted cursor.
    worker.send(typed("s"));
    let overlay = worker.predictor.overlay().unwrap();
    assert_eq!(overlay.cells, [(0, 4, 's')]);
    assert_eq!(overlay.cursor, (0, 5));
    worker.send(typed(" "));
    assert_eq!(worker.shown(), ['s', ' ']);

    // The frame that reflects both keys and shows their echo confirms them.
    worker.echo("s ", Duration::from_millis(80));
    assert_eq!(worker.predictor.overlay(), None);
}

#[test]
fn no_prediction_appears_below_the_round_trip_threshold() {
    let mut worker = Worker::at_prompt(Duration::from_millis(29));
    for text in ["a", "b", "c", "d"] {
        worker.send(typed(text));
        assert!(worker.shown().is_empty());
        worker.echo(text, Duration::from_millis(29));
    }
    assert!(worker.shown().is_empty());

    // Prediction turns off when the round trip falls under the lower threshold.
    let mut worker = Worker::confirmed();
    for _ in 0..ROUND_TRIP_SAMPLES {
        worker.send(typed("x"));
        worker.echo("x", Duration::from_millis(5));
    }
    worker.send(typed("y"));
    assert!(worker.shown().is_empty());
}

#[test]
fn password_prompts_raw_mode_and_foreground_jobs_never_predict() {
    for mode in [PASSWORD, RAW, JOB] {
        let mut worker = Worker::confirmed();
        worker.predictor.line(line(2, mode));
        for text in ["p", "w", "d"] {
            worker.send(typed(text));
            assert!(worker.shown().is_empty(), "{mode:?} predicted {text}");
        }
        // The program's frames change nothing.
        worker.now += Duration::from_millis(80);
        worker.server_frame(worker.serial);
        worker.send(typed("x"));
        assert!(
            worker.shown().is_empty(),
            "{mode:?} predicted after a frame"
        );
    }
}

#[test]
fn enter_and_control_keys_suspend_until_a_frame_reflects_them_and_the_echo_confirms_again() {
    for suspending in [
        key("\r", 0),
        key("c", 1 << 2),
        key("u", 1 << 8),
        key("\u{7f}", 0),
    ] {
        let mut worker = Worker::confirmed();
        worker.send(keys(vec![suspending.clone()]));
        worker.send(typed("a"));
        assert!(worker.shown().is_empty(), "{suspending:?} did not suspend");

        // A frame reflects the suspending key: the next key starts an unconfirmed epoch.
        worker.lines.push("$ ".into());
        worker.now += Duration::from_millis(80);
        worker.server_frame(worker.serial);
        worker.send(typed("b"));
        assert!(worker.shown().is_empty());

        // Its echo confirms the epoch again.
        worker.echo("b", Duration::from_millis(80));
        worker.send(typed("c"));
        assert_eq!(worker.shown(), ['c']);
    }
}

#[test]
fn a_password_typed_before_the_prompt_appears_never_shows() {
    let mut worker = Worker::confirmed();
    worker.send(typed("\r"));
    // The frame reflecting Enter arrives before the program reads it, so the line state still
    // shows the line editor.
    worker.lines.push(String::new());
    worker.now += Duration::from_millis(80);
    worker.server_frame(worker.serial);

    // The first password key lands where the prompt will start, as a hidden prediction.
    worker.send(typed("P"));
    assert!(worker.shown().is_empty());

    // The server pushes the line state it read with the prompt before the prompt's frame.
    worker.predictor.line(line(2, PASSWORD));
    worker.echo("Password:", Duration::from_millis(80));
    for text in ["a", "s", "s"] {
        worker.send(typed(text));
        assert!(worker.shown().is_empty());
    }
}

#[test]
fn an_unechoed_key_never_shows_and_expires_after_the_grace() {
    let mut worker = Worker::confirmed();
    worker.send(typed("\r"));
    worker.lines.push(String::new());
    worker.now += Duration::from_millis(80);
    worker.server_frame(worker.serial);
    worker.send(typed("s"));

    // A program that turned echo off reads the key; its frame shows nothing at the cell.
    worker.now += Duration::from_millis(80);
    worker.server_frame(worker.serial);
    assert!(worker.shown().is_empty());
    let deadline = worker.predictor.deadline().unwrap();
    assert_eq!(deadline, worker.now + ECHO_GRACE);
    worker.predictor.expire(deadline);
    assert_eq!(worker.predictor.deadline(), None);
    worker.send(typed("t"));
    assert!(worker.shown().is_empty());
}

#[test]
fn a_frame_that_reflects_a_key_before_its_echo_keeps_the_shown_prediction_within_the_grace() {
    let mut worker = Worker::confirmed();
    worker.send(typed("a"));
    worker.send(typed("b"));
    // The frame of `a`'s echo already reflects `b`.
    worker.echo("a", Duration::from_millis(80));
    assert_eq!(worker.shown(), ['b']);
    worker.echo("b", Duration::from_millis(1));
    assert!(worker.shown().is_empty());
    worker.send(typed("c"));
    assert_eq!(worker.shown(), ['c'], "the epoch stays confirmed");
}

#[test]
fn a_mismatch_rolls_back_the_whole_epoch_and_restores_the_server_screen() {
    let mut worker = Worker::confirmed();
    let before: Vec<&str> = worker.lines.iter().map(String::as_str).collect();
    let server = screen(&before);
    worker.send(typed("a"));
    worker.send(typed("b"));
    assert_eq!(worker.shown(), ['a', 'b']);

    // The overlay draws into a copy; the server's screen is untouched.
    let mut presented = ScreenSnapshot::clone(&server);
    worker.predictor.overlay().unwrap().apply(&mut presented);
    assert_eq!(presented.rows[0][4].text, "a");
    assert_eq!(
        presented.rows[0][4].underline,
        TerminalUnderlineSnapshot::Single
    );
    assert_eq!(
        presented.cursor.position.map(|cursor| cursor.column),
        Some(6)
    );
    assert_eq!(server.rows[0][4].text, " ");

    // The program echoed something else: every prediction goes, so the view shows exactly the
    // server's screen.
    worker.echo("ax", Duration::from_millis(80));
    assert_eq!(worker.predictor.overlay(), None);
    worker.send(typed("c"));
    assert!(worker.shown().is_empty());
}

#[test]
fn the_view_shows_predictions_over_the_server_screen_and_exactly_that_screen_after_rollback() {
    use super::super::reading::Reading;
    use crate::terminal::PresentationGeneration;
    let mut worker = Worker::confirmed();
    let mut reading = Reading::default();
    reading.scrollback.screen(screen(&["$ ec"]));
    worker.send(typed("a"));
    assert!(reading.predict(worker.predictor.overlay()));
    let predicted = reading.presented(PresentationGeneration::test(1)).unwrap();
    assert_eq!(predicted.rows[0][4].text, "a");
    assert_eq!(
        predicted.rows[0][4].underline,
        TerminalUnderlineSnapshot::Single
    );

    // The server echoed something else. The view shows exactly the server's rows and cursor.
    worker.echo("x", Duration::from_millis(80));
    let server = screen(&["$ ecx"]);
    reading.scrollback.screen(Arc::clone(&server));
    assert!(reading.predict(worker.predictor.overlay()));
    let presented = reading.presented(PresentationGeneration::test(2)).unwrap();
    assert_eq!(presented.rows, server.rows);
    assert_eq!(presented.cursor, server.cursor);
}

#[test]
fn a_line_state_change_drops_shown_predictions() {
    let mut worker = Worker::confirmed();
    worker.send(typed("a"));
    assert_eq!(worker.shown(), ['a']);
    worker.predictor.line(line(2, SHELL));
    assert!(worker.shown().is_empty());
}

#[test]
fn the_alternate_screen_a_hidden_cursor_and_the_last_column_stop_prediction() {
    let mut worker = Worker::confirmed();
    let mut alternate = (*screen(&["$ ec"])).clone();
    alternate.active_screen = ActiveScreenSnapshot::Alternate;
    worker
        .predictor
        .frame(&Arc::new(alternate), worker.serial, worker.now);
    worker.send(typed("a"));
    assert!(worker.shown().is_empty());

    let mut worker = Worker::confirmed();
    let mut hidden = (*screen(&["$ ec"])).clone();
    hidden.cursor.visible = false;
    worker
        .predictor
        .frame(&Arc::new(hidden), worker.serial, worker.now);
    worker.send(typed("a"));
    assert!(worker.shown().is_empty());

    let mut worker = Worker::confirmed();
    for text in "abcdefgh".chars() {
        worker.send(typed(&text.to_string()));
    }
    assert_eq!(worker.shown().len(), usize::from(COLUMNS) - 4 - 1);
}

#[test]
fn only_unmodified_printable_ascii_is_predicted() {
    let printable = |key: pane::Key| {
        let mut kinds = Vec::new();
        typed_of(&keys(vec![key]), &mut kinds);
        kinds
    };
    assert_eq!(printable(key("a", 1)), [Typed::Printable('a')]);
    assert_eq!(printable(key("a", 1 << 1)), [Typed::Other]);
    assert_eq!(printable(key("a", 1 << 3)), [Typed::Other]);
    assert_eq!(printable(key("é", 0)), [Typed::Other]);
    assert_eq!(printable(key("ab", 0)), [Typed::Other]);
    assert_eq!(printable(key("\t", 0)), [Typed::Other]);
    let mut release = key("a", 0);
    release.action = KEY_RELEASE;
    assert!(printable(release).is_empty());

    let mut kinds = Vec::new();
    let paste = Request::Input {
        lease: LEASE,
        generation: 1,
        input: InteractiveInput::Paste { text: "a".into() },
    };
    typed_of(&paste, &mut kinds);
    typed_of(
        &Request::EndAccessibilityDemand { lease: LEASE },
        &mut kinds,
    );
    assert_eq!(kinds, [Typed::Other]);
}

fn typed_of(request: &Request, kinds: &mut Vec<Typed>) {
    super::typed(request, |kind| kinds.push(kind));
}
