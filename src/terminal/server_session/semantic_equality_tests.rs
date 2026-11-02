//! The model a client publishes equals the server's (ADR 0062).
//!
//! A real Terminal Emulator builds the server's models. The server's Pane revisions and the
//! connection's outbox encode them, the frames cross the real frame codec, and the client's
//! staging applies them to its retained document. After every step the published model must
//! present exactly what the server's does, and announce the same notifications.
use super::accessibility::{RetainedSemantics, Semantics};
use super::inbox::Ack;
use super::*;
use crate::runtime::directory_testing::ServerSemantics;
use crate::terminal::accessibility::{
    AccessibilityCellRef, AccessibilitySelectionRefs, TerminalAccessibilityModel,
};
use crate::terminal::emulator::TerminalEmulator;
use crate::terminal::geometry::{BackingScale, CellGridSize, LogicalCellSize, TerminalGeometry};
use spaceterm_server::protocol::frame::Assembler;
use spaceterm_server::protocol::{RawFrame, ServerMessage};

fn geometry(columns: u16, rows: u16) -> TerminalGeometry {
    TerminalGeometry::from_grid(
        CellGridSize::new(columns, rows),
        LogicalCellSize::new(8.0, 16.0),
        BackingScale::ONE,
    )
}

/// One client channel: its demand, its staging and the document it retains for one connection.
struct Client {
    semantics: Semantics,
    retained: Mutex<RetainedSemantics>,
    generation: PresentationGeneration,
    published: Option<Arc<TerminalAccessibilityModel>>,
    /// Whether a model was published since the last comparison.
    fresh: bool,
}

impl Client {
    /// A new connection epoch: nothing retained from an earlier connection.
    fn connect() -> Self {
        Self {
            semantics: Semantics::new(),
            retained: Mutex::default(),
            generation: PresentationGeneration::default(),
            published: None,
            fresh: false,
        }
    }
}

/// A Terminal Emulator, its server channel and one client, joined by the wire.
struct Pane {
    terminal: TerminalEmulator,
    server: ServerSemantics,
    client: Client,
    assembler: Assembler,
    /// The newest model the Terminal Session built.
    model: Option<Arc<TerminalAccessibilityModel>>,
    /// The server and client models compared last, for their notifications.
    compared: Option<(
        Arc<TerminalAccessibilityModel>,
        Arc<TerminalAccessibilityModel>,
    )>,
    /// Messages and revisions that crossed the wire.
    messages: usize,
    revisions: usize,
}

impl Pane {
    fn new(columns: u16, rows: u16) -> Self {
        let mut pane = Self {
            terminal: TerminalEmulator::new(geometry(columns, rows)).expect("a terminal starts"),
            server: ServerSemantics::connect(),
            client: Client::connect(),
            assembler: Assembler::default(),
            model: None,
            compared: None,
            messages: 0,
            revisions: 0,
        };
        pane.capture();
        pane.demand(true);
        pane
    }

    /// Builds the Terminal Session's next model to completion, as its bounded capture steps do,
    /// and hands it to the server's channel.
    fn capture(&mut self) {
        let mut built = None;
        for _ in 0..4_096 {
            let (model, more) = self
                .terminal
                .accessibility_snapshot(false)
                .expect("the terminal builds semantics");
            built = model.or(built);
            if !more {
                break;
            }
        }
        if let Some(model) = built {
            self.model = Some(Arc::clone(&model));
            self.server.publish(model);
        }
    }

    fn feed(&mut self, bytes: &str) {
        self.terminal.feed(bytes.as_bytes());
        self.capture();
    }

    fn resize(&mut self, columns: u16, rows: u16) {
        self.terminal
            .resize(geometry(columns, rows))
            .expect("the terminal resizes");
        self.capture();
    }

    /// Selects from `start` to `end`, each a line and column of the server's model, as a native
    /// Selection request the server applies.
    fn select(&mut self, start: (usize, u16), end: (usize, u16)) {
        let model = self.model.clone().expect("a model to select in");
        let cell = |(line, column): (usize, u16)| {
            model.row(line).map(|row| AccessibilityCellRef {
                row: row.id,
                row_revision: row.revision,
                column,
            })
        };
        let (Some(start), Some(end)) = (cell(start), cell(end)) else {
            return;
        };
        self.terminal
            .set_accessibility_selection_refs(Some(AccessibilitySelectionRefs {
                start,
                end,
                rectangle: false,
            }))
            .expect("the terminal applies a Selection");
        self.capture();
    }

    fn deselect(&mut self) {
        self.terminal
            .set_accessibility_selection_refs(None)
            .expect("the terminal clears a Selection");
        self.capture();
    }

    /// Starts or ends demand on the client, which tells the server, as the worker does.
    fn demand(&mut self, wanted: bool) {
        let server = &mut self.server;
        self.client
            .semantics
            .follow(wanted, |demand| {
                server.demand(demand);
                Ok(())
            })
            .expect("demand follows");
        if wanted && let Some(model) = &self.model {
            // A new demand rebuilds the Terminal Session's store, which republishes its model.
            self.server.publish(Arc::clone(model));
        }
    }

    /// A new connection: the server's store and the client's retained document are gone, and
    /// the client demands again after its claim.
    fn reconnect(&mut self) {
        self.server = ServerSemantics::connect();
        self.client = Client::connect();
        self.assembler = Assembler::default();
        self.compared = None;
        self.demand(true);
    }

    fn view(&mut self, visible: bool) {
        self.server.view(visible);
    }

    /// Writes every frame the server's I/O thread has, delivers each to the client in order,
    /// and returns each acknowledgement to the server, then publishes as the worker does.
    fn pump(&mut self) {
        while let Some(frame) = self.server.next_frame() {
            let message = self
                .assembler
                .server(RawFrame::parse(&frame).expect("a whole frame"))
                .expect("the client decodes every server frame");
            let Some(ServerMessage::Accessibility(message)) = message else {
                continue;
            };
            assert_eq!(message.channel, ServerSemantics::CHANNEL);
            self.messages += 1;
            let ack = self
                .client
                .semantics
                .receive(
                    message.update,
                    &self.client.retained,
                    self.client.generation,
                )
                .expect("the client accepts what the server encodes");
            if let Some(Ack::Semantics {
                demand_generation,
                revision,
            }) = ack
            {
                self.revisions += 1;
                self.server.acknowledge(demand_generation, revision);
            }
        }
        if let Some(model) = self
            .client
            .semantics
            .publish(&self.client.retained, self.client.generation)
        {
            self.client.published = Some(model);
            self.client.fresh = true;
        }
    }

    /// Pumps, then requires the client to present what the server does.
    fn settle(&mut self, step: &str) {
        self.pump();
        let server = self.model.clone().expect("the server built a model");
        let client = self
            .client
            .published
            .clone()
            .unwrap_or_else(|| panic!("{step}: the client published no model"));
        assert_same(&server, &client, step);
        if let Some((previous_server, previous_client)) = &self.compared {
            let expected = server.notifications_since(previous_server);
            let published = if std::mem::take(&mut self.client.fresh) {
                client.notifications_since(previous_client)
            } else {
                Default::default()
            };
            assert_eq!(
                published.iter().collect::<Vec<_>>(),
                expected.iter().collect::<Vec<_>>(),
                "{step}: notifications"
            );
        }
        self.client.fresh = false;
        self.compared = Some((server, client));
    }
}

/// Requires `client` to present exactly what `server` does: the same rows with the same
/// identities, revisions, text, cells and UTF-16 ranges, the same line ranges and index lookups,
/// and the same cursor, Selection, visible lines and screen.
fn assert_same(
    server: &TerminalAccessibilityModel,
    client: &TerminalAccessibilityModel,
    step: &str,
) {
    assert_eq!(client.text(), server.text(), "{step}: text");
    assert_eq!(
        client.len_utf16(),
        server.len_utf16(),
        "{step}: UTF-16 length"
    );
    assert_eq!(
        client.rows().count(),
        server.rows().count(),
        "{step}: row count"
    );
    for (line, (theirs, ours)) in server.rows().zip(client.rows()).enumerate() {
        assert_eq!(ours.id, theirs.id, "{step}: row id at line {line}");
        assert_eq!(
            ours.revision, theirs.revision,
            "{step}: revision at line {line}"
        );
        assert_eq!(
            ours.soft_wrapped, theirs.soft_wrapped,
            "{step}: soft wrap at line {line}"
        );
        assert_eq!(ours.text, theirs.text, "{step}: text at line {line}");
        assert_eq!(
            ours.range, theirs.range,
            "{step}: UTF-16 range of line {line}"
        );
        assert_eq!(
            client.range_for_line(line),
            server.range_for_line(line),
            "{step}: line range {line}"
        );
        let cells = |row: &crate::terminal::accessibility::AccessibilityRowView<'_>| {
            row.cells()
                .map(|cell| (cell.text.to_owned(), cell.utf16, cell.columns))
                .collect::<Vec<_>>()
        };
        let expected = cells(&theirs);
        assert_eq!(cells(&ours), expected, "{step}: cells at line {line}");
        assert_eq!(
            client.line_for_index(theirs.range.start),
            server.line_for_index(theirs.range.start),
            "{step}: line of the start of line {line}"
        );
        // Every index of a row that is not one ASCII cell per column.
        if theirs.text.is_ascii()
            && expected
                .iter()
                .all(|(text, _, columns)| text.len() == 1 && columns.len() == 1)
        {
            continue;
        }
        for (_, utf16, _) in &expected {
            let index = theirs.range.start + utf16.start;
            assert_eq!(
                client.line_for_index(index),
                server.line_for_index(index),
                "{step}: line of index {index}"
            );
            assert_eq!(
                client.range_for_index(index),
                server.range_for_index(index),
                "{step}: cell range at index {index}"
            );
        }
    }
    for (line, row) in server.rows().enumerate() {
        assert_eq!(
            client.line_for_row(row.id),
            server.line_for_row(row.id),
            "{step}: line of row {line}"
        );
    }
    assert_eq!(
        client.line_for_index(server.len_utf16()),
        server.line_for_index(server.len_utf16()),
        "{step}: line of the end"
    );
    assert_eq!(
        client.cursor_cell(),
        server.cursor_cell(),
        "{step}: cursor cell"
    );
    assert_eq!(
        client.cursor_ref(),
        server.cursor_ref(),
        "{step}: cursor reference"
    );
    assert_eq!(
        client.cursor_range(),
        server.cursor_range(),
        "{step}: cursor range"
    );
    assert_eq!(
        client.selection_refs(),
        server.selection_refs(),
        "{step}: Selection"
    );
    assert_eq!(
        client.selection_range(),
        server.selection_range(),
        "{step}: Selection range"
    );
    assert_eq!(
        client.visible_lines(),
        server.visible_lines(),
        "{step}: visible lines"
    );
    assert_eq!(
        client.visible_range(),
        server.visible_range(),
        "{step}: visible range"
    );
    assert_eq!(
        client.active_screen(),
        server.active_screen(),
        "{step}: screen"
    );
    assert_eq!(
        client.screen_generation(),
        server.screen_generation(),
        "{step}: screen generation"
    );
}

/// A small deterministic generator, so a failing seed reproduces.
struct Random(u64);

impl Random {
    fn next(&mut self) -> u64 {
        // xorshift64*
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }

    fn below(&mut self, bound: u64) -> u64 {
        self.next() % bound
    }

    fn pick<'a>(&mut self, items: &[&'a str]) -> &'a str {
        items[self.below(items.len() as u64) as usize]
    }
}

/// Text that exercises every cell form: ASCII, wide characters, combining marks, emoji
/// sequences, a cluster at the longest grapheme Ghostty stores, and blanks.
const WORDS: &[&str] = &[
    "ls",
    "-la",
    "src/",
    "漢字",
    "ｗｉｄｅ",
    "e\u{301}",
    "a\u{308}\u{323}",
    "\u{1f600}",
    "\u{1f469}\u{200d}\u{1f4bb}",
    "\u{2764}\u{fe0f}",
    "  ",
    "\t",
];

fn longest_grapheme() -> String {
    format!("e{}", "\u{301}".repeat(64))
}

/// One random terminal or client step, named for failure messages.
fn random_step(pane: &mut Pane, random: &mut Random, step: usize) -> String {
    match random.below(20) {
        0..=5 => {
            let words = 1 + random.below(6);
            let mut line = String::new();
            for _ in 0..words {
                line.push_str(random.pick(WORDS));
                line.push(' ');
            }
            if random.below(8) == 0 {
                line.push_str(&longest_grapheme());
            }
            line.push_str(&format!("{step}\r\n"));
            pane.feed(&line);
            "a line of output".into()
        }
        6 => {
            // A line longer than the screen, which soft-wraps and reflows.
            let line = "wrapped words ".repeat(1 + random.below(8) as usize);
            pane.feed(&format!("{line}\r\n"));
            "a soft-wrapped line".into()
        }
        7 => {
            let (row, column) = (1 + random.below(8), 1 + random.below(30));
            pane.feed(&format!("\x1b[{row};{column}H{}", random.pick(WORDS)));
            "a cursor move and an overwrite".into()
        }
        8 => {
            pane.feed(random.pick(&["\x1b[K", "\x1b[1K", "\x1b[J", "\x1b[2K"]));
            "an erase".into()
        }
        9 => {
            pane.feed(random.pick(&["\x1b[2L", "\x1b[M", "\x1b[3S", "\x1b[T"]));
            "lines inserted, deleted or scrolled".into()
        }
        10 => {
            let (columns, rows) = (8 + random.below(40) as u16, 3 + random.below(10) as u16);
            pane.resize(columns, rows);
            format!("a resize to {columns}x{rows}")
        }
        11 => {
            if pane.model.as_ref().is_some_and(|model| {
                model.active_screen()
                    == crate::terminal::accessibility::AccessibilityScreen::Primary
            }) {
                pane.feed("\x1b[?1049h\x1b[Halternate screen\r\n");
                "the alternate screen".into()
            } else {
                pane.feed("\x1b[?1049l");
                "back to the primary screen".into()
            }
        }
        12 | 13 => {
            let lines = pane.model.as_ref().map_or(0, |model| model.rows().count());
            if lines == 0 {
                return "no Selection without rows".into();
            }
            let start = random.below(lines as u64) as usize;
            let end = start + random.below((lines - start) as u64) as usize;
            pane.select(
                (start, random.below(6) as u16),
                (end, random.below(12) as u16),
            );
            format!("a Selection from line {start} to {end}")
        }
        14 => {
            pane.deselect();
            "no Selection".into()
        }
        15 => {
            // Several models before the client acknowledges, which the server coalesces.
            for burst in 0..3 {
                pane.terminal
                    .feed(format!("burst {step}.{burst}\r\n").as_bytes());
                pane.capture();
            }
            "a burst of revisions".into()
        }
        16 => {
            pane.view(false);
            pane.feed(&format!("while hidden {step}\r\n"));
            pane.pump();
            pane.feed("\x1b[A\x1b[2Kedited while hidden\r\n");
            pane.view(true);
            "a hidden Pane shown again".into()
        }
        17 => {
            pane.demand(false);
            pane.pump();
            pane.feed(&format!("between demands {step}\r\n"));
            pane.demand(true);
            "demand restarted".into()
        }
        18 => {
            pane.feed(&format!("before the reconnect {step}\r\n"));
            pane.reconnect();
            "a reconnect".into()
        }
        _ => {
            pane.feed("\x1b[3J");
            "Scrollback cleared".into()
        }
    }
}

#[test]
fn the_published_model_equals_the_servers_across_random_terminal_histories() {
    let mut coverage = [0_usize; 2];
    for seed in 1..=24_u64 {
        let mut random = Random(seed.wrapping_mul(0x9e37_79b9_7f4a_7c15));
        let mut pane = Pane::new(20 + random.below(30) as u16, 4 + random.below(8) as u16);
        pane.feed("$ start\r\n");
        pane.settle(&format!("seed {seed}: first document"));
        for step in 0..60 {
            let name = random_step(&mut pane, &mut random, step);
            pane.settle(&format!("seed {seed} step {step}: {name}"));
        }
        coverage[0] += pane.messages;
        coverage[1] += pane.revisions;
    }
    assert!(
        coverage[1] > 24 * 30,
        "most steps crossed the wire: {coverage:?}"
    );
}

/// Scrollback trims at its cap a page at a time while output appends, and the document then
/// reflows; every revision on the way stays equal.
#[test]
fn the_published_model_equals_the_servers_while_scrollback_trims_at_its_cap_and_reflows() {
    let mut pane = Pane::new(40, 6);
    let mut history = String::new();
    for line in 0..10_400 {
        history.push_str(&format!("history {line:05}\r\n"));
    }
    for line in 0..40 {
        history.push_str(&format!(
            "recent {line:02} 漢字 e\u{301} \u{1f469}\u{200d}\u{1f4bb}\r\n"
        ));
    }
    pane.feed(&history);
    pane.settle("a full Scrollback");
    let full = pane.model.as_ref().unwrap().rows().count();
    assert!(full > 9_000, "{full} rows");
    let first = pane.model.as_ref().unwrap().row(0).unwrap().id;

    let mut trimmed = false;
    for round in 0..8 {
        let mut more = String::new();
        for line in 0..200 {
            more.push_str(&format!("appended {round:02}.{line:03}\r\n"));
        }
        more.push_str("ｗｉｄｅ a\u{308}\r\n");
        pane.feed(&more);
        pane.settle(&format!("append round {round}"));
        trimmed |= pane.model.as_ref().unwrap().line_for_row(first).is_none();
    }
    assert!(trimmed, "the oldest history row was trimmed at the cap");

    pane.select((3, 2), (9_000, 5));
    pane.settle("a Selection across trimmed history");
    pane.resize(23, 6);
    pane.settle("a reflow to 23 columns");
    pane.resize(61, 9);
    pane.settle("a reflow to 61 columns");
    pane.reconnect();
    pane.settle("a reconnect with a full Scrollback");
}

/// The rows a server's document reaches at the narrowest Pane stay within half the wire's
/// topology bound, so a client never refuses a real document.
#[test]
fn the_narrowest_panes_full_scrollback_stays_within_the_topology_bound() {
    let mut terminal = TerminalEmulator::new(geometry(2, 2)).unwrap();
    let mut most = 0;
    for _ in 0..100 {
        terminal.feed(&b"x\r\n".repeat(300));
        let snapshot = terminal.snapshot().unwrap().unwrap();
        most = most.max(snapshot.scrollbar.total_rows);
    }
    assert!(most > 10_000, "Scrollback reached its cap: {most} rows");
    assert!(
        most <= u64::from(pane::accessibility::MAX_TOPOLOGY_ROWS) / 2,
        "{most} rows leave the bound no margin"
    );
}
