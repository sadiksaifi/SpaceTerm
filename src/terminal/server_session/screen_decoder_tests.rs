//! Round trips from a real emulator through the server encoder, the wire and this decoder.
use super::screen_decoder::{DecodedScreen, Presenter, ScreenDecoder, UpdateError, decode_page};
use super::*;
use crate::runtime::screen_encoder::{ScreenCapture, ScreenEncoder, encode_page};
use crate::terminal::emulator::TerminalEmulator;
use crate::terminal::geometry::{BackingScale, CellGridSize, LogicalCellSize, TerminalGeometry};
use crate::terminal::testing::count_allocations;
use spaceterm_server::protocol::frame::{Assembler, encode_server};
use spaceterm_server::protocol::screen::{Cells, Row, Run, ScreenUpdate, ScrollShift, StyleRef};
use spaceterm_server::protocol::{Channel, Frame, RawFrame, ServerMessage, limits};

fn geometry(columns: u16, rows: u16) -> TerminalGeometry {
    TerminalGeometry::from_grid(
        CellGridSize::new(columns, rows),
        LogicalCellSize::new(8.0, 16.0),
        BackingScale::ONE,
    )
}

/// An emulator on the server, its channel encoder, and the client's decoder and presenter.
struct Session {
    emulator: TerminalEmulator,
    encoder: ScreenEncoder,
    decoder: ScreenDecoder,
    presenter: Presenter,
    appearance: TerminalAppearanceUpdate,
    screen: Option<Arc<ScreenSnapshot>>,
    generation: u64,
    frame: u64,
}

impl Session {
    fn new(columns: u16, rows: u16) -> Self {
        Self {
            emulator: TerminalEmulator::new(geometry(columns, rows)).unwrap(),
            encoder: ScreenEncoder::default(),
            decoder: ScreenDecoder::default(),
            presenter: Presenter::default(),
            appearance: crate::terminal::test_terminal_appearance_update(),
            screen: None,
            generation: 0,
            frame: 0,
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
            accessibility_revision: self.generation / 3,
        }
    }

    /// Presents the emulator's state against `base` through the wire, and checks that the
    /// decoder then holds exactly the server's screen.
    fn sync_against(&mut self, base: Option<u64>) -> Option<&DecodedScreen> {
        let (capture, frame, update) = self.next_against(base)?;
        let decoded = self.decoder.apply(frame, base, update, None).unwrap();
        assert_same(decoded, &capture, &mut self.presenter, &self.appearance);
        Some(decoded)
    }

    /// The emulator's state as the next frame against `base`, after the wire.
    fn next_against(&mut self, base: Option<u64>) -> Option<(ScreenCapture, u64, ScreenUpdate)> {
        let capture = self.capture();
        let frame = self.frame + 1;
        let update = self.encoder.encode(&capture, frame, base)?;
        self.frame = frame;
        Some((capture, frame, through_wire(frame, base, update)))
    }

    fn sync(&mut self) -> Option<&DecodedScreen> {
        let base = self.decoder.frame();
        self.sync_against(base)
    }
}

fn through_wire(frame: u64, base: Option<u64>, update: ScreenUpdate) -> ScreenUpdate {
    let wire = encode_server(&ServerMessage::Frame(Box::new(Frame {
        channel: Channel(1),
        frame,
        base,
        input: 0,
        update,
    })))
    .unwrap();
    let mut assembler = Assembler::default();
    let mut decoded = None;
    for bytes in &wire.frames {
        decoded = assembler.server(RawFrame::parse(bytes).unwrap()).unwrap();
    }
    match decoded {
        Some(ServerMessage::Frame(frame)) => frame.update,
        _ => panic!("the frame did not survive the wire"),
    }
}

/// What the wire carries of one cell. File links need local authority, which these tests do not
/// grant, so they arrive without a target.
#[derive(Debug, PartialEq)]
struct SentCell {
    text: String,
    colors: [TerminalColor; 3],
    flags: [bool; 10],
    underline: TerminalUnderlineSnapshot,
    link: Option<String>,
}

fn sent(cell: &CellSnapshot) -> SentCell {
    let mut text = if cell.spacer_tail {
        " ".to_owned()
    } else {
        cell.text.to_string()
    };
    if text.len() > limits::MAX_CELL_BYTES {
        let mut end = limits::MAX_CELL_BYTES;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        text.truncate(end);
    }
    SentCell {
        text,
        colors: [
            cell.foreground_source,
            cell.background_source,
            cell.underline_source,
        ],
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
            cell.spacer_tail,
        ],
        underline: cell.underline,
        link: cell
            .hyperlink
            .as_ref()
            .filter(|link| link.kind == crate::runtime::HyperlinkKind::Url)
            .map(|link| link.value.clone()),
    }
}

fn assert_same(
    decoded: &DecodedScreen,
    capture: &ScreenCapture,
    presenter: &mut Presenter,
    appearance: &TerminalAppearanceUpdate,
) {
    let screen = capture.screen.as_ref();
    assert_eq!(decoded.rows.len(), screen.rows.len());
    for (index, (decoded, expected)) in decoded.rows.iter().zip(screen.rows.iter()).enumerate() {
        let same = decoded.len() == expected.len()
            && decoded
                .iter()
                .zip(expected.iter())
                .all(|(decoded, expected)| sent(decoded) == sent(expected));
        if !same {
            assert_eq!(
                decoded.iter().map(sent).collect::<Vec<_>>(),
                expected.iter().map(sent).collect::<Vec<_>>(),
                "row {index}"
            );
        }
    }
    assert_eq!(decoded.wrapped.as_ref(), screen.row_soft_wrapped.as_ref());
    assert_eq!(decoded.graphics, Some(sent_graphics(&screen.graphics)));
    assert_eq!(
        (decoded.geometry.columns, decoded.geometry.rows),
        (screen.size.cols, screen.size.rows)
    );
    let history = capture.screen.history;
    assert_eq!(
        (
            decoded.history.epoch,
            decoded.history.oldest,
            decoded.history.total
        ),
        (history.epoch, history.oldest, history.total)
    );
    assert_eq!(decoded.generation, capture.generation);
    assert_eq!(
        decoded.accessibility_revision,
        capture.accessibility_revision
    );
    let rendered = presenter.snapshot(decoded, PresentationGeneration::test(1), appearance);
    assert!(Arc::ptr_eq(&rendered.rows, &decoded.rows));
    assert_eq!(rendered.cursor.position, screen.cursor.position);
    assert_eq!(rendered.cursor.visible, screen.cursor.visible);
    assert_eq!(rendered.cursor.blinking, screen.cursor.blinking);
    assert_eq!(rendered.cursor.shape, screen.cursor.shape);
    assert_eq!(rendered.active_screen, screen.active_screen);
    assert_eq!(rendered.mouse_tracking, screen.mouse_tracking);
    assert_eq!(rendered.alternate_scroll, screen.alternate_scroll);
    assert_eq!(rendered.colors.reversed, screen.colors.reversed);
    // The client's Scrollback view sets the viewport, scrollbar, Selection and Find.
    assert_eq!(rendered.history, screen.history);
    assert_eq!(rendered.text_blinking, screen.text_blinking);
    let colors = &screen.colors;
    assert_eq!(rendered.colors.foreground_source, colors.foreground_source);
    assert_eq!(rendered.colors.background_source, colors.background_source);
    assert_eq!(rendered.colors.cursor_source, colors.cursor_source);
    assert_eq!(rendered.colors.palette_overrides, colors.palette_overrides);
    if colors.foreground_source == TerminalDefaultColorSource::ProgramOverride {
        assert_eq!(rendered.colors.foreground, colors.foreground);
    }
    if colors.background_source == TerminalDefaultColorSource::ProgramOverride {
        assert_eq!(rendered.colors.background, colors.background);
    }
    if colors.cursor_source == TerminalDefaultColorSource::ProgramOverride {
        assert_eq!(rendered.cursor.color, screen.cursor.color);
    }
    for index in 16..256 {
        assert_eq!(rendered.colors.palette[index], colors.palette[index]);
    }
    for index in (0..16).filter(|index| colors.palette_overrides[*index]) {
        assert_eq!(rendered.colors.palette[index], colors.palette[index]);
    }
}

/// The graphics manifest a frame carries for `graphics`.
fn sent_graphics(
    graphics: &crate::terminal::GraphicsSnapshot,
) -> Result<pane::graphics::Snapshot, pane::graphics::Error> {
    if graphics.images.len() > pane::graphics::MAX_IMAGES
        || graphics.placements.len() > pane::graphics::MAX_PLACEMENTS
    {
        return Err(pane::graphics::Error::TooLarge);
    }
    let snapshot = pane::graphics::Snapshot {
        generation: graphics.generation,
        placement_generation: graphics.placement_generation,
        images: graphics
            .images
            .iter()
            .map(|image| pane::graphics::Image {
                key: image.key,
                width: image.width,
                height: image.height,
            })
            .collect(),
        placements: graphics.placements.to_vec(),
    };
    snapshot.validate()?;
    Ok(snapshot)
}

/// A small deterministic generator, so every run replays the same workload.
struct Random(u64);

impl Random {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, bound: u64) -> u64 {
        self.next() % bound
    }
}

/// One step of terminal output that exercises a part of the encoding.
fn step(random: &mut Random, session: &mut Session) {
    let rows = session.emulator.geometry().grid().rows;
    let columns = session.emulator.geometry().grid().cols;
    let output = match random.below(22) {
        0 => ["hello ", "world", "x", " $ ", "ls -la"][random.below(5) as usize].to_owned(),
        1 => "界面宽字".to_owned(),
        2 => ["👨‍👩‍👧", "✅️", "🙂", "🇯🇵"][random.below(4) as usize].to_owned(),
        3 => format!("e{}a\u{30d}", "\u{301}".repeat(random.below(40) as usize)),
        4 => {
            let n = random.below(256);
            let (r, g, b) = (random.below(256), random.below(256), random.below(256));
            [
                format!("\x1b[3{}m", random.below(8)),
                format!("\x1b[9{}m", random.below(8)),
                format!("\x1b[38;5;{n}m"),
                format!("\x1b[38;2;{r};{g};{b}m"),
                format!("\x1b[48;5;{n}m"),
                format!("\x1b[48;2;{r};{g};{b}m"),
                format!("\x1b[58;5;{n}m"),
                format!("\x1b[58;2;{r};{g};{b}m"),
                [
                    "\x1b[1m",
                    "\x1b[2m",
                    "\x1b[3m",
                    "\x1b[4m",
                    "\x1b[4:3m",
                    "\x1b[21m",
                ][random.below(6) as usize]
                    .to_owned(),
                [
                    "\x1b[5m",
                    "\x1b[7m",
                    "\x1b[8m",
                    "\x1b[9m",
                    "\x1b[53m",
                    "\x1b[4:5m",
                ][random.below(6) as usize]
                    .to_owned(),
                "\x1b[0m".to_owned(),
            ][random.below(11) as usize]
                .clone()
        }
        5 => "\r\n".to_owned(),
        6 => format!(
            "\x1b[{};{}H",
            random.below(u64::from(rows)) + 1,
            random.below(u64::from(columns)) + 1
        ),
        7 => ["\x1b[K", "\x1b[1K", "\x1b[J", "\x1b[2J", "\x1b[X"][random.below(5) as usize]
            .to_owned(),
        8 => {
            let top = random.below(u64::from(rows) / 2) + 1;
            let bottom = top + 1 + random.below(u64::from(rows) - top);
            format!(
                "\x1b[{top};{bottom}r\x1b[{bottom};1H{}",
                "\nscrolled".repeat(random.below(4) as usize + 1)
            )
        }
        9 => format!(
            "\x1b[r\x1b[1;1H{}",
            "\x1bM".repeat(random.below(3) as usize + 1)
        ),
        10 => ["\x1b[?1049h", "\x1b[?1049l"][random.below(2) as usize].to_owned(),
        11 => format!(
            "\x1b]8;;https://example.com/{}\x1b\\link\x1b]8;;\x1b\\ https://spaceterm.dev/{} ",
            random.below(5),
            random.below(3)
        ),
        12 => [
            "\x1b[?25l",
            "\x1b[?25h",
            "\x1b[1 q",
            "\x1b[2 q",
            "\x1b[3 q",
            "\x1b[4 q",
            "\x1b[5 q",
            "\x1b[6 q",
        ][random.below(8) as usize]
            .to_owned(),
        13 => [
            "\x1b[?1000h",
            "\x1b[?1000l",
            "\x1b[?1007h",
            "\x1b[?1007l",
            "\x1b[?5h",
            "\x1b[?5l",
        ][random.below(6) as usize]
            .to_owned(),
        14 => {
            let color = format!(
                "rgb:{:02x}/{:02x}/{:02x}",
                random.below(256),
                random.below(256),
                random.below(256)
            );
            match random.below(6) {
                0 => format!("\x1b]4;{};{color}\x1b\\", random.below(256)),
                1 => format!("\x1b]10;{color}\x1b\\"),
                2 => format!("\x1b]11;{color}\x1b\\"),
                3 => format!("\x1b]12;{color}\x1b\\"),
                4 => "\x1b]104\x1b\\".to_owned(),
                _ => "\x1b]110\x1b\\\x1b]111\x1b\\\x1b]112\x1b\\".to_owned(),
            }
        }
        15 => {
            let columns = 2 + random.below(60) as u16;
            let rows = 2 + random.below(20) as u16;
            session.emulator.resize(geometry(columns, rows)).unwrap();
            String::new()
        }
        16 => (0..random.below(12))
            .map(|line| format!("output line {line}\r\n"))
            .collect(),
        17 => format!(
            "\x1b[{}{}",
            random.below(3) + 1,
            ["L", "M", "@", "P", "S", "T"][random.below(6) as usize]
        ),
        // An accessible Selection, which the server presents in the cells' style.
        18 => {
            let line = random.below(u64::from(rows)) as usize;
            let start = random.below(u64::from(columns)) as u16;
            let end = random.below(u64::from(columns)) as u16;
            let _ = session
                .emulator
                .select_visible((line, start.min(end)), (line, start.max(end)));
            String::new()
        }
        // Clearing the Scrollback, which moves the history's oldest position.
        19 => "\x1b[3J".to_owned(),
        // An image placement, which changes the graphics manifest.
        20 => format!(
            "\x1b_Ga=T,t=d,f=32,i={},s=1,v=1,c=2,r=1,q=2;AQIDBA==\x1b\\",
            random.below(3) + 1
        ),
        _ => "abcdefghijklmnopqrstuvwxyz0123456789".repeat(random.below(4) as usize),
    };
    session.emulator.feed(output.as_bytes());
}

#[test]
fn random_terminal_output_round_trips_through_every_frame() {
    let _graphics = crate::terminal::graphics::test_lock();
    // Deltas that show a selection, find results or images, most of them carried forward.
    let (mut selected, mut images) = (0, 0);
    for seed in 1..=10_u64 {
        let mut random = Random(seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1);
        let mut session = Session::new(2 + random.below(80) as u16, 2 + random.below(30) as u16);
        session.sync().unwrap();
        for _ in 0..100 {
            step(&mut random, &mut session);
            match random.below(20) {
                // A keyframe resets the channel's tables.
                0 => {
                    session.sync_against(None).unwrap();
                }
                // A new encoder does not know the client's frame, so it sends every row.
                1 => {
                    session.encoder = ScreenEncoder::default();
                    session.sync();
                }
                _ => {
                    if let Some(decoded) = session.sync() {
                        selected += usize::from(
                            decoded
                                .rows
                                .iter()
                                .any(|row| row.iter().any(|cell| cell.selected)),
                        );
                        images += usize::from(decoded.graphics.as_ref().is_some_and(|graphics| {
                            graphics.as_ref().is_ok_and(|g| !g.images.is_empty())
                        }));
                    }
                }
            }
        }
    }
    assert!(selected > 0 && images > 0, "{selected} {images}");
}

#[test]
fn unchanged_rows_keep_their_pointers_and_scrolled_rows_move_with_them() {
    let mut session = Session::new(30, 6);
    session
        .emulator
        .feed(b"zero\r\none\r\ntwo\r\nthree\r\nfour\r\nfive");
    let before = Arc::clone(&session.sync().unwrap().rows);
    session.emulator.feed(b"\x1b[2;1Hchanged");
    let after = Arc::clone(&session.sync().unwrap().rows);
    for index in [0, 2, 3, 4, 5] {
        assert!(Arc::ptr_eq(&before[index], &after[index]), "row {index}");
    }
    assert!(!Arc::ptr_eq(&before[1], &after[1]));
    session.emulator.feed(b"\x1b[6;1H\n\nnew");
    let scrolled = Arc::clone(&session.sync().unwrap().rows);
    for index in 0..3 {
        assert!(
            Arc::ptr_eq(&after[index + 2], &scrolled[index]),
            "row {index}"
        );
    }
    // A cursor-only change keeps the whole row list.
    session.emulator.feed(b"\x1b[1;1H");
    let moved = Arc::clone(&session.sync().unwrap().rows);
    assert!(Arc::ptr_eq(&scrolled, &moved));
}

#[test]
fn an_unknown_base_empties_the_decoder_so_the_next_request_asks_for_a_keyframe() {
    let mut session = Session::new(10, 3);
    session.emulator.feed(b"text");
    session.sync().unwrap();
    session.emulator.feed(b"more");
    let capture = session.capture();
    let update = session.encoder.encode(&capture, 2, Some(1)).unwrap();
    assert_eq!(
        session
            .decoder
            .apply(2, Some(9), update.clone(), None)
            .err(),
        Some(UpdateError::UnknownBase)
    );
    assert_eq!(session.decoder.frame(), None);
    assert_eq!(
        session.decoder.apply(2, Some(1), update, None).err(),
        Some(UpdateError::UnknownBase)
    );
    session.frame = 2;
    session.sync().unwrap();
    assert_eq!(session.decoder.frame(), Some(3));
}

/// The decoder refuses these by itself: they name what its tables or grid do not hold, or cells
/// outside the run's text.
#[test]
fn malformed_updates_are_refused_and_reset_the_decoder() {
    let mut session = Session::new(10, 3);
    session.emulator.feed(b"\x1b[31mred");
    session.sync().unwrap();
    let delta = |rows: Vec<Row>| ScreenUpdate {
        rows,
        ..ScreenUpdate::default()
    };
    let run = |style: StyleRef, link: Option<u16>, text: &str, cells: Cells| Run {
        style,
        link,
        text: text.into(),
        cells,
    };
    let row = |runs: Vec<Run>| Row {
        index: 0,
        wrapped: false,
        runs,
    };
    let cases = [
        // An undefined style or link.
        delta(vec![row(vec![run(
            StyleRef::Id(9),
            None,
            "x",
            Cells::Narrow,
        )])]),
        delta(vec![row(vec![run(
            StyleRef::Default,
            Some(3),
            "x",
            Cells::Narrow,
        )])]),
        // More cells than columns.
        delta(vec![row(vec![run(
            StyleRef::Default,
            None,
            "elevencells",
            Cells::Narrow,
        )])]),
        // Lengths that split a character or run past the text.
        delta(vec![row(vec![run(
            StyleRef::Default,
            None,
            "界",
            Cells::Explicit(vec![1, 2]),
        )])]),
        delta(vec![row(vec![run(
            StyleRef::Default,
            None,
            "ab",
            Cells::Explicit(vec![1, 2]),
        )])]),
        // A row or a scroll outside the grid.
        delta(vec![Row {
            index: 3,
            wrapped: false,
            runs: Vec::new(),
        }]),
        ScreenUpdate {
            shift: Some(ScrollShift {
                top: 0,
                bottom: 4,
                lines: 1,
            }),
            ..ScreenUpdate::default()
        },
    ];
    for update in cases {
        let base = session.decoder.frame();
        assert!(base.is_some());
        let frame = session.frame + 1;
        assert_eq!(
            session.decoder.apply(frame, base, update, None).err(),
            Some(UpdateError::Malformed)
        );
        assert_eq!(session.decoder.frame(), None);
        session.sync_against(None).unwrap();
    }
    // A keyframe without geometry, modes and colors.
    let frame = session.frame + 1;
    assert_eq!(
        session
            .decoder
            .apply(frame, None, ScreenUpdate::default(), None)
            .err(),
        Some(UpdateError::Malformed)
    );
}

#[test]
fn pages_round_trip_with_their_own_tables() {
    let mut session = Session::new(40, 5);
    session.emulator.feed(
        "\x1b[1;31mbold red\x1b[0m 界\r\n\x1b]8;;https://example.com\x1b\\link\x1b]8;;\x1b\\\r\n👨‍👩‍👧 e\u{301}\r\n"
            .as_bytes(),
    );
    session.emulator.feed(&b"w".repeat(45));
    let screen = session.capture().screen;
    let page = encode_page(
        2,
        700,
        screen
            .rows
            .iter()
            .zip(screen.row_soft_wrapped.iter())
            .map(|(row, wrapped)| (row.as_ref(), *wrapped)),
    );
    let decoded = decode_page(&page, 40, None).unwrap();
    assert_eq!(decoded.len(), 5);
    for ((row, wrapped), (expected, expected_wrapped)) in decoded
        .iter()
        .zip(screen.rows.iter().zip(screen.row_soft_wrapped.iter()))
    {
        assert_eq!(wrapped, expected_wrapped);
        assert_eq!(
            row.iter().map(sent).collect::<Vec<_>>(),
            expected.iter().map(sent).collect::<Vec<_>>()
        );
    }
    assert!(decoded[3].1);
    assert_eq!(
        decode_page(&page, 4, None).err(),
        Some(UpdateError::Malformed)
    );
}

#[test]
fn snapshots_keep_program_colors_and_project_client_defaults() {
    let mut appearance = crate::terminal::test_terminal_appearance_update();
    let configured = Arc::make_mut(&mut appearance.appearance);
    configured.colors.background = crate::appearance::Color::rgb(0xffffff);
    configured.colors.normal[0] = crate::appearance::Color::rgb(0x112233);
    let link = |id, link| spaceterm_server::protocol::screen::LinkDefinition { id, link };
    let run = |link| Run {
        style: StyleRef::Default,
        link: Some(link),
        text: "x".into(),
        cells: Cells::Narrow,
    };
    let update = ScreenUpdate {
        geometry: Some(spaceterm_server::protocol::screen::Geometry {
            columns: 2,
            rows: 2,
            cell_pixels: None,
        }),
        modes: Some(Default::default()),
        colors: Some(spaceterm_server::protocol::screen::Colors {
            foreground: [10, 20, 30],
            background: [0, 0, 0],
            cursor: [0, 0, 0],
            cursor_text: [0, 0, 0],
            palette: vec![[40, 50, 60]; 256],
            foreground_overridden: true,
            background_overridden: false,
            cursor_overridden: false,
            palette_overrides: [0b10, 0, 0, 0],
        }),
        links: vec![
            link(
                0,
                pane::effects::Hyperlink::Url("https://example.test".into()),
            ),
            link(1, pane::effects::Hyperlink::File("/etc/passwd".into())),
        ],
        rows: vec![Row {
            index: 0,
            wrapped: false,
            runs: vec![run(0), run(1)],
        }],
        ..ScreenUpdate::default()
    };
    let mut decoder = ScreenDecoder::default();
    let decoded = decoder.apply(1, None, update, None).unwrap();
    // A file link needs local authority, which this decoder was not given.
    assert_eq!(
        decoded.rows[0][0].hyperlink.as_ref().unwrap().kind,
        crate::runtime::HyperlinkKind::Url
    );
    assert!(decoded.rows[0][1].hyperlink.is_none());
    let screen =
        Presenter::default().snapshot(decoded, PresentationGeneration::test(1), &appearance);
    assert_eq!(
        screen.colors.foreground,
        crate::appearance::Color::rgb(0x0a141e)
    );
    assert_eq!(screen.background, crate::appearance::Color::rgb(0xffffff));
    assert_eq!(
        screen.colors.palette[0],
        crate::appearance::Color::rgb(0x112233)
    );
    assert_eq!(
        screen.colors.palette[1],
        crate::appearance::Color::rgb(0x28323c)
    );
}

#[test]
fn truecolor_beyond_the_style_table_keeps_every_color() {
    let mut session = Session::new(512, 12);
    for row in 0..12 {
        let mut line = format!("\x1b[{};1H", row + 1);
        for column in 0..512 {
            let value = row * 512 + column;
            line.push_str(&format!(
                "\x1b[48;2;{};{};{}m ",
                value % 256,
                value / 256 % 256,
                value / 65_536
            ));
        }
        session.emulator.feed(line.as_bytes());
        session.sync().unwrap();
    }
    session.emulator.feed(b"\x1b[0m\x1b[1;1Hx");
    session.sync().unwrap();
}

#[test]
fn a_frame_that_changes_only_metadata_allocates_nothing() {
    let mut session = Session::new(80, 24);
    session.emulator.feed(b"\x1b[31mhello\r\n\x1b[5mworld");
    session.sync().unwrap();
    session.emulator.feed(b"\x1b[3;5H");
    let base = session.decoder.frame();
    let (_, frame, update) = session.next_against(base).unwrap();
    assert!(update.rows.is_empty() && update.shift.is_none() && update.colors.is_none());
    let (applied, allocations) = count_allocations(|| {
        session
            .decoder
            .apply(frame, base, update, None)
            .map(|decoded| decoded.cursor.position)
    });
    assert!(applied.unwrap().is_some());
    assert_eq!(allocations.count, 0);
}

#[test]
fn a_changed_row_allocates_nothing_per_cell() {
    let mut session = Session::new(200, 3);
    session.emulator.feed(b"warm\r\n");
    // The renderer still holds the rows of the screen it draws.
    let _drawn = Arc::clone(&session.sync().unwrap().rows);
    let line = "ab \u{754c}\u{9762} e\u{301}\u{30d} \u{1f468}\u{200d}\u{1f469}\u{200d}\u{1f467} \u{1f642} ";
    session.emulator.feed(line.repeat(8).as_bytes());
    let base = session.decoder.frame();
    let (_, frame, update) = session.next_against(base).unwrap();
    assert_eq!(update.rows.len(), 1);
    let (applied, allocations) = count_allocations(|| {
        session
            .decoder
            .apply(frame, base, update, None)
            .map(|decoded| decoded.rows[0].len())
    });
    assert_eq!(applied, Ok(200));
    // The row itself, and the copy of the row list the renderer holds.
    assert!(allocations.count <= 2, "{allocations:?}");
}

#[test]
fn consecutive_screens_share_the_projected_palette_until_its_inputs_change() {
    use spaceterm_server::protocol::screen::{Colors, CursorPosition, Geometry};
    let colors = Colors {
        foreground: [10, 20, 30],
        background: [0, 0, 0],
        cursor: [0, 0, 0],
        cursor_text: [0, 0, 0],
        palette: vec![[40, 50, 60]; 256],
        foreground_overridden: false,
        background_overridden: false,
        cursor_overridden: false,
        palette_overrides: [0, 0, 0, 0],
    };
    let keyframe = ScreenUpdate {
        geometry: Some(Geometry {
            columns: 4,
            rows: 2,
            cell_pixels: None,
        }),
        modes: Some(Default::default()),
        colors: Some(colors.clone()),
        ..ScreenUpdate::default()
    };
    let moved = |column| ScreenUpdate {
        cursor: spaceterm_server::protocol::screen::Cursor {
            position: Some(CursorPosition {
                row: 1,
                column,
                width: 1,
            }),
            ..Default::default()
        },
        ..ScreenUpdate::default()
    };
    let generation = PresentationGeneration::test;
    let mut appearance = crate::terminal::test_terminal_appearance_update();
    let mut decoder = ScreenDecoder::default();
    let mut presenter = Presenter::default();
    let decoded = decoder.apply(1, None, keyframe, None).unwrap();
    let first = presenter.snapshot(decoded, generation(1), &appearance);
    let decoded = decoder.apply(2, Some(1), moved(1), None).unwrap();
    let (second, allocations) =
        count_allocations(|| presenter.snapshot(decoded, generation(2), &appearance));
    assert_eq!(allocations.count, 0);
    assert!(Arc::ptr_eq(&first.colors.palette, &second.colors.palette));
    assert_eq!(
        second.cursor.position.map(|position| position.column),
        Some(1)
    );
    assert_eq!(second.generation, generation(2));

    // A program's palette entry and a new appearance each project a new palette.
    let decoded = decoder
        .apply(
            3,
            Some(2),
            ScreenUpdate {
                colors: Some(Colors {
                    palette_overrides: [0b100, 0, 0, 0],
                    ..colors
                }),
                ..moved(2)
            },
            None,
        )
        .unwrap();
    let third = presenter.snapshot(decoded, generation(3), &appearance);
    assert_eq!(
        third.colors.palette[2],
        crate::appearance::Color::rgb(0x28323c)
    );
    assert!(third.colors.palette_overrides[2]);
    let configured = Arc::make_mut(&mut appearance.appearance);
    configured.colors.normal[1] = crate::appearance::Color::rgb(0x112233);
    configured.colors.background = crate::appearance::Color::rgb(0x445566);
    let decoded = decoder.apply(4, Some(3), moved(3), None).unwrap();
    let fourth = presenter.snapshot(decoded, generation(4), &appearance);
    assert_eq!(
        fourth.colors.palette[1],
        crate::appearance::Color::rgb(0x112233)
    );
    assert_eq!(fourth.background, crate::appearance::Color::rgb(0x445566));
    assert_eq!(
        fourth.colors.palette[2],
        crate::appearance::Color::rgb(0x28323c)
    );
}

#[test]
fn a_program_that_changes_one_palette_entry_again_shows_its_new_color() {
    let mut session = Session::new(8, 2);
    let mut palette_entry = |osc: &[u8]| {
        session.emulator.feed(osc);
        let base = session.decoder.frame();
        let (_, frame, update) = session.next_against(base).unwrap();
        let decoded = session.decoder.apply(frame, base, update, None).unwrap();
        let rendered = session.presenter.snapshot(
            decoded,
            PresentationGeneration::test(frame),
            &session.appearance,
        );
        assert!(rendered.colors.palette_overrides[2]);
        rendered.colors.palette[2]
    };
    assert_eq!(
        palette_entry(b"\x1b]4;2;rgb:01/02/03\x1b\\"),
        crate::appearance::Color::rgb(0x010203)
    );
    assert_eq!(
        palette_entry(b"\x1b]4;2;rgb:04/05/06\x1b\\"),
        crate::appearance::Color::rgb(0x040506)
    );
}
