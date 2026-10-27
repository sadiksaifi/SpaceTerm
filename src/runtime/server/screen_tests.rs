use super::tests::{attach, await_text, command, host, keyframe, size};
use super::*;
use spaceterm_server::protocol::interim::ScreenFrame;

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
    let update = keyframe(&screen);
    let Some(Ok(graphics)) = &update.graphics else {
        panic!("the keyframe carries the graphics manifest")
    };
    assert_eq!(graphics.placements[0].destination_width, 26);
    let bytes = super::dispatch::wire_bytes(Reply::Screen(Box::new(ScreenFrame {
        frame: 1,
        base: None,
        update,
    })));
    assert!(
        bytes.is_some_and(|bytes| bytes < 4096),
        "pixels stay out of the frame"
    );
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
    let update = keyframe(&screen);
    assert_eq!(update.links.len(), 1);
    let reply = Reply::Screen(Box::new(ScreenFrame {
        frame: 1,
        base: None,
        update,
    }));
    assert!(super::dispatch::wire_bytes(reply).is_some());
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
    // Links beyond the channel's table lose their target, not their text.
    let update = keyframe(&screen);
    assert_eq!(
        update.links.len(),
        spaceterm_server::protocol::limits::MAX_LINKS
    );
    assert!(update.rows[0].runs[0].link.is_some());
    assert!(update.rows[0].runs[0].text.starts_with('f'));
    let last = update.rows.last().unwrap().runs.last().unwrap();
    assert!(last.link.is_none());
    assert_eq!(update.rows.len(), 40);
    assert!(
        super::dispatch::wire_bytes(Reply::Screen(Box::new(ScreenFrame {
            frame: 1,
            base: None,
            update,
        })))
        .is_some()
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
    let Reply::Screen(reset) = host.request(
        [1; 32],
        lease.connection,
        Request::PollChanges { lease, base: None },
    ) else {
        panic!("a poll without a base receives a keyframe")
    };
    assert!(reset.base.is_none());
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
            base: Some(reset.frame),
        },
    );
    assert!(matches!(
        &reply,
        Reply::Screen(changes) if changes.base == Some(reset.frame)
            && !changes.update.rows.is_empty()
            && changes.update.rows.len() < 24
    ));
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
                base: Some(reset.frame)
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
            Reply::Screen(_)
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
