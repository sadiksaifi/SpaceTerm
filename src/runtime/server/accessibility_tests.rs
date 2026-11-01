use super::tests::{attach, command, host, size};
use super::*;
use crate::terminal::accessibility::{
    AccessibilityCell, AccessibilityRowUpdate, AccessibilitySelectionRefs, AccessibilityUpdate,
    TerminalAccessibilityState,
};
use pane::accessibility as wire;
use spaceterm_server::connection::outbox::{Outbox, SemanticTicket, Semantics};
use spaceterm_server::protocol::frame::Assembler;
use spaceterm_server::protocol::{AccessibilityMessage, Channel, RawFrame, ServerMessage, limits};
use std::collections::HashMap;
use std::ops::Range;
use std::time::Instant;

/// One row as a client retains it.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Row {
    revision: u64,
    soft_wrapped: bool,
    text: String,
    /// Each cell's columns and byte range in `text`.
    cells: Vec<(Range<u16>, Range<usize>)>,
}

/// A row identity as a map key.
type Key = (bool, u64, u64, u16);

fn key(id: &wire::RowId) -> Key {
    (
        id.screen == wire::Screen::Alternate,
        id.screen_generation,
        id.node_serial,
        id.page_row,
    )
}

/// A client's view of one channel's semantics, applied from pushed messages as ADR 0062 says a
/// client applies them, with the checks a client makes.
#[derive(Clone, Debug, Default)]
struct Client {
    applied: Option<u64>,
    screen: Option<(wire::Screen, u64)>,
    topology: Vec<wire::RowId>,
    rows: HashMap<Key, Row>,
    tail: Option<wire::Tail>,
    staged: Option<Box<Staged>>,
}

/// A revision a client stages until its tail: its header, the document it builds and the row
/// fragment that continues into the next message.
#[derive(Clone, Debug)]
struct Staged {
    header: wire::Header,
    document: Client,
    fragment: Option<(u32, Row)>,
}

impl Client {
    /// Applies one message. Returns whether it completed a revision.
    fn apply(&mut self, update: wire::Update) -> bool {
        update.validate().expect("a message within its bounds");
        if self.staged.is_none() {
            let start = match update.header.base {
                None => Self::default(),
                Some(base) => {
                    assert_eq!(
                        Some(base),
                        self.applied,
                        "a delta applies to the applied revision"
                    );
                    self.clone()
                }
            };
            self.staged = Some(Box::new(Staged {
                header: update.header,
                document: start,
                fragment: None,
            }));
        }
        let Staged {
            header,
            document,
            fragment,
        } = &mut **self.staged.as_mut().expect("staged above");
        assert_eq!(*header, update.header, "every message repeats the header");
        let header = *header;
        for splice in update.splices {
            let at = splice.at as usize;
            let ids = splice.insert.iter().flat_map(|run| {
                (0..run.count).map(move |offset| wire::RowId {
                    screen: header.screen,
                    screen_generation: header.screen_generation,
                    node_serial: run.node_serial,
                    page_row: run.page_row + offset,
                })
            });
            document
                .topology
                .splice(at..at + splice.remove as usize, ids.collect::<Vec<_>>());
        }
        for row in update.rows {
            let mut whole = match fragment.take() {
                Some((line, mut joined)) => {
                    assert_eq!(line, row.line, "a fragment continues its line");
                    let offset = joined.text.len();
                    joined.text.push_str(&row.text);
                    joined.cells.extend(row.cells().map(|(columns, bytes)| {
                        (columns, bytes.start + offset..bytes.end + offset)
                    }));
                    joined
                }
                None => Row {
                    revision: row.revision,
                    soft_wrapped: row.soft_wrapped,
                    text: row.text.clone(),
                    cells: row.cells().collect(),
                },
            };
            whole.revision = row.revision;
            if row.continues {
                *fragment = Some((row.line, whole));
            } else {
                let id = key(&document.topology[row.line as usize]);
                document.rows.insert(id, whole);
            }
        }
        let Some(tail) = update.tail else {
            return false;
        };
        assert!(fragment.is_none(), "no fragment continues past the tail");
        let mut document = std::mem::take(document);
        let present: std::collections::HashSet<_> = document.topology.iter().map(key).collect();
        document.rows.retain(|id, _| present.contains(id));
        assert_eq!(
            document.rows.len(),
            document.topology.len(),
            "every row of the topology has contents"
        );
        let lines = document.topology.len() as u32;
        assert!(tail.visible_lines.end <= lines);
        assert!(tail.cursor.is_none_or(|cursor| cursor.line < lines));
        document.applied = Some(header.revision);
        document.screen = Some((header.screen, header.screen_generation));
        document.tail = Some(tail);
        document.staged = None;
        *self = document;
        true
    }

    /// Applies every message of one revision.
    fn revision(&mut self, updates: Vec<wire::Update>) {
        let count = updates.len();
        for (index, update) in updates.into_iter().enumerate() {
            assert_eq!(self.apply(update), index + 1 == count);
        }
    }

    fn texts(&self) -> impl Iterator<Item = &str> {
        self.topology
            .iter()
            .map(|id| self.rows[&key(id)].text.as_str())
    }

    fn line(&self, text: &str) -> Option<usize> {
        self.texts().position(|row| row.contains(text))
    }

    /// Asserts that this client presents exactly `model`.
    fn assert_presents(&self, model: &TerminalAccessibilityModel) {
        let rows: Vec<_> = model.rows().collect();
        assert_eq!(self.topology.len(), rows.len(), "row count");
        for (line, (id, row)) in self.topology.iter().zip(&rows).enumerate() {
            assert_eq!(*id, wire_id(row.id), "row id at line {line}");
            let held = &self.rows[&key(id)];
            assert_eq!(held.revision, row.revision, "revision at line {line}");
            assert_eq!(held.soft_wrapped, row.soft_wrapped, "wrap at line {line}");
            assert_eq!(held.text, row.text, "text at line {line}");
            let mut offset = 0;
            let cells: Vec<_> = row
                .cells()
                .map(|cell| {
                    let start = offset;
                    offset += cell.text.len();
                    (cell.columns, start..offset)
                })
                .collect();
            assert_eq!(held.cells, cells, "cells at line {line}");
        }
        assert_eq!(self.tail, Some(accessibility_tail(model)));
    }
}

fn wire_id(id: AccessibilityRowId) -> wire::RowId {
    wire::RowId {
        screen: match id.screen {
            AccessibilityScreen::Primary => wire::Screen::Primary,
            AccessibilityScreen::Alternate => wire::Screen::Alternate,
        },
        screen_generation: id.screen_generation as u64,
        node_serial: id.node_serial,
        page_row: id.page_row,
    }
}

/// The tail a client expects for `model`, from the row identities its Selection names.
fn accessibility_tail(model: &TerminalAccessibilityModel) -> wire::Tail {
    let line_of = |reference: AccessibilityCellRef| {
        model
            .rows()
            .position(|row| row.id == reference.row)
            .map(|line| wire::Point {
                line: line as u32,
                column: reference.column,
            })
    };
    let visible = model.visible_lines();
    wire::Tail {
        visible_lines: visible.start as u32..visible.end as u32,
        cursor: model.cursor_cell().map(|(line, column)| wire::Point {
            line: line as u32,
            column,
        }),
        selection: model.selection_refs().map(|refs| wire::Selection {
            start: line_of(refs.start).unwrap(),
            end: line_of(refs.end).unwrap(),
            rectangle: refs.rectangle,
        }),
    }
}

fn ticket(revision: u64, base: Option<u64>) -> SemanticTicket {
    SemanticTicket {
        demand_generation: 1,
        revision,
        base,
    }
}

/// Every message of `model` encoded as one revision against `base`.
fn encode(model: &Arc<dyn Semantics>, base: Option<&Arc<dyn Semantics>>) -> Vec<wire::Update> {
    let ticket = ticket(model.revision(), base.map(|base| base.revision()));
    let Some(mut encoding) = Arc::clone(model).encode(ticket, base.cloned()) else {
        return Vec::new();
    };
    let mut updates = Vec::new();
    loop {
        let update = encoding.next();
        let last = update.tail.is_some();
        updates.push(update);
        if last {
            return updates;
        }
    }
}

/// The wire bytes of a revision's messages on channel `u32::MAX`, the widest.
fn wire_bytes(updates: &[wire::Update]) -> usize {
    updates
        .iter()
        .map(|update| {
            let message = ServerMessage::Accessibility(Box::new(AccessibilityMessage {
                channel: Channel(u32::MAX),
                update: update.clone(),
            }));
            spaceterm_server::protocol::frame::encode_server(&message)
                .unwrap()
                .bytes()
        })
        .sum()
}

/// A Pane's revisions as its channel is handed them, one per model.
struct Revisions(accessibility::Accessibility);

impl Revisions {
    fn new() -> Self {
        let mut state = accessibility::Accessibility::default();
        state.start();
        Self(state)
    }

    /// The model as the channel's next revision, or `None` when it did not advance it.
    fn next(&mut self, model: Arc<TerminalAccessibilityModel>) -> Option<Arc<dyn Semantics>> {
        self.0.update(model);
        self.0.take_changed()
    }
}

/// Builds models the way the Terminal Session does, from emulator deltas.
struct Terminal {
    state: TerminalAccessibilityState,
    revision: u64,
}

fn row_id(line: usize) -> AccessibilityRowId {
    // A page node holds 100 rows, so a topology is a few runs.
    AccessibilityRowId {
        screen: AccessibilityScreen::Primary,
        screen_generation: 0,
        node_serial: line as u64 / 100 + 1,
        page_row: (line % 100) as u16,
    }
}

fn ascii(id: AccessibilityRowId, revision: u64, text: &str) -> AccessibilityRowUpdate {
    AccessibilityRowUpdate {
        id,
        revision,
        soft_wrapped: false,
        cells: text
            .chars()
            .enumerate()
            .map(|(column, character)| {
                AccessibilityCell::at_column(character.to_string(), column as u16, 1)
            })
            .collect(),
    }
}

impl Terminal {
    fn new() -> Self {
        Self {
            state: TerminalAccessibilityState::default(),
            revision: 0,
        }
    }

    /// Applies a complete delta and returns the model it publishes.
    fn apply(
        &mut self,
        topology: Option<Vec<AccessibilityRowId>>,
        changed_rows: Vec<AccessibilityRowUpdate>,
        visible_lines: Range<usize>,
        cursor: Option<AccessibilityCellRef>,
        selection: Option<AccessibilitySelectionRefs>,
    ) -> Arc<TerminalAccessibilityModel> {
        self.revision += 1;
        self.state
            .apply(
                AccessibilityUpdate {
                    revision: self.revision,
                    screen: AccessibilityScreen::Primary,
                    screen_generation: 0,
                    complete: true,
                    more: false,
                    topology,
                    visible_lines,
                    cursor,
                    selection,
                    changed_rows,
                },
                PresentationGeneration::default(),
            )
            .expect("the delta changes the model")
    }

    /// `rows` rows of `columns` ASCII characters each.
    fn history(&mut self, rows: usize, columns: usize) -> Arc<TerminalAccessibilityModel> {
        let ids: Vec<_> = (0..rows).map(row_id).collect();
        let changed = ids
            .iter()
            .enumerate()
            .map(|(line, id)| {
                let text: String = format!("row {line:05} ")
                    .chars()
                    .cycle()
                    .take(columns)
                    .collect();
                ascii(*id, 1, &text)
            })
            .collect();
        self.apply(Some(ids), changed, rows - 24..rows, None, None)
    }
}

fn cell(id: AccessibilityRowId, column: u16) -> AccessibilityCellRef {
    AccessibilityCellRef {
        row: id,
        row_revision: 1,
        column,
    }
}

/// A cursor move, a Selection or a scroll each advance the revision, and each travels as a tail
/// alone, under 256 bytes at 10,000 rows on the widest channel.
#[test]
fn a_cursor_selection_or_scroll_only_revision_at_ten_thousand_rows_is_a_small_tail() {
    let mut terminal = Terminal::new();
    let mut revisions = Revisions::new();
    let history = terminal.history(10_000, 80);
    let base = revisions.next(history).unwrap();
    let mut client = Client::default();
    client.revision(encode(&base, None));

    let last = row_id(9_999);
    let moves = [
        (9_976..10_000, Some(cell(last, 7)), None),
        (
            9_976..10_000,
            Some(cell(last, 7)),
            Some(AccessibilitySelectionRefs {
                start: cell(row_id(17), 3),
                end: cell(row_id(9_998), 40),
                rectangle: false,
            }),
        ),
        (100..124, Some(cell(last, 7)), None),
    ];
    let mut base = base;
    for (visible, cursor, selection) in moves {
        let model = terminal.apply(None, Vec::new(), visible, cursor, selection);
        let revision = revisions
            .next(Arc::clone(&model))
            .expect("a tail-only change advances the revision");
        let updates = encode(&revision, Some(&base));
        assert_eq!(updates.len(), 1);
        assert!(updates[0].splices.is_empty(), "no topology");
        assert!(updates[0].rows.is_empty(), "no rows");
        let bytes = wire_bytes(&updates);
        assert!(bytes < 256, "{bytes} B");
        client.revision(updates);
        client.assert_presents(&model);
        base = revision;
    }
}

/// A complete 10,000 by 80 document leaves the outbox one bounded message per I/O turn, encoded
/// on the thread that writes it. Each turn allocates a bounded amount whatever the document's
/// size, so no turn holds the encoded document, and the largest turn stays under twice the
/// encoded size: an upper bound on the encoder's peak transient allocation, since each turn hands
/// its message to the writer and keeps nothing.
#[test]
fn a_complete_ten_thousand_by_eighty_document_is_written_one_bounded_message_per_turn() {
    let model = Terminal::new().history(10_000, 80);
    let semantics = Revisions::new().next(Arc::clone(&model)).unwrap();
    let channel = Channel(u32::MAX);
    let outbox = Outbox::new(Arc::new(|| {}));
    outbox.open(channel).unwrap();
    outbox.start_semantics(channel, 1).unwrap();
    outbox.semantics(channel, semantics).unwrap();

    let mut assembler = Assembler::default();
    let mut client = Client::default();
    let (mut encoded, mut allocated, mut largest, mut turns) = (0, 0, 0, 0);
    loop {
        let (frame, counted) = crate::terminal::testing::count_allocations(|| outbox.next_frame());
        let Some((frame, _)) = frame else {
            break;
        };
        turns += 1;
        encoded += frame.len();
        allocated += counted.bytes;
        largest = largest.max(counted.bytes);
        assert!(frame.len() <= wire::UPDATE_TARGET_BYTES + limits::HEADER_BYTES);
        // One message's rows, their vector's growth, and the frame's growing buffer.
        assert!(
            counted.bytes < 8 * wire::UPDATE_TARGET_BYTES,
            "turn {turns} allocated {} B",
            counted.bytes
        );
        let Some(ServerMessage::Accessibility(message)) =
            assembler.server(RawFrame::parse(&frame).unwrap()).unwrap()
        else {
            panic!("one semantic message per frame")
        };
        client.apply(message.update);
    }
    assert!(encoded < 1_200_000, "{encoded} B");
    assert!(turns > 20, "{turns} messages");
    assert!(
        largest < 2 * encoded,
        "largest turn {largest} B of {encoded} B"
    );
    assert!(
        allocated >= encoded,
        "the writing thread allocated the messages it wrote: {allocated} B for {encoded} B"
    );
    client.assert_presents(&model);
}

/// Scrollback that trims at the top while output appends at the bottom sends two small splices
/// and the new and changed rows, not the history in between.
#[test]
fn scrolling_history_sends_only_trimmed_and_appended_rows() {
    let mut terminal = Terminal::new();
    let mut revisions = Revisions::new();
    let history = terminal.history(1_000, 80);
    let base = revisions.next(Arc::clone(&history)).unwrap();
    let mut client = Client::default();
    client.revision(encode(&base, None));

    let ids: Vec<_> = (5..1_005).map(row_id).collect();
    let mut changed: Vec<_> = (1_000..1_005)
        .map(|line| ascii(row_id(line), 1, &format!("appended {line}")))
        .collect();
    changed.push(ascii(row_id(999), 2, "the old last row, edited"));
    let scrolled = terminal.apply(Some(ids), changed, 976..1_000, None, None);
    let revision = revisions.next(Arc::clone(&scrolled)).unwrap();
    let updates = encode(&revision, Some(&base));

    let splices: Vec<_> = updates
        .iter()
        .flat_map(|update| &update.splices)
        .map(|splice| (splice.at, splice.remove, splice.insert.len()))
        .collect();
    assert_eq!(splices, [(0, 5, 0), (995, 0, 1)]);
    let lines: Vec<_> = updates
        .iter()
        .flat_map(|update| &update.rows)
        .map(|row| row.line)
        .collect();
    assert_eq!(lines, [994, 995, 996, 997, 998, 999]);
    client.revision(updates);
    client.assert_presents(&scrolled);
}

/// Each kind of change reaches a client as a delta that reproduces the new model exactly: rows
/// inserted and removed in the middle, a row edited in place, wide and combining cells, the
/// longest graphemes, and a switch to another screen generation.
#[test]
fn deltas_reproduce_every_kind_of_change() {
    let mut terminal = Terminal::new();
    let mut revisions = Revisions::new();
    let first = terminal.history(300, 40);
    let mut base = revisions.next(Arc::clone(&first)).unwrap();
    let mut client = Client::default();
    client.revision(encode(&base, None));

    let longest = format!("e{}", "\u{0301}".repeat(64));
    let wide = |id, revision| AccessibilityRowUpdate {
        id,
        revision,
        soft_wrapped: true,
        cells: vec![
            AccessibilityCell::at_column("界", 0, 2),
            AccessibilityCell::at_column("A\u{301}", 2, 1),
            AccessibilityCell::at_column("😀", 3, 2),
            AccessibilityCell::at_column(longest.as_str(), 5, 1),
        ],
    };
    let inserted = AccessibilityRowId {
        node_serial: 900,
        ..row_id(0)
    };
    let mut middle: Vec<_> = (0..300).map(row_id).collect();
    middle.splice(120..130, [inserted]);
    let changes: Vec<(Option<Vec<AccessibilityRowId>>, Vec<AccessibilityRowUpdate>)> = vec![
        (Some(middle.clone()), vec![wide(inserted, 1)]),
        (None, vec![ascii(row_id(5), 2, "edited in place")]),
        (None, vec![wide(row_id(6), 2)]),
        (
            Some((0..300).map(row_id).collect()),
            (120..130)
                .map(|line| ascii(row_id(line), 3, "back again"))
                .collect(),
        ),
    ];
    for (topology, rows) in changes {
        let model = terminal.apply(topology, rows, 0..24, Some(cell(row_id(3), 1)), None);
        let revision = revisions.next(Arc::clone(&model)).unwrap();
        client.revision(encode(&revision, Some(&base)));
        client.assert_presents(&model);
        base = revision;
    }

    // Long rows of the longest graphemes travel as fragments across messages, losslessly.
    let long: Vec<_> = (0..512)
        .map(|column| AccessibilityCell::at_column(longest.as_str(), column, 1))
        .collect();
    let model = terminal.apply(
        None,
        vec![AccessibilityRowUpdate {
            id: row_id(7),
            revision: 4,
            soft_wrapped: false,
            cells: long,
        }],
        0..24,
        None,
        None,
    );
    let revision = revisions.next(Arc::clone(&model)).unwrap();
    let updates = encode(&revision, Some(&base));
    assert!(updates.len() > 1, "the row crosses messages");
    client.revision(updates);
    client.assert_presents(&model);
    base = revision;

    // Another screen generation shares no row identity, so its document replaces the old.
    let mut alternate = TerminalAccessibilityState::default();
    let ids: Vec<_> = (0..30)
        .map(|line| AccessibilityRowId {
            screen: AccessibilityScreen::Alternate,
            screen_generation: 1,
            ..row_id(line)
        })
        .collect();
    let model = alternate
        .apply(
            AccessibilityUpdate {
                revision: 1,
                screen: AccessibilityScreen::Alternate,
                screen_generation: 1,
                complete: true,
                more: false,
                topology: Some(ids.clone()),
                visible_lines: 0..24,
                cursor: None,
                selection: None,
                changed_rows: ids.iter().map(|id| ascii(*id, 1, "alternate")).collect(),
            },
            PresentationGeneration::default(),
        )
        .unwrap();
    let revision = revisions.next(Arc::clone(&model)).unwrap();
    client.revision(encode(&revision, Some(&base)));
    client.assert_presents(&model);
}

/// A model equal to its base, sharing its document and tail, encodes as nothing, so the
/// channel's credit stays available.
#[test]
fn a_model_equal_to_its_base_encodes_nothing() {
    let model = Terminal::new().history(50, 10);
    let mut state = accessibility::Accessibility::default();
    state.start();
    state.update(Arc::clone(&model));
    let first = state.take_changed().unwrap();
    // Demand that starts again hands the same revision, unchanged.
    assert!(state.take_changed().is_none());
    state.end();
    state.start();
    let again = state.take_changed().unwrap();
    assert_eq!(again.revision(), first.revision());
    assert!(encode(&again, Some(&first)).is_empty());
    state.end();
    state.update(model);
    assert!(
        state.take_changed().is_none(),
        "nothing is handed without demand"
    );
}

#[test]
fn takeover_semantics_ignore_models_published_before_the_reading_reset() {
    let old = ScreenSnapshot::empty(crate::local_path::LocalPathSemantics::Posix);
    let mut reset = (*old).clone();
    reset.generation = PresentationGeneration::test(2);
    let mut state = accessibility::Accessibility::default();
    state.start();
    state.update(Arc::new(TerminalAccessibilityModel::from_screen(&old)));
    state.reset();
    assert!(!state.demanded(), "a reset ends demand");
    state.start();
    state.bind_reset_screen(reset.generation);
    state.update(Arc::new(TerminalAccessibilityModel::from_screen(&old)));
    assert!(state.take_changed().is_none());
    state.update(Arc::new(TerminalAccessibilityModel::from_screen(&reset)));
    assert!(state.take_changed().is_some());
}

/// Demands semantics for `lease` and returns the first complete document `until` accepts.
fn document(host: &mut ServerPaneHost, lease: Lease, until: impl Fn(&Client) -> bool) -> Client {
    assert!(matches!(
        host.request(
            [1; 32],
            lease.connection,
            Request::AccessibilityDemand {
                lease,
                demand: wire::Demand::Start { generation: 1 },
            },
        ),
        Reply::Unchanged
    ));
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let Observed::Live(observation) = host.observe([1; 32], &lease) else {
            panic!("the Pane is live");
        };
        if let Some(model) = observation.accessibility {
            let mut client = Client::default();
            client.revision(encode(&model, None));
            if until(&client) {
                return client;
            }
        }
        assert!(Instant::now() < deadline, "no matching document");
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn demand(host: &mut ServerPaneHost, lease: Lease) -> Reply {
    host.request(
        [1; 32],
        lease.connection,
        Request::AccessibilityDemand {
            lease,
            demand: wire::Demand::Start { generation: 1 },
        },
    )
}

#[test]
fn server_semantics_push_history_and_refuse_a_disconnected_or_replaced_lease() {
    let mut host = host();
    let lease = attach(&mut host, 1, true);
    command(
        &mut host,
        1,
        lease,
        "stty -echo; printf '\\033[2J\\033[H'; i=0; while [ $i -lt 600 ]; do printf 'accessible-history-%04d\\r\\n' $i; i=$((i+1)); done",
    );
    let first = document(&mut host, lease, |client| {
        client.line("accessible-history-0599").is_some()
    });
    assert!(first.topology.len() > 600);
    assert!(
        first.tail.as_ref().unwrap().visible_lines.start > 0,
        "history must extend outside the viewport"
    );
    let id = first.topology[first.line("accessible-history-0001").unwrap()];
    host.disconnected(lease.connection);
    assert!(matches!(
        demand(&mut host, lease),
        Reply::Refused(Refusal::Stale)
    ));
    let resumed = attach(&mut host, 2, false);
    let after = document(&mut host, resumed, |_| true);
    assert!(
        after.topology.contains(&id),
        "reconnect must preserve retained row identities"
    );
    let Reply::Attached { lease: taken } = host.request(
        [2; 32],
        [3; 32],
        Request::TakeOver {
            target: resumed.into(),
            size: size(),
            style: None,
        },
    ) else {
        panic!("takeover failed");
    };
    assert_ne!(taken.control, resumed.control);
    assert!(matches!(
        demand(&mut host, resumed),
        Reply::Refused(Refusal::Stale)
    ));
    assert!(matches!(
        host.request(
            [1; 32],
            resumed.connection,
            Request::AccessibilitySelection {
                lease: resumed,
                focus_generation: 1,
                selection: None
            }
        ),
        Reply::Refused(Refusal::Stale)
    ));
    host.shutdown().unwrap();
}

/// An idle Pane's first demand builds its complete document without publishing a screen.
#[test]
fn an_idle_panes_first_demand_delivers_a_complete_document_without_a_screen() {
    let mut host = host();
    let lease = attach(&mut host, 1, true);
    command(
        &mut host,
        1,
        lease,
        "stty -echo; printf 'idle-marker\\r\\n'",
    );
    let deadline = Instant::now() + Duration::from_secs(60);
    let mut quiet = None;
    // Idle: no screen for a while after the marker appeared.
    loop {
        let Observed::Live(observation) = host.observe([1; 32], &lease) else {
            panic!("the Pane is live");
        };
        let screen = observation.screen.map(|screen| screen.generation);
        match quiet {
            Some((generation, since)) if Some(generation) == screen => {
                if Instant::now().duration_since(since) > Duration::from_millis(500) {
                    break;
                }
            }
            _ => quiet = screen.map(|generation| (generation, Instant::now())),
        }
        assert!(Instant::now() < deadline, "the Pane never went idle");
        std::thread::sleep(Duration::from_millis(20));
    }
    let (idle, _) = quiet.unwrap();
    let client = document(&mut host, lease, |client| {
        client.line("idle-marker").is_some()
    });
    assert!(client.applied.is_some());
    let Observed::Live(observation) = host.observe([1; 32], &lease) else {
        panic!("the Pane is live");
    };
    assert_eq!(
        observation.screen.map(|screen| screen.generation),
        Some(idle),
        "demand published no screen"
    );
    host.shutdown().unwrap();
}

#[test]
fn server_accessibility_selection_requires_current_focus_and_live_cell_revisions() {
    let mut host = host();
    let lease = attach(&mut host, 1, true);
    // Keep the shell prompt on the next row so it cannot invalidate the target's CellRefs.
    command(
        &mut host,
        1,
        lease,
        "stty -echo; printf '\\033[2J\\033[Hnative-selection-target\\r\\n'",
    );
    let client = document(&mut host, lease, |client| {
        client
            .texts()
            .any(|text| text.starts_with("native-selection-target"))
    });
    let line = client
        .texts()
        .position(|text| text.starts_with("native-selection-target"))
        .unwrap();
    let id = client.topology[line];
    let row = client.rows[&key(&id)].clone();
    let reference = |column| wire::CellRef {
        row: id,
        row_revision: row.revision,
        column,
    };
    let selection = wire::SelectionRefs {
        start: reference(0),
        end: reference(5),
        rectangle: false,
    };
    let selected = Some(wire::Selection {
        start: wire::Point {
            line: line as u32,
            column: 0,
        },
        end: wire::Point {
            line: line as u32,
            column: 5,
        },
        rectangle: false,
    });
    let select = |host: &mut ServerPaneHost, selection| {
        host.request(
            [1; 32],
            lease.connection,
            Request::AccessibilitySelection {
                lease,
                focus_generation: 2,
                selection,
            },
        )
    };
    assert!(matches!(
        select(&mut host, Some(selection)),
        Reply::Refused(Refusal::Stale)
    ));
    assert!(matches!(
        host.request(
            [1; 32],
            lease.connection,
            Request::Focus {
                lease,
                generation: 2,
                target: FocusTarget::Terminal
            }
        ),
        Reply::Unchanged
    ));
    assert!(matches!(
        select(&mut host, Some(selection)),
        Reply::Unchanged
    ));
    document(&mut host, lease, |client| {
        client.tail.as_ref().unwrap().selection == selected
    });
    let rectangle = wire::SelectionRefs {
        rectangle: true,
        ..selection
    };
    assert!(matches!(
        select(&mut host, Some(rectangle)),
        Reply::Refused(Refusal::Unsupported)
    ));
    select(&mut host, None);
    document(&mut host, lease, |client| {
        client.tail.as_ref().unwrap().selection.is_none()
    });
    let stale = wire::SelectionRefs {
        start: wire::CellRef {
            row_revision: row.revision.wrapping_add(1),
            ..reference(0)
        },
        ..selection
    };
    select(&mut host, Some(stale));
    // The worker answers commands in order, so a page read's reply follows the selection.
    let (barrier, passed) = std::sync::mpsc::channel();
    host.pane.as_ref().unwrap().runtime.read_scrollback(
        0,
        0,
        1,
        crate::terminal::session::ScrollbackReply::new(move |_| {
            let _ = barrier.send(());
        }),
    );
    passed.recv_timeout(Duration::from_secs(10)).unwrap();
    let client = document(&mut host, lease, |_| true);
    assert!(client.tail.unwrap().selection.is_none());
    host.shutdown().unwrap();

    // Shape-only output changes must advance the wire revision, even with the same UTF-16 range.
    let refs = accessibility::decode_selection(selection).unwrap();
    let mut semantics = TerminalAccessibilityState::default();
    let mut update = AccessibilityUpdate {
        revision: 1,
        screen: refs.start.row.screen,
        screen_generation: refs.start.row.screen_generation,
        complete: true,
        more: false,
        topology: Some(vec![refs.start.row]),
        visible_lines: 0..1,
        cursor: None,
        selection: Some(refs),
        changed_rows: vec![AccessibilityRowUpdate {
            id: refs.start.row,
            revision: row.revision,
            soft_wrapped: row.soft_wrapped,
            cells: row
                .cells
                .iter()
                .map(|(columns, bytes)| {
                    AccessibilityCell::at_column(
                        &row.text[bytes.clone()],
                        columns.start,
                        columns.end - columns.start,
                    )
                })
                .collect(),
        }],
    };
    let generation = PresentationGeneration::default();
    let mut publication = accessibility::Accessibility::default();
    let linear = semantics.apply(update.clone(), generation).unwrap();
    let range = linear.selection_range();
    publication.update(linear);
    let revision = publication.revision();
    update.changed_rows.clear();
    update.topology = None;
    update.selection.as_mut().unwrap().rectangle = true;
    let rectangle = semantics.apply(update, generation.next()).unwrap();
    assert_eq!(rectangle.selection_range(), range);
    publication.update(rectangle);
    assert!(publication.revision() > revision);
}

/// Output that changes one row of retained history sends that row only, and a captured model
/// keeps encoding the same revision while the terminal moves on.
#[test]
fn unchanged_history_is_not_resent() {
    use crate::terminal::emulator::TerminalEmulator;
    let mut emulator = TerminalEmulator::new(geometry(size())).unwrap();
    emulator.feed(b"retained row\r\nsecond row");
    fn complete(emulator: &mut TerminalEmulator) -> Arc<TerminalAccessibilityModel> {
        let mut first = true;
        loop {
            let (model, more) = emulator.accessibility_snapshot(first).unwrap();
            first = false;
            if !more {
                return model.unwrap();
            }
        }
    }
    let mut revisions = Revisions::new();
    let original = complete(&mut emulator);
    let base = revisions.next(Arc::clone(&original)).unwrap();
    let mut client = Client::default();
    client.revision(encode(&base, None));
    emulator.feed(b" appended");
    let updated = complete(&mut emulator);
    let revision = revisions.next(Arc::clone(&updated)).unwrap();
    let delta = encode(&revision, Some(&base));
    assert!(delta.iter().all(|update| update.splices.is_empty()));
    let rows: Vec<_> = delta.iter().flat_map(|update| &update.rows).collect();
    assert_eq!(rows.len(), 1, "unchanged history must not be retransmitted");
    assert!(rows[0].text.starts_with("second row appended"));
    client.revision(delta);
    client.assert_presents(&updated);
}
