#![cfg(all(feature = "engine", feature = "test-support"))]
use sha2::{Digest, Sha256};
use spaceterm_core::{authority::*, content::*, content_queries::*, identity::*};
use spaceterm_terminal::{
    TerminalEmulator,
    content::{copy::CopyStep, find::FindStep, pages::ContentScanBudget},
    geometry::*,
};
fn emulator(columns: u16, rows: u16) -> TerminalEmulator {
    let mut emulator = TerminalEmulator::new(TerminalGeometry::from_grid(
        CellGridSize::new(columns, rows),
        LogicalCellSize::new(10.0, 20.0),
        BackingScale::ONE,
    ))
    .unwrap();
    emulator
        .set_content_session(SessionKey {
            pane: PaneId::new(ServerId::new([8; 16]), 1).unwrap(),
            epoch: ServerEpoch::new(1),
            generation: SessionGeneration::new(1),
        })
        .unwrap();
    emulator
}
fn stamp(emulator: &TerminalEmulator) -> QueryStamp {
    let base = emulator.content_base().unwrap();
    let client = ClientId::new(base.session.pane.server(), 1).unwrap();
    QueryStamp::new(
        EffectFence::new(
            base.session,
            ControlGrant::new(base.session.pane, client, ControlGeneration::new(1)).unwrap(),
            ConnectionBinding::new(client, ConnectionGeneration::new(1)),
            ContextGeneration::new(1),
            Some(base.revision),
        )
        .unwrap(),
        base,
        ContentRequestId::new(1).unwrap(),
        RequestGeneration::new(1),
    )
    .unwrap()
}
fn selection(emulator: &mut TerminalEmulator) -> (QueryStamp, ContentSelection) {
    let stamp = stamp(emulator);
    let request = TopologyPageRequest::new(
        stamp,
        None,
        PageLimits::new(256, 16384, 1024 * 1024).unwrap(),
    )
    .unwrap();
    let mut rows = Vec::new();
    let mut request = request;
    loop {
        let ContentReadResult::Ready(page) = emulator.topology_page(&request).unwrap() else {
            panic!("expected topology")
        };
        rows.extend_from_slice(page.rows());
        let Some(next) = page.continuation() else {
            break;
        };
        request = TopologyPageRequest::new(stamp, Some(next), request.limits()).unwrap();
    }
    let first = rows.first().unwrap();
    let last = rows.last().unwrap();
    let cell = |row: &TopologyRow, column| CellRef {
        session: stamp.base().session,
        row: row.row(),
        row_revision: row.revision(),
        column,
    };
    (
        stamp,
        ContentSelection::new(stamp, cell(first, 0), cell(last, last.columns() - 1), false)
            .unwrap(),
    )
}
#[test]
fn full_scrollback_copy_is_complete_and_not_limited_to_one_mebibyte() {
    let mut emulator = emulator(120, 3);
    for _ in 0..9800 {
        emulator.feed(format!("{}\r\n", "x".repeat(120)).as_bytes());
    }
    let (stamp, selection) = selection(&mut emulator);
    let request = CopyRequest::new(
        stamp,
        selection,
        CopyOptions {
            unwrap_soft_wraps: true,
            trim_trailing_spaces: true,
            include_html: false,
        },
    )
    .unwrap();
    let mut job = emulator
        .begin_copy(&request, ResourceTransferId::new(1).unwrap())
        .unwrap();
    let mut text = Vec::new();
    let mut complete = None;
    for _ in 0..100_000 {
        match emulator
            .step_copy(&mut job, ContentScanBudget::default())
            .unwrap()
        {
            CopyStep::Pending => {}
            CopyStep::Complete => break,
            CopyStep::Item(CopyStreamItem::Chunk(chunk)) => {
                assert_eq!(chunk.offset(), text.len() as u64);
                assert!(chunk.bytes().len() <= 256 * 1024);
                text.extend_from_slice(chunk.bytes());
            }
            CopyStep::Item(CopyStreamItem::Complete(value)) => {
                complete = Some(value);
            }
            CopyStep::Item(CopyStreamItem::Failed { .. }) => panic!("copy failed"),
        }
    }
    assert!(text.len() > 1024 * 1024);
    let complete = complete.unwrap();
    assert_eq!(complete.plain_bytes(), text.len() as u64);
    assert_eq!(
        complete.plain_digest(),
        <[u8; 32]>::from(Sha256::digest(&text))
    );
    assert_eq!(
        std::str::from_utf8(&text).unwrap().matches('x').count(),
        9800 * 120
    );
}
#[test]
fn copy_revision_change_never_completes_partial_text() {
    let mut emulator = emulator(8, 2);
    emulator.feed(b"original");
    let (stamp, selection) = selection(&mut emulator);
    let request = CopyRequest::new(
        stamp,
        selection,
        CopyOptions {
            unwrap_soft_wraps: true,
            trim_trailing_spaces: true,
            include_html: true,
        },
    )
    .unwrap();
    let mut job = emulator
        .begin_copy(&request, ResourceTransferId::new(1).unwrap())
        .unwrap();
    emulator.feed(b"changed");
    assert!(matches!(
        emulator
            .step_copy(&mut job, ContentScanBudget::default())
            .unwrap(),
        CopyStep::Item(CopyStreamItem::Failed {
            reason: ContentReadFailure::StaleAuthority,
            ..
        })
    ));
    assert!(matches!(
        emulator
            .step_copy(&mut job, ContentScanBudget::default())
            .unwrap(),
        CopyStep::Complete
    ));
}
#[test]
fn find_matches_ascii_unicode_soft_wrap_and_wide_cells_without_moving_view() {
    let mut emulator = emulator(5, 3);
    emulator.feed("abcDEF\r\né😀é😀".as_bytes());
    let before = emulator.snapshot().unwrap().unwrap();
    let request = FindRequest::new(
        stamp(&emulator),
        BoundedFindQuery::new("CdEf".into()).unwrap(),
        None,
    )
    .unwrap();
    let mut job = emulator.begin_find(&request).unwrap();
    let mut matches = Vec::new();
    for _ in 0..1000 {
        match emulator
            .step_find(&mut job, ContentScanBudget::default())
            .unwrap()
        {
            FindStep::Pending => {}
            FindStep::Complete => break,
            FindStep::Item(FindStreamItem::Chunk(chunk)) => {
                matches.extend_from_slice(chunk.matches())
            }
            FindStep::Item(FindStreamItem::Complete { .. }) => {}
            FindStep::Item(FindStreamItem::Failed { .. }) => panic!("Find failed"),
        }
    }
    assert_eq!(matches.len(), 1);
    assert_eq!(matches[0].start().column, 2);
    assert_eq!(matches[0].end().column, 0);
    if let Some(after) = emulator.snapshot().unwrap() {
        assert_eq!(before.viewport, after.viewport);
        assert_eq!(before.find, after.find);
    }
}

fn copy_result(
    emulator: &mut TerminalEmulator,
    request: &CopyRequest,
) -> (String, String, CopyCompletion) {
    let mut job = emulator
        .begin_copy(request, ResourceTransferId::new(2).unwrap())
        .unwrap();
    let mut parts = [Vec::new(), Vec::new()];
    let mut completion = None;
    for _ in 0..100_000 {
        match emulator
            .step_copy(&mut job, ContentScanBudget::default())
            .unwrap()
        {
            CopyStep::Pending => {}
            CopyStep::Complete => break,
            CopyStep::Item(CopyStreamItem::Chunk(chunk)) => {
                let index = usize::from(chunk.part() == CopyPart::Html);
                assert_eq!(chunk.offset(), parts[index].len() as u64);
                assert!(std::str::from_utf8(chunk.bytes()).is_ok());
                assert!(chunk.bytes().len() <= MAX_RESOURCE_CHUNK_BYTES);
                parts[index].extend_from_slice(chunk.bytes());
            }
            CopyStep::Item(CopyStreamItem::Complete(value)) => completion = Some(value),
            CopyStep::Item(CopyStreamItem::Failed { .. }) => panic!("Copy failed"),
        }
    }
    let completion = completion.unwrap();
    assert_eq!(completion.plain_bytes(), parts[0].len() as u64);
    assert_eq!(
        completion.plain_digest(),
        <[u8; 32]>::from(Sha256::digest(&parts[0]))
    );
    if request.options().include_html {
        assert_eq!(
            completion.html(),
            Some((
                parts[1].len() as u64,
                <[u8; 32]>::from(Sha256::digest(&parts[1]))
            ))
        );
    }
    let [plain, html] = parts;
    (
        String::from_utf8(plain).unwrap(),
        String::from_utf8(html).unwrap(),
        completion,
    )
}

#[test]
fn rich_copy_preserves_complete_style_and_escapes_text_and_links() {
    let mut emulator = emulator(8, 2);
    emulator.feed(b"\x1b]8;;https://example.invalid/?a=\"x\"&b='y'\x1b\\\x1b[1;2;3;4:3;5;7;8;9;53m\x1b[38;2;1;2;3m\x1b[48;2;4;5;6m\x1b[58;2;7;8;9m<&>\"'\x1b[0m\x1b]8;;\x1b\\");
    let (stamp, selection) = selection(&mut emulator);
    let request = CopyRequest::new(
        stamp,
        selection,
        CopyOptions {
            unwrap_soft_wraps: true,
            trim_trailing_spaces: true,
            include_html: true,
        },
    )
    .unwrap();
    let (plain, html, _) = copy_result(&mut emulator, &request);
    assert_eq!(plain.trim_end(), "<&>\"'");
    for expected in [
        "&lt;",
        "&amp;",
        "&gt;",
        "&quot;",
        "&#39;",
        "font-weight: bold;",
        "font-style: italic;",
        "opacity: 0.5;",
        "visibility: hidden;",
        "text-decoration-style: wavy;",
        "line-through",
        "overline",
        "blink",
        "color: #040506;",
        "background-color: #010203;",
        "text-decoration-color: #070809;",
    ] {
        assert!(
            html.contains(expected),
            "missing bounded rich attribute {expected}"
        );
    }
    assert!(html.contains("href=\"https://example.invalid/?a=&quot;x&quot;&amp;b=&#39;y&#39;\""));
}

#[test]
fn copy_rectangle_and_wide_tail_endpoints_include_complete_graphemes() {
    let mut emulator = emulator(8, 3);
    emulator.feed("ab😀e\u{301}  \r\n01234567".as_bytes());
    let topology = TopologyPageRequest::new(
        stamp(&emulator),
        None,
        PageLimits::new(256, 16384, 1024 * 1024).unwrap(),
    )
    .unwrap();
    let ContentReadResult::Ready(page) = emulator.topology_page(&topology).unwrap() else {
        panic!("expected topology")
    };
    let reference = |row: usize, column| CellRef {
        session: topology.stamp().base().session,
        row: page.rows()[row].row(),
        row_revision: page.rows()[row].revision(),
        column,
    };
    let selected =
        ContentSelection::new(topology.stamp(), reference(0, 3), reference(0, 3), false).unwrap();
    let request = CopyRequest::new(
        topology.stamp(),
        selected,
        CopyOptions {
            unwrap_soft_wraps: true,
            trim_trailing_spaces: true,
            include_html: false,
        },
    )
    .unwrap();
    assert_eq!(copy_result(&mut emulator, &request).0, "😀");
    let rectangle =
        ContentSelection::new(topology.stamp(), reference(0, 2), reference(1, 4), true).unwrap();
    let request = CopyRequest::new(
        topology.stamp(),
        rectangle,
        CopyOptions {
            unwrap_soft_wraps: true,
            trim_trailing_spaces: true,
            include_html: false,
        },
    )
    .unwrap();
    assert_eq!(copy_result(&mut emulator, &request).0, "😀e\u{301}\n234");
}

#[test]
fn find_streams_every_history_match_and_maps_unicode_endpoints() {
    let mut emulator = emulator(20, 3);
    for _ in 0..700 {
        emulator.feed("Needle 😀e\u{301}\r\n".as_bytes());
    }
    let request = FindRequest::new(
        stamp(&emulator),
        BoundedFindQuery::new("nEEdLe".into()).unwrap(),
        None,
    )
    .unwrap();
    let mut job = emulator.begin_find(&request).unwrap();
    let mut total = 0;
    let mut chunks = 0;
    let mut completed = None;
    for _ in 0..100_000 {
        match emulator
            .step_find(&mut job, ContentScanBudget::default())
            .unwrap()
        {
            FindStep::Pending => {}
            FindStep::Complete => break,
            FindStep::Item(FindStreamItem::Chunk(chunk)) => {
                assert_eq!(chunk.index(), chunks);
                assert!(chunk.matches().len() <= MAX_FIND_CHUNK_MATCHES);
                total += chunk.matches().len();
                chunks += 1;
            }
            FindStep::Item(FindStreamItem::Complete { chunks, .. }) => completed = Some(chunks),
            FindStep::Item(FindStreamItem::Failed { .. }) => panic!("Find failed"),
        }
    }
    assert_eq!(total, 700);
    assert!(chunks > 1);
    assert_eq!(completed, Some(chunks));
    let request = FindRequest::new(
        stamp(&emulator),
        BoundedFindQuery::new("😀e\u{301}".into()).unwrap(),
        None,
    )
    .unwrap();
    let mut job = emulator.begin_find(&request).unwrap();
    let mut found = None;
    for _ in 0..100 {
        if let FindStep::Item(FindStreamItem::Chunk(chunk)) = emulator
            .step_find(&mut job, ContentScanBudget::default())
            .unwrap()
        {
            found = Some(chunk.matches()[0]);
            break;
        }
    }
    let found = found.unwrap();
    assert_eq!(found.start().column, 7);
    assert_eq!(found.end().column, 9);
}

#[test]
fn find_revision_change_fails_and_never_completes_a_partial_scan() {
    let mut emulator = emulator(20, 3);
    emulator.feed(b"needle");
    let request = FindRequest::new(
        stamp(&emulator),
        BoundedFindQuery::new("needle".into()).unwrap(),
        None,
    )
    .unwrap();
    let mut job = emulator.begin_find(&request).unwrap();
    emulator.feed(b"changed");
    assert!(
        matches!(emulator.step_find(&mut job, ContentScanBudget::default()).unwrap(), FindStep::Item(FindStreamItem::Failed { stamp, reason: ContentReadFailure::StaleAuthority }) if stamp == request.stamp())
    );
    assert!(matches!(
        emulator
            .step_find(&mut job, ContentScanBudget::default())
            .unwrap(),
        FindStep::Complete
    ));
}

#[test]
fn rich_copy_streams_complete_html_above_one_mebibyte() {
    let mut emulator = emulator(80, 3);
    emulator.feed(b"\x1b[1m");
    for _ in 0..500 {
        emulator.feed(format!("{}\r\n", "x".repeat(80)).as_bytes());
    }
    let (stamp, selection) = selection(&mut emulator);
    let request = CopyRequest::new(
        stamp,
        selection,
        CopyOptions {
            unwrap_soft_wraps: true,
            trim_trailing_spaces: true,
            include_html: true,
        },
    )
    .unwrap();
    let (plain, html, _) = copy_result(&mut emulator, &request);
    assert_eq!(plain.matches('x').count(), 500 * 80);
    assert!(html.len() > 1024 * 1024);
    assert_eq!(html.matches("font-weight: bold;").count(), 500 * 80);
}

#[test]
fn copy_defers_wrap_spaces_and_trailing_blank_rows_like_the_formatter() {
    for trim in [true, false] {
        let mut emulator = emulator(4, 4);
        emulator.feed(b"ab  cd");
        let (stamp, selection) = selection(&mut emulator);
        let request = CopyRequest::new(
            stamp,
            selection,
            CopyOptions {
                unwrap_soft_wraps: true,
                trim_trailing_spaces: trim,
                include_html: true,
            },
        )
        .unwrap();
        let (plain, html, _) = copy_result(&mut emulator, &request);
        assert_eq!(plain, "ab  cd");
        assert!(html.contains("ab  cd</div>"));
    }
    let mut emulator = emulator(8, 4);
    emulator.feed(b"text  \r\n\r\n");
    let (stamp, selection) = selection(&mut emulator);
    for (trim, expected) in [(true, "text"), (false, "text  ")] {
        let request = CopyRequest::new(
            stamp,
            selection,
            CopyOptions {
                unwrap_soft_wraps: true,
                trim_trailing_spaces: trim,
                include_html: true,
            },
        )
        .unwrap();
        assert_eq!(copy_result(&mut emulator, &request).0, expected);
    }
}

#[test]
fn rich_copy_preserves_styled_and_background_only_trailing_blanks() {
    let mut emulator = emulator(8, 2);
    emulator.feed(b"a\x1b[1m \x1b[0;48;2;1;2;3m\x1b[K\x1b[0m");
    let (stamp, selection) = selection(&mut emulator);
    let request = CopyRequest::new(
        stamp,
        selection,
        CopyOptions {
            unwrap_soft_wraps: true,
            trim_trailing_spaces: true,
            include_html: true,
        },
    )
    .unwrap();
    let (plain, html, _) = copy_result(&mut emulator, &request);
    assert_eq!(plain, "a");
    assert!(html.contains("<span style=\"font-weight: bold;\"> </span>"));
    assert_eq!(
        html.matches("<span style=\"background-color: #010203;\"> </span>")
            .count(),
        6
    );
}

#[test]
fn copy_normalizes_an_unwrapped_spacer_head_end_to_the_next_grapheme() {
    let mut emulator = emulator(3, 2);
    emulator.feed("1A⚡".as_bytes());
    let stamp = stamp(&emulator);
    let request = TopologyPageRequest::new(
        stamp,
        None,
        PageLimits::new(256, 16384, 1024 * 1024).unwrap(),
    )
    .unwrap();
    let ContentReadResult::Ready(page) = emulator.topology_page(&request).unwrap() else {
        panic!("topology")
    };
    let cell = |column| CellRef {
        session: stamp.base().session,
        row: page.rows()[0].row(),
        row_revision: page.rows()[0].revision(),
        column,
    };
    let selection = ContentSelection::new(stamp, cell(0), cell(2), false).unwrap();
    let request = CopyRequest::new(
        stamp,
        selection,
        CopyOptions {
            unwrap_soft_wraps: true,
            trim_trailing_spaces: true,
            include_html: false,
        },
    )
    .unwrap();
    assert_eq!(copy_result(&mut emulator, &request).0, "1A⚡");
    let next = CellRef {
        session: stamp.base().session,
        row: page.rows()[1].row(),
        row_revision: page.rows()[1].revision(),
        column: 0,
    };
    let selection = ContentSelection::new(stamp, cell(2), next, false).unwrap();
    let request = CopyRequest::new(stamp, selection, request.options()).unwrap();
    assert_eq!(copy_result(&mut emulator, &request).0, "⚡");
}

#[test]
fn copy_normalizes_spacer_head_after_a_byte_limited_linked_fragment() {
    let mut emulator = emulator(1024, 2);
    let link = format!("https://example.invalid/{}", "x".repeat(1950));
    emulator.feed(format!("\x1b]8;;{link}\x1b\\{}⚡", "x".repeat(1023)).as_bytes());
    let stamp = stamp(&emulator);
    let request = TopologyPageRequest::new(
        stamp,
        None,
        PageLimits::new(256, 16384, 1024 * 1024).unwrap(),
    )
    .unwrap();
    let ContentReadResult::Ready(page) = emulator.topology_page(&request).unwrap() else {
        panic!("topology")
    };
    let cell = |column| CellRef {
        session: stamp.base().session,
        row: page.rows()[0].row(),
        row_revision: page.rows()[0].revision(),
        column,
    };
    let selection = ContentSelection::new(stamp, cell(1022), cell(1023), false).unwrap();
    let request = CopyRequest::new(
        stamp,
        selection,
        CopyOptions {
            unwrap_soft_wraps: true,
            trim_trailing_spaces: true,
            include_html: false,
        },
    )
    .unwrap();
    assert_eq!(copy_result(&mut emulator, &request).0, "x⚡");
}

#[test]
fn streamed_plain_copy_matches_the_pinned_formatter_whitespace_matrix() {
    for (columns, bytes) in [
        (4, &b"ab  cd"[..]),
        (8, &b"a\r\n\r\nb\r\n\r\n"[..]),
        (4, &b"abcd    xy"[..]),
        (3, "1A⚡".as_bytes()),
        (8, &b"a\x1b[1m \x1b[0;48;2;1;2;3m\x1b[K\x1b[0m"[..]),
    ] {
        let mut semantic = emulator(columns, 4);
        semantic.feed(bytes);
        let (stamp, selected) = selection(&mut semantic);
        let mut native = libghostty_vt::Terminal::new(libghostty_vt::TerminalOptions {
            cols: columns,
            rows: 4,
            max_scrollback: 10_000,
            max_scrollback_bytes: 50_000_000,
        })
        .unwrap();
        native.vt_write(bytes);
        let rows = libghostty_vt::content::State::new()
            .unwrap()
            .capture(&native)
            .unwrap()
            .total_rows;
        let start = native
            .grid_ref(libghostty_vt::terminal::Point::Screen(
                libghostty_vt::terminal::PointCoordinate { x: 0, y: 0 },
            ))
            .unwrap();
        let end = native
            .grid_ref(libghostty_vt::terminal::Point::Screen(
                libghostty_vt::terminal::PointCoordinate {
                    x: columns - 1,
                    y: rows - 1,
                },
            ))
            .unwrap();
        let selection = libghostty_vt::selection::Selection::new(start, end, false);
        for unwrap in [false, true] {
            for trim in [false, true] {
                let options = libghostty_vt::selection::FormatOptions::new()
                    .with_selection(&selection)
                    .with_emit_format(libghostty_vt::fmt::Format::Plain)
                    .with_unwrap(unwrap)
                    .with_trim(trim);
                let expected = native
                    .format_selection_alloc(None, options)
                    .unwrap()
                    .unwrap();
                let request = CopyRequest::new(
                    stamp,
                    selected,
                    CopyOptions {
                        unwrap_soft_wraps: unwrap,
                        trim_trailing_spaces: trim,
                        include_html: false,
                    },
                )
                .unwrap();
                assert_eq!(
                    copy_result(&mut semantic, &request).0.as_bytes(),
                    expected.as_ref()
                );
            }
        }
    }
}
