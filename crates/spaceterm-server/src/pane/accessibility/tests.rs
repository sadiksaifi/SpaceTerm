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

/// Encodes `update` as the server writes it, and returns its bytes on the wire.
fn wire(update: Update) -> (usize, Update) {
    let message = ServerMessage::Accessibility(Box::new(AccessibilityMessage {
        channel: Channel(127),
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

/// Every message of a complete 10,000 by 80 ASCII document, packed as a server packs it.
fn document() -> Vec<Update> {
    // Large magnitudes, so varints take the room real counters take.
    let revision = 1 << 40;
    let mut packer = Packer::new(revision, None, Screen::Primary, 1 << 20);
    let mut messages = Vec::new();
    for page in 0..ROWS.div_ceil(PAGE_ROWS) {
        let count = PAGE_ROWS.min(ROWS - page * PAGE_ROWS) as u16;
        messages.extend(packer.splice(Splice {
            at: page * PAGE_ROWS,
            remove: 0,
            insert: vec![Run {
                node_serial: (1 << 32) + u64::from(page),
                page_row: 0,
                count,
            }],
        }));
    }
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

#[test]
fn a_cursor_only_revision_at_ten_thousand_rows_encodes_under_256_bytes_with_no_rows() {
    let update = Update {
        revision: 1 << 40,
        base: Some((1 << 40) - 1),
        screen: Screen::Alternate,
        screen_generation: 1 << 20,
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
    let mut lines = 0_u32;
    let mut rows = Vec::new();
    for (index, message) in messages.into_iter().enumerate() {
        let last = message.tail.is_some();
        let (bytes, decoded) = wire(message);
        assert!(
            bytes <= limits::HEADER_BYTES + UPDATE_TARGET_BYTES,
            "message {index} took {bytes} bytes"
        );
        assert_eq!((decoded.revision, decoded.base), (1 << 40, None));
        for splice in &decoded.splices {
            assert!(rows.is_empty(), "every splice precedes every row");
            assert_eq!((splice.at, splice.remove), (lines, 0));
            lines += splice
                .insert
                .iter()
                .map(|run| u32::from(run.count))
                .sum::<u32>();
        }
        assert_eq!(
            last,
            decoded.tail == Some(tail()),
            "only the last message has the tail"
        );
        rows.extend(decoded.rows);
        total += bytes;
    }
    assert!(total < 1_200_000, "the document took {total} bytes");
    assert_eq!(lines, ROWS);
    assert_eq!(rows.len(), ROWS as usize);
    assert!(
        rows.iter()
            .zip(0..)
            .all(|(row, line)| row.line == line && row.cells.is_none())
    );
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
        revision: 2,
        base: Some(1),
        screen: Screen::Primary,
        screen_generation: 0,
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
    let cases: [(&str, Break); 13] = [
        ("a base at or after its revision", |u| u.base = Some(2)),
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
        ("a row over the text bound", |u| {
            u.rows[0] = row(
                &"a".repeat(MAX_ROW_TEXT_BYTES + 1),
                Some(vec![span(0, 1, u16::MAX)]),
            )
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
fn a_row_larger_than_the_target_travels_alone_within_the_message_bound() {
    let cell = "e\u{301}".repeat(21) + "e";
    let text = cell.repeat(usize::from(crate::pane::MAX_COLUMNS));
    assert_eq!(text.len(), MAX_ROW_TEXT_BYTES);
    let spans = (0..crate::pane::MAX_COLUMNS).map(|column| CellSpan {
        column,
        width: 1,
        bytes: cell.len() as u16,
    });
    let largest = RowUpdate::new(1, 1, false, text, spans);
    let small = RowUpdate::new(0, 1, false, "a".into(), simple("a"));

    let mut packer = Packer::new(2, Some(1), Screen::Primary, 0);
    assert_eq!(packer.row(small.clone()), None);
    let first = packer
        .row(largest.clone())
        .expect("the large row starts a message");
    assert_eq!(first.rows, [small]);
    assert_eq!(
        (first.revision, first.base, first.tail.clone()),
        (2, Some(1), None)
    );
    let second = packer.finish(Tail {
        visible_lines: 0..2,
        cursor: None,
        selection: None,
    });
    assert_eq!(second.rows, [largest]);
    let (bytes, _) = wire(second);
    assert!(
        bytes > limits::HEADER_BYTES + UPDATE_TARGET_BYTES
            && bytes <= limits::HEADER_BYTES + limits::MAX_ACCESSIBILITY_BYTES,
        "the largest row took {bytes} bytes"
    );
}

#[test]
#[should_panic(expected = "a revision's splices precede its rows")]
fn a_splice_after_a_row_is_a_server_bug() {
    let mut packer = Packer::new(1, None, Screen::Primary, 0);
    let _ = packer.row(RowUpdate::new(0, 1, false, "a".into(), simple("a")));
    let _ = packer.splice(Splice {
        at: 0,
        remove: 0,
        insert: vec![Run {
            node_serial: 0,
            page_row: 0,
            count: 1,
        }],
    });
}
