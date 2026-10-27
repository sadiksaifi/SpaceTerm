//! Encoder behavior that does not need the client decoder: sizes, scroll detection, baselines,
//! table bounds and Scrollback epochs.
use super::screen_encoder::{ScreenCapture, ScreenEncoder, encode_page};
use super::*;
use crate::terminal::emulator::TerminalEmulator;
use crate::terminal::geometry::{BackingScale, CellGridSize, LogicalCellSize, TerminalGeometry};
use spaceterm_server::protocol::frame::encode_server;
use spaceterm_server::protocol::screen::{Cells, ScreenUpdate, ScrollShift, StyleRef};
use spaceterm_server::protocol::{Channel, Frame, ServerMessage, limits};

fn geometry(columns: u16, rows: u16) -> TerminalGeometry {
    TerminalGeometry::from_grid(
        CellGridSize::new(columns, rows),
        LogicalCellSize::new(8.0, 16.0),
        BackingScale::ONE,
    )
}

/// One emulator presented through one encoder, acknowledging every frame.
struct Channel1 {
    emulator: TerminalEmulator,
    encoder: ScreenEncoder,
    screen: Option<Arc<ScreenSnapshot>>,
    generation: u64,
    frame: u64,
    acknowledged: Option<u64>,
}

impl Channel1 {
    fn new(columns: u16, rows: u16) -> Self {
        Self {
            emulator: TerminalEmulator::new(geometry(columns, rows)).unwrap(),
            encoder: ScreenEncoder::default(),
            screen: None,
            generation: 0,
            frame: 0,
            acknowledged: None,
        }
    }

    fn capture(&mut self) -> ScreenCapture {
        if let Some(screen) = self.emulator.snapshot().unwrap() {
            self.screen = Some(screen);
            self.generation += 1;
        }
        let screen = Arc::clone(self.screen.as_ref().unwrap());
        ScreenCapture {
            screen,
            generation: self.generation,
            accessibility_revision: 0,
        }
    }

    /// Encodes the emulator's state against the acknowledged frame and returns the update and
    /// its bytes on the wire.
    fn present(&mut self) -> Option<(ScreenUpdate, usize)> {
        let capture = self.capture();
        let frame = self.frame + 1;
        let update = self.encoder.encode(&capture, frame, self.acknowledged)?;
        self.frame = frame;
        let bytes = wire_bytes(frame, self.acknowledged, &update);
        self.acknowledged = Some(frame);
        Some((update, bytes))
    }
}

fn wire_bytes(frame: u64, base: Option<u64>, update: &ScreenUpdate) -> usize {
    encode_server(&ServerMessage::Frame(Box::new(Frame {
        channel: Channel(1),
        frame,
        base,
        input: 1,
        update: update.clone(),
    })))
    .unwrap()
    .frames
    .iter()
    .map(Vec::len)
    .sum()
}

/// A shell session on a 200x50 grid, as in the encoding spike.
fn shell(channel: &mut Channel1) {
    for line in 0..49 {
        channel.emulator.feed(match line % 4 {
            0 => b"\x1b[1;32msdk@mac \x1b[1;34m~/Projects/SpaceTerm \x1b[0;35m(main) \x1b[32m\xe2\x9d\xaf \x1b[0mls -la crates/\r\n".as_slice(),
            1 => b"drwxr-xr-x  12 sdk  staff    384 Oct 27 10:12 \x1b[1;34mspaceterm-server\x1b[0m/\r\n",
            2 => b"-rw-r--r--   1 sdk  staff   2210 Oct 27 09:01 \xe6\x97\xa5\xe6\x9c\xac\xe8\xaa\x9e.txt \xf0\x9f\x91\xa8\xe2\x80\x8d\xf0\x9f\x91\xa9\xe2\x80\x8d\xf0\x9f\x91\xa7\r\n",
            _ => b"   Compiling spaceterm-server v0.42.0 (/Users/sdk/Projects/SpaceTerm/crates/spaceterm-server)\r\n",
        });
    }
    channel.emulator.feed(
        b"\x1b[1;32msdk@mac \x1b[1;34m~/Projects/SpaceTerm \x1b[0;35m(main) \x1b[32m\xe2\x9d\xaf \x1b[0mgit sta",
    );
}

fn log_line(number: usize) -> String {
    let (level, color) = [("INFO ", 32), ("WARN ", 33), ("DEBUG", 90), ("ERROR", 31)][number % 4];
    format!(
        "\x1b[90m2026-10-27T10:{:02}:{:02}.{:03}Z \x1b[1;{color}m{level} \x1b[0;36mspaceterm_server::pane::presentation: \x1b[0mrequest={number} encoded screen generation={} rows=50 bytes={}\r\n",
        number / 60 % 60,
        number % 60,
        number * 7 % 1000,
        number * 3,
        390_000 + number,
    )
}

#[test]
fn a_typed_character_costs_about_one_hundred_bytes() {
    let mut channel = Channel1::new(200, 50);
    shell(&mut channel);
    let (keyframe, keyframe_bytes) = channel.present().unwrap();
    assert!(keyframe.geometry.is_some() && keyframe.colors.is_some());
    channel.emulator.feed(b"t");
    let (update, bytes) = channel.present().unwrap();
    assert_eq!(update.rows.len(), 1);
    assert_eq!(update.rows[0].index, 49);
    assert!(update.shift.is_none() && update.styles.is_empty() && update.colors.is_none());
    // 105 B at the time of writing: 10 B of frame header, 13 B of cursor, history and absent
    // fields, and the 82 B prompt row of five runs, four of them in a table style.
    assert!(bytes <= 105, "a typed character took {bytes} B");
    assert!(
        keyframe_bytes < 8 * 1024,
        "the keyframe took {keyframe_bytes} B"
    );
}

#[test]
fn a_three_line_log_scroll_costs_about_half_a_kilobyte() {
    let mut channel = Channel1::new(200, 50);
    for number in 0..80 {
        channel.emulator.feed(log_line(number).as_bytes());
    }
    channel.present().unwrap();
    for number in 80..83 {
        channel.emulator.feed(log_line(number).as_bytes());
    }
    let (update, bytes) = channel.present().unwrap();
    // The blank cursor row below the log does not move, so the region may end above it.
    let shift = update.shift.unwrap();
    assert_eq!((shift.top, shift.lines), (0, 3));
    assert!(shift.bottom >= 49);
    assert_eq!(
        update.rows.iter().map(|row| row.index).collect::<Vec<_>>(),
        [46, 47, 48]
    );
    assert!(bytes <= 550, "a 3-line scroll took {bytes} B");
}

#[test]
fn a_scroll_region_keeps_its_status_line_and_scrolls_down_too() {
    let mut channel = Channel1::new(40, 10);
    for number in 0..9 {
        channel
            .emulator
            .feed(format!("\x1b[{};1Hline {number}", number + 1).as_bytes());
    }
    channel
        .emulator
        .feed(b"\x1b[10;1H\x1b[7mstatus\x1b[0m\x1b[1;9r\x1b[9;1H");
    channel.present().unwrap();
    channel.emulator.feed(b"\nnew 1\r\nnew 2");
    let (update, _) = channel.present().unwrap();
    assert_eq!(
        update.shift,
        Some(ScrollShift {
            top: 0,
            bottom: 9,
            lines: 2
        })
    );
    assert_eq!(
        update.rows.iter().map(|row| row.index).collect::<Vec<_>>(),
        [7, 8]
    );
    channel.emulator.feed(b"\x1b[1;1H\x1bM\x1bMtop");
    let (update, _) = channel.present().unwrap();
    assert_eq!(
        update.shift,
        Some(ScrollShift {
            top: 0,
            bottom: 9,
            lines: -2
        })
    );
    assert_eq!(
        update.rows.iter().map(|row| row.index).collect::<Vec<_>>(),
        [0]
    );
}

#[test]
fn an_unchanged_capture_sends_nothing_and_keeps_the_baseline() {
    let mut channel = Channel1::new(20, 4);
    channel.emulator.feed(b"hello");
    channel.present().unwrap();
    assert!(channel.present().is_none());
    // A base the encoder never emitted receives every row, even though nothing changed.
    let capture = channel.capture();
    let replaced = channel.encoder.encode(&capture, 9, Some(7)).unwrap();
    assert_eq!(replaced.rows.len(), 4);
    assert!(replaced.geometry.is_some() && replaced.colors.is_some());
    assert!(replaced.modes.is_some());
    // The previous baseline is gone, so frame 1 is unknown now.
    let unknown = channel.encoder.encode(&capture, 10, Some(1)).unwrap();
    assert_eq!(unknown.rows.len(), 4);
    assert!(channel.encoder.encode(&capture, 11, Some(10)).is_none());
    channel.emulator.feed(b"!");
    let capture = channel.capture();
    let update = channel.encoder.encode(&capture, 11, Some(10)).unwrap();
    assert_eq!(update.rows.len(), 1);
}

#[test]
fn keyframes_omit_blank_rows_and_resizes_resend_every_row_that_is_not_blank() {
    let mut channel = Channel1::new(20, 6);
    channel.emulator.feed(b"one\r\n\r\nthree");
    let (keyframe, _) = channel.present().unwrap();
    assert_eq!(
        keyframe
            .rows
            .iter()
            .map(|row| row.index)
            .collect::<Vec<_>>(),
        [0, 2]
    );
    channel.emulator.resize(geometry(30, 8)).unwrap();
    let (resized, _) = channel.present().unwrap();
    let geometry = resized.geometry.unwrap();
    assert_eq!((geometry.columns, geometry.rows), (30, 8));
    assert!(resized.shift.is_none());
    assert!(resized.rows.iter().all(|row| !row.runs.is_empty()));
    assert!(!resized.rows.is_empty());
}

#[test]
fn styles_are_defined_once_per_channel_until_a_keyframe() {
    let mut channel = Channel1::new(20, 4);
    channel.emulator.feed(b"\x1b[31mred\x1b[0m");
    let (first, _) = channel.present().unwrap();
    assert_eq!(first.styles.len(), 1);
    channel.emulator.feed(b"\r\n\x1b[31mred again\x1b[0m");
    let (second, _) = channel.present().unwrap();
    assert!(second.styles.is_empty());
    assert_eq!(
        second.rows[0].runs[0].style,
        StyleRef::Id(first.styles[0].id)
    );
    channel.acknowledged = None;
    let (keyframe, _) = channel.present().unwrap();
    assert_eq!(keyframe.styles.len(), 1);
}

#[test]
fn wide_characters_and_graphemes_use_explicit_cells() {
    let mut channel = Channel1::new(20, 2);
    channel.emulator.feed("ab界ce\u{301}🙂".as_bytes());
    let (update, _) = channel.present().unwrap();
    let run = &update.rows[0].runs[0];
    assert_eq!(run.text, "ab界ce\u{301}🙂");
    // A wide character is followed by its zero-length spacer.
    assert_eq!(run.cells, Cells::Explicit(vec![1, 1, 3, 0, 1, 3, 4, 0]));
    channel.emulator.feed(b"\r\nplain");
    let (update, _) = channel.present().unwrap();
    assert_eq!(update.rows[0].runs[0].cells, Cells::Narrow);
}

#[test]
fn styles_beyond_a_full_table_travel_inline_without_resending_the_screen() {
    let mut channel = Channel1::new(512, 12);
    let distinct = |row: usize| {
        let mut line = format!("\x1b[{};1H", row + 1);
        for column in 0..512 {
            let value = row * 512 + column;
            line.push_str(&format!(
                "\x1b[38;2;{};{};{}mx",
                value % 256,
                value / 256 % 256,
                value / 65_536
            ));
        }
        line
    };
    for row in 0..8 {
        let line = distinct(row);
        channel.emulator.feed(line.as_bytes());
    }
    let (first, _) = channel.present().unwrap();
    assert_eq!(first.styles.len(), limits::MAX_STYLES);
    let line = distinct(8);
    channel.emulator.feed(line.as_bytes());
    let (update, _) = channel.present().unwrap();
    // Only the new row travels, and every one of its cells keeps its own color.
    assert_eq!(
        update.rows.iter().map(|row| row.index).collect::<Vec<_>>(),
        [8]
    );
    assert!(update.styles.is_empty());
    assert_eq!(update.rows[0].runs.len(), 512);
    assert!(
        update.rows[0]
            .runs
            .iter()
            .all(|run| matches!(run.style, StyleRef::Inline(_)))
    );
    update.validate().unwrap();
    channel.emulator.feed(b"\x1b[12;1Hplain");
    let (typed, _) = channel.present().unwrap();
    assert_eq!(
        typed.rows.iter().map(|row| row.index).collect::<Vec<_>>(),
        [11]
    );
}

#[test]
fn frames_carry_the_emulators_history_positions() {
    let mut channel = Channel1::new(20, 4);
    for _ in 0..10 {
        channel.emulator.feed(b"line\r\n");
    }
    let capture = channel.capture();
    let (update, _) = channel.present().unwrap();
    let history = capture.screen.history;
    assert_eq!(
        (
            update.history.epoch,
            update.history.oldest,
            update.history.total
        ),
        (history.epoch, history.oldest, history.total)
    );
    assert_eq!(update.history.total, 7);
}

#[test]
fn pages_carry_their_own_tables() {
    let mut channel = Channel1::new(20, 4);
    channel.emulator.feed(
        b"\x1b[31mred\x1b[0m\r\nplain\r\n\x1b]8;;https://example.com\x1b\\link\x1b]8;;\x1b\\",
    );
    let screen = channel.capture().screen;
    let rows = screen
        .rows
        .iter()
        .zip(screen.row_soft_wrapped.iter())
        .map(|(row, wrapped)| (row.as_ref(), *wrapped));
    let first = encode_page(3, 100, rows.clone());
    let second = encode_page(3, 100, rows);
    assert_eq!(first, second);
    assert_eq!((first.epoch, first.start), (3, 100));
    assert_eq!(first.rows.len(), 4);
    // Only the red text has a style; the link keeps the default style.
    assert_eq!(first.styles.len(), 1);
    assert_eq!(first.links.len(), 1);
    first.validate().unwrap();
}
