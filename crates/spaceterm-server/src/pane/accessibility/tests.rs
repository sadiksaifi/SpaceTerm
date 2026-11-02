use super::*;
use crate::protocol::frame::{Assembler, encode_server};
use crate::protocol::{
    AccessibilityMessage, Channel, DecodeError, RawFrame, ServerMessage, limits,
};

const ROWS: u32 = 10_000;
/// Page rows per page node. Ghostty's pages hold more rows at 80 columns, so fewer, longer runs
/// make the real topology smaller than this one.
const PAGE_ROWS: u32 = 64;

fn simple(text: &str) -> impl Iterator<Item = CellSpan> + '_ {
    text.chars()
        .zip(0_u16..)
        .map(|(character, column)| CellSpan {
            column,
            width: 1,
            bytes: character.len_utf8() as u16,
        })
}

fn header(revision: u64, base: Option<u64>) -> Header {
    Header {
        demand_generation: 1 << 20,
        revision,
        base,
        screen: Screen::Primary,
        screen_generation: 1 << 20,
    }
}

fn tail() -> Tail {
    Tail {
        visible_lines: ROWS - 24..ROWS,
        cursor: Some(Point {
            line: ROWS - 1,
            column: 79,
        }),
        selection: Some(Selection {
            start: Point {
                line: ROWS - 24,
                column: 0,
            },
            end: Point {
                line: ROWS - 1,
                column: 79,
            },
            rectangle: false,
        }),
    }
}

/// Encodes `update` as the server writes it, on the widest channel, and returns its bytes on the
/// wire.
fn wire(update: Update) -> (usize, Update) {
    let message = ServerMessage::Accessibility(Box::new(AccessibilityMessage {
        channel: Channel(u32::MAX),
        update,
    }));
    let encoded = encode_server(&message).unwrap();
    assert_eq!(encoded.frames.len(), 1, "an update never continues");
    let mut assembler = Assembler::default();
    let decoded = assembler
        .server(RawFrame::parse(&encoded.frames[0]).unwrap())
        .unwrap();
    let Some(ServerMessage::Accessibility(decoded)) = decoded else {
        panic!("an accessibility message decodes as one");
    };
    (encoded.bytes(), decoded.update)
}

/// Every message of a complete 10,000 by 80 ASCII document, packed as a server packs it: one
/// splice inserts every run.
fn document() -> Vec<Update> {
    // Large magnitudes, so varints take the room real counters take.
    let revision = 1 << 40;
    let mut packer = Packer::new(header(revision, None));
    let runs = (0..ROWS.div_ceil(PAGE_ROWS)).map(|page| Run {
        node_serial: (1 << 32) + u64::from(page),
        page_row: 0,
        count: PAGE_ROWS.min(ROWS - page * PAGE_ROWS) as u16,
    });
    let mut messages = packer.splice(0, 0, runs);
    for line in 0..ROWS {
        let text = (0..80)
            .map(|column| char::from(b' ' + ((line + column) % 95) as u8))
            .collect::<String>();
        let row = RowUpdate::new(
            line,
            revision - 1,
            line % 3 == 0,
            text.clone(),
            simple(&text),
        );
        messages.extend(packer.row(row));
    }
    messages.push(packer.finish(tail()));
    messages
}

/// Applies `splices` in order to `topology`, as a client applies them, with each row named by its
/// page node and page row.
fn apply(topology: &mut Vec<(u64, u16)>, splices: &[Splice]) {
    for splice in splices {
        let at = splice.at as usize;
        let rows = splice.insert.iter().flat_map(|run| {
            (run.page_row..run.page_row + run.count).map(|page_row| (run.node_serial, page_row))
        });
        topology.splice(at..at + splice.remove as usize, rows);
    }
}

#[test]
fn a_cursor_only_revision_at_ten_thousand_rows_encodes_under_256_bytes_with_no_rows() {
    let update = Update {
        header: Header {
            screen: Screen::Alternate,
            ..header(1 << 40, Some((1 << 40) - 1))
        },
        splices: Vec::new(),
        rows: Vec::new(),
        tail: Some(tail()),
    };
    let (bytes, decoded) = wire(update.clone());
    assert!(bytes < 256, "a cursor-only revision took {bytes} bytes");
    assert!(decoded.rows.is_empty() && decoded.splices.is_empty());
    assert_eq!(decoded, update);
}

#[test]
fn a_complete_ten_thousand_by_eighty_document_encodes_under_1_2_mb_in_bounded_messages() {
    let messages = document();
    let mut total = 0;
    let mut topology = Vec::new();
    let mut rows = Vec::new();
    let count = messages.len();
    for (index, message) in messages.into_iter().enumerate() {
        let (bytes, decoded) = wire(message);
        assert!(
            bytes <= limits::HEADER_BYTES + UPDATE_TARGET_BYTES,
            "message {index} took {bytes} bytes"
        );
        assert_eq!(decoded.header, header(1 << 40, None));
        assert!(
            decoded.splices.is_empty() || rows.is_empty(),
            "every splice precedes every row"
        );
        apply(&mut topology, &decoded.splices);
        assert_eq!(
            index + 1 == count,
            decoded.tail == Some(tail()),
            "only the last message has the tail"
        );
        rows.extend(decoded.rows);
        total += bytes;
    }
    assert!(total < 1_200_000, "the document took {total} bytes");
    let expected = (0..ROWS)
        .map(|line| {
            (
                (1 << 32) + u64::from(line / PAGE_ROWS),
                (line % PAGE_ROWS) as u16,
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(topology, expected);
    assert_eq!(rows.len(), ROWS as usize);
    assert!(
        rows.iter()
            .zip(0..)
            .all(|(row, line)| row.line == line && row.cells.is_none() && !row.continues)
    );
}

#[test]
fn one_large_insertion_travels_as_bounded_splices_with_one_removal() {
    let base = (0..20).map(|page_row| (7, page_row)).collect::<Vec<_>>();
    let runs = (0..10_000_u64).map(|index| Run {
        node_serial: (1 << 32) + index,
        page_row: 3,
        count: 1,
    });
    let mut expected = base.clone();
    expected.splice(5..8, runs.clone().map(|run| (run.node_serial, 3)));

    let mut packer = Packer::new(header(2, Some(1)));
    let mut messages = packer.splice(5, 3, runs);
    messages.push(packer.finish(tail()));
    assert!(messages.len() > 1, "the insertion fits no single message");
    let mut topology = base;
    let mut removals = 0;
    for message in messages {
        let (_, decoded) = wire(message);
        removals += decoded
            .splices
            .iter()
            .filter(|splice| splice.remove > 0)
            .count();
        apply(&mut topology, &decoded.splices);
    }
    assert_eq!(removals, 1);
    assert_eq!(topology, expected);
}

#[test]
fn the_last_message_has_room_for_the_widest_tail_and_channel_however_full_it_is() {
    let widest = Point {
        line: u32::MAX,
        column: u16::MAX,
    };
    let tail = Tail {
        visible_lines: u32::MAX..u32::MAX,
        cursor: Some(widest),
        selection: Some(Selection {
            start: widest,
            end: widest,
            rectangle: true,
        }),
    };
    let row = |line, length| {
        let text = "a".repeat(length);
        RowUpdate::new(line, u64::MAX, false, text.clone(), simple(&text))
    };
    // Rows of 512 characters, one fewer than fill a message, so the first row's length sweeps
    // what is left of it from hundreds of bytes down to none.
    let widest_row = usize::from(crate::pane::MAX_COLUMNS);
    let rows = UPDATE_TARGET_BYTES / encoded_len(&row(1, widest_row)) - 1;
    for first in 1..=widest_row {
        let mut packer = Packer::new(Header {
            demand_generation: u64::MAX,
            revision: u64::MAX,
            base: Some(u64::MAX - 1),
            screen: Screen::Alternate,
            screen_generation: u64::MAX,
        });
        let _ = packer.row(row(0, first));
        for line in 1..=rows as u32 {
            let _ = packer.row(row(line, widest_row));
        }
        let (_, last) = wire(packer.finish(tail.clone()));
        assert_eq!(last.tail.as_ref(), Some(&tail));
    }
}

/// Packs `row` between two small rows, joins its fragments from the decoded messages as a client
/// joins them, and returns the joined row.
fn through_the_wire(row: RowUpdate) -> RowUpdate {
    let small = |line| RowUpdate::new(line, 1, false, "a".into(), simple("a"));
    let mut packer = Packer::new(header(2, Some(1)));
    let mut messages = packer.row(small(0));
    messages.extend(packer.row(row));
    messages.extend(packer.row(small(2)));
    messages.push(packer.finish(tail()));
    let count = messages.len();
    let mut rows = Vec::<RowUpdate>::new();
    for (index, message) in messages.into_iter().enumerate() {
        let (_, decoded) = wire(message);
        assert_eq!(decoded.tail.is_some(), index + 1 == count);
        for fragment in decoded.rows {
            match rows.last_mut() {
                Some(joined) if joined.continues => {
                    assert_eq!(
                        (fragment.line, fragment.revision, fragment.soft_wrapped),
                        (joined.line, joined.revision, joined.soft_wrapped),
                        "a fragment continues its row"
                    );
                    joined.text.push_str(&fragment.text);
                    let spans = joined.cells.as_mut().expect("a fragment has spans");
                    spans.extend(fragment.cells.expect("a fragment has spans"));
                    joined.continues = fragment.continues;
                }
                _ => rows.push(fragment),
            }
        }
    }
    assert_eq!(
        rows.iter().map(|row| row.line).collect::<Vec<_>>(),
        [0, 1, 2]
    );
    rows.swap_remove(1)
}

/// Each cell's UTF-16 range in its row's text, as a client derives it.
fn utf16(row: &RowUpdate) -> Vec<Range<usize>> {
    let mut offset = 0;
    row.cells()
        .map(|(_, bytes)| {
            let start = offset;
            offset += row.text[bytes].encode_utf16().count();
            start..offset
        })
        .collect()
}

#[test]
fn rows_of_the_longest_graphemes_cross_messages_without_losing_text() {
    // The longest grapheme the engine keeps in one cell: a base and 64 suffix codepoints.
    let combining = ("e".to_owned() + &"\u{301}".repeat(64), 1, 65);
    let supplementary = ("\u{1F600}".to_owned() + &"\u{E0100}".repeat(64), 1, 130);
    let wide = (supplementary.0.clone(), 2, 130);
    assert_eq!(supplementary.0.len(), MAX_CELL_TEXT_BYTES);
    for (cluster, width, units) in [combining, supplementary, wide] {
        let cells = crate::pane::MAX_COLUMNS / width;
        let text = cluster.repeat(usize::from(cells));
        let spans = (0..cells).map(|cell| CellSpan {
            column: cell * width,
            width,
            bytes: cluster.len() as u16,
        });
        let row = RowUpdate::new(1, 1, true, text, spans);
        assert!(row.text.len() > UPDATE_TARGET_BYTES && row.text.len() <= MAX_ROW_TEXT_BYTES);

        let joined = through_the_wire(row.clone());
        assert_eq!(
            joined.text, row.text,
            "every byte of the row's text arrives"
        );
        assert_eq!(
            joined.cells().collect::<Vec<_>>(),
            row.cells().collect::<Vec<_>>()
        );
        assert!(!joined.continues);
        let expected = (0..usize::from(cells))
            .map(|cell| cell * units..(cell + 1) * units)
            .collect::<Vec<_>>();
        assert_eq!(utf16(&joined), expected);
    }
}

#[test]
fn rows_keep_cell_spans_only_when_they_are_not_one_column_per_character() {
    let ascii = RowUpdate::new(0, 1, false, "ab c".into(), simple("ab c"));
    assert_eq!(ascii.cells, None);
    let accented = RowUpdate::new(0, 1, false, "é".into(), simple("é"));
    assert_eq!(
        accented.cells, None,
        "one character of two bytes is still one column"
    );

    // A wide character after a simple prefix, a combining sequence, and a gap.
    let spans = [
        CellSpan {
            column: 0,
            width: 1,
            bytes: 1,
        },
        CellSpan {
            column: 1,
            width: 2,
            bytes: 3,
        },
        CellSpan {
            column: 3,
            width: 1,
            bytes: 3,
        },
        CellSpan {
            column: 6,
            width: 1,
            bytes: 1,
        },
    ];
    let mixed = RowUpdate::new(0, 1, false, "a界e\u{301}z".into(), spans);
    assert_eq!(mixed.cells.as_deref(), Some(&spans[..]));
    assert_eq!(
        mixed.cells().collect::<Vec<_>>(),
        [(0..1, 0..1), (1..3, 1..4), (3..4, 4..7), (6..7, 7..8)]
    );
    assert_eq!(
        accented.cells().collect::<Vec<_>>(),
        [(0..1, 0..2)],
        "a simple row derives the same cells its spans would name"
    );

    // Cells that leave text uncovered keep spans, so validation refuses the row.
    let uncovered = RowUpdate::new(0, 1, false, "ab".into(), simple("a"));
    assert!(uncovered.cells.is_some() && !uncovered.valid());
}

fn row(text: &str, cells: Option<Vec<CellSpan>>) -> RowUpdate {
    RowUpdate {
        line: 0,
        revision: 1,
        soft_wrapped: false,
        text: text.into(),
        cells,
        continues: false,
    }
}

fn span(column: u16, width: u16, bytes: u16) -> CellSpan {
    CellSpan {
        column,
        width,
        bytes,
    }
}

type Break = fn(&mut Update);

#[test]
fn validation_refuses_updates_one_message_shows_are_inconsistent() {
    let valid = Update {
        header: header(2, Some(1)),
        splices: vec![Splice {
            at: 0,
            remove: 1,
            insert: Vec::new(),
        }],
        rows: vec![row("a界", Some(vec![span(0, 1, 1), span(1, 2, 3)]))],
        tail: Some(Tail {
            visible_lines: 0..1,
            cursor: None,
            selection: None,
        }),
    };
    assert_eq!(valid.validate(), Ok(()));
    let cases: [(&str, Break); 17] = [
        ("a message with neither items nor a tail", |u| {
            u.splices.clear();
            u.rows.clear();
            u.tail = None;
        }),
        ("a base at or after its revision", |u| {
            u.header.base = Some(2)
        }),
        ("an empty splice", |u| u.splices[0].remove = 0),
        ("an empty run", |u| {
            u.splices[0].insert.push(Run {
                node_serial: 0,
                page_row: 0,
                count: 0,
            })
        }),
        ("a run past the last page row", |u| {
            u.splices[0].insert.push(Run {
                node_serial: 0,
                page_row: u16::MAX,
                count: 2,
            })
        }),
        ("rows out of line order", |u| {
            let mut repeat = u.rows[0].clone();
            repeat.line = 0;
            u.rows.push(repeat);
        }),
        ("a span inside a character", |u| {
            u.rows[0].cells = Some(vec![span(0, 1, 2), span(1, 2, 2)])
        }),
        ("spans short of the text", |u| {
            u.rows[0].cells = Some(vec![span(0, 1, 1)])
        }),
        ("overlapping spans", |u| {
            u.rows[0].cells = Some(vec![span(0, 2, 1), span(1, 2, 3)])
        }),
        ("a zero-width span", |u| {
            u.rows[0].cells = Some(vec![span(0, 0, 1), span(1, 2, 3)])
        }),
        ("a span past the last column", |u| {
            u.rows[0].cells = Some(vec![
                span(0, 1, 1),
                span(crate::pane::MAX_COLUMNS - 1, 2, 3),
            ])
        }),
        ("a simple row wider than the grid", |u| {
            u.rows[0] = row(&"a".repeat(usize::from(crate::pane::MAX_COLUMNS) + 1), None)
        }),
        ("a span over the cell text bound", |u| {
            let cell = "\u{1F600}".repeat(MAX_CELL_TEXT_BYTES / 4 + 1);
            u.rows[0] = row(&cell, Some(vec![span(0, 1, cell.len() as u16)]))
        }),
        ("a simple fragment", |u| {
            u.rows[0] = RowUpdate {
                continues: true,
                ..row("a", None)
            }
        }),
        ("a fragment continued on another line", |u| {
            u.rows[0].continues = true;
            u.rows.push(RowUpdate {
                line: 1,
                ..row("b", Some(vec![span(3, 1, 1)]))
            });
            u.tail = None;
        }),
        ("a tail after a fragment that continues", |u| {
            u.rows[0].continues = true
        }),
        ("reversed visible lines", |u| {
            u.tail.as_mut().unwrap().visible_lines = Range { start: 1, end: 0 }
        }),
    ];
    for (name, break_it) in cases {
        let mut update = valid.clone();
        break_it(&mut update);
        assert!(update.validate().is_err(), "accepted {name}");
        let encoded = encode_server(&ServerMessage::Accessibility(Box::new(
            AccessibilityMessage {
                channel: Channel(1),
                update,
            },
        )))
        .unwrap();
        let mut assembler = Assembler::default();
        let frame = RawFrame::parse(&encoded.frames[0]).unwrap();
        assert_eq!(
            assembler.server(frame).err(),
            Some(DecodeError::OutOfBounds),
            "decoded {name}"
        );
    }
}

#[test]
#[should_panic(expected = "a revision's splices precede its rows")]
fn a_splice_after_a_row_is_a_server_bug() {
    let mut packer = Packer::new(header(1, None));
    let _ = packer.row(RowUpdate::new(0, 1, false, "a".into(), simple("a")));
    let _ = packer.splice(
        0,
        0,
        [Run {
            node_serial: 0,
            page_row: 0,
            count: 1,
        }],
    );
}
