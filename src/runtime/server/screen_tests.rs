use super::tests::{attach, await_text, command, host, size};
use super::*;

#[test]
fn server_screen_carries_graphics_geometry_without_inline_pixels() {
    let _guard = crate::terminal::graphics::test_lock();
    let mut emulator = crate::terminal::emulator::TerminalEmulator::new(geometry(PaneSize {
        columns: 8,
        rows: 4,
        cell_width: 13,
        cell_height: 27,
    }))
    .unwrap();
    emulator.feed(b"\x1b_Ga=T,t=d,f=32,i=7,p=3,s=1,v=1,c=2,r=1;AQIDBA==\x1b\\");
    let screen = emulator.snapshot().unwrap().unwrap();
    let encoded = serde_json::to_value(encode_screen(&screen)).unwrap();
    assert_eq!(
        encoded["graphics"]["Ok"]["placements"][0]["destination_width"],
        26
    );
    assert!(!encoded.to_string().contains("rgba"));
}

#[test]
fn server_screen_with_a_long_hyperlink_remains_within_the_transport_limit() {
    let geometry = TerminalGeometry::from_grid(
        CellGridSize::new(120, 40),
        LogicalCellSize::new(10.0, 20.0),
        BackingScale::ONE,
    );
    let mut emulator = crate::terminal::emulator::TerminalEmulator::new(geometry).unwrap();
    let url = format!("https://example.test/{}", "a".repeat(3980));
    emulator.feed("x".repeat(120 * 39).as_bytes());
    let mut screen = (*emulator.snapshot().unwrap().unwrap()).clone();
    let link = Arc::new(crate::terminal::HyperlinkTarget::url(&url).unwrap());
    for row in Arc::make_mut(&mut screen.rows).iter_mut().take(39) {
        for cell in Arc::make_mut(row) {
            cell.hyperlink = Some(Arc::clone(&link));
        }
    }
    let encoded = encode_screen(&screen);
    assert_eq!(encoded.hyperlinks.len(), 1);
    let reply = Reply::Screen(Box::new(encoded));
    let bytes = serde_json::to_vec(&reply).unwrap();
    assert!(
        bytes.len() < pane::MAX_TERMINAL_FRAME,
        "screen frame has {} bytes",
        bytes.len()
    );
}

#[test]
fn server_screen_with_many_distinct_links_retains_text_with_bounded_metadata() {
    let geometry = TerminalGeometry::from_grid(
        CellGridSize::new(120, 40),
        LogicalCellSize::new(10.0, 20.0),
        BackingScale::ONE,
    );
    let mut emulator = crate::terminal::emulator::TerminalEmulator::new(geometry).unwrap();
    emulator.feed(b"fixture");
    let mut screen = (*emulator.snapshot().unwrap().unwrap()).clone();
    let mut index = 0;
    for row in Arc::make_mut(&mut screen.rows) {
        for cell in Arc::make_mut(row) {
            let url = format!("https://example.test/{index}/{}", "a".repeat(3970));
            cell.hyperlink = Some(Arc::new(
                crate::terminal::HyperlinkTarget::url(&url).unwrap(),
            ));
            index += 1;
        }
    }
    let encoded = encode_screen(&screen);
    assert!(encoded.rows[0][0].hyperlink.is_some());
    assert!(
        encoded
            .rows
            .last()
            .unwrap()
            .last()
            .unwrap()
            .hyperlink
            .is_none()
    );
    assert_eq!(encoded.rows[0][0].text, "f");
    assert!(
        serde_json::to_vec(&Reply::Screen(Box::new(encoded)))
            .unwrap()
            .len()
            < pane::MAX_TERMINAL_FRAME
    );
}

#[test]
fn row_changes_follow_the_exact_baseline_and_connection_lease() {
    let mut host = host();
    let lease = attach(&mut host, 1, true);
    command(
        &mut host,
        1,
        lease,
        "stty -echo; printf '\\033[2J\\033[HBASELINE-READY\\r\\n'",
    );
    await_text(&mut host, 1, lease, "BASELINE-READY");
    let mut decoder = pane::presentation::ScreenDecoder::default();
    let reset = host.request(
        [1; 32],
        lease.connection,
        Request::PollChanges { lease, base: None },
    );
    assert!(matches!(&reset, Reply::Screen(_)));
    decoder.apply(reset).unwrap();
    command(
        &mut host,
        1,
        lease,
        "printf '\\033[H\\033[2KCHANGED-ROW\\r\\n'",
    );
    await_text(&mut host, 1, lease, "CHANGED-ROW");
    let reply = host.request(
        [1; 32],
        lease.connection,
        Request::PollChanges {
            lease,
            base: decoder.baseline(),
        },
    );
    assert!(
        matches!(&reply, Reply::ScreenChanges(changes) if !changes.indices.is_empty() && changes.indices.len() < 24)
    );
    decoder.apply(reply).unwrap();
    assert!(matches!(
        host.request(
            [1; 32],
            lease.connection,
            Request::PollChanges { lease, base: None }
        ),
        Reply::Screen(_)
    ));
    let Reply::Attached { lease: replacement } = host.request(
        [1; 32],
        [2; 32],
        Request::Attach {
            create: false,
            size: size(),
            resume: Some(lease),
            style: None,
        },
    ) else {
        panic!("reattachment failed")
    };
    assert!(matches!(
        host.request(
            [1; 32],
            lease.connection,
            Request::PollChanges {
                lease,
                base: decoder.baseline()
            }
        ),
        Reply::Refused(Refusal::Stale)
    ));
    assert!(matches!(
        host.request(
            [1; 32],
            replacement.connection,
            Request::PollChanges {
                lease: replacement,
                base: None
            }
        ),
        Reply::Screen(_)
    ));
    host.shutdown().unwrap();
}

#[test]
fn hidden_presentation_suppresses_rows_and_reveal_resets_the_baseline() {
    let mut host = host();
    let lease = attach(&mut host, 1, true);
    command(
        &mut host,
        1,
        lease,
        "stty -echo; printf '\\033[2J\\033[HHIDDEN-READY\\r\\n'",
    );
    await_text(&mut host, 1, lease, "HIDDEN-READY");
    assert!(matches!(
        host.request(
            [1; 32],
            lease.connection,
            Request::PresentationDemand {
                lease,
                visible: false
            }
        ),
        Reply::Unchanged
    ));
    for _ in 0..10 {
        assert!(!matches!(
            host.request(
                [1; 32],
                lease.connection,
                Request::PollChanges { lease, base: None }
            ),
            Reply::Screen(_) | Reply::ScreenChanges(_)
        ));
    }
    command(
        &mut host,
        1,
        lease,
        "printf '\\033[H\\033[2KHIDDEN-OUTPUT\\r\\n'",
    );
    assert!(matches!(
        host.request(
            [1; 32],
            lease.connection,
            Request::PresentationDemand {
                lease,
                visible: true
            }
        ),
        Reply::Unchanged
    ));
    assert!(matches!(
        host.request(
            [1; 32],
            lease.connection,
            Request::PollChanges { lease, base: None }
        ),
        Reply::Screen(_)
    ));
    await_text(&mut host, 1, lease, "HIDDEN-OUTPUT");
    host.shutdown().unwrap();
}
