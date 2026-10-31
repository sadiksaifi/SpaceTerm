use super::tests::{attach, command, host, size};
use super::*;
use pane::accessibility::{self as wire, Error};
use std::time::Instant;

fn snapshot(host: &mut ServerPaneHost, lease: Lease, base: Option<u64>) -> wire::Snapshot {
    let deadline = Instant::now() + Duration::from_secs(60);
    let mut revision = None;
    let mut bytes = Vec::new();
    loop {
        match host.request(
            [1; 32],
            lease.connection,
            Request::Accessibility {
                lease,
                revision,
                offset: bytes.len(),
                base,
            },
        ) {
            Reply::Accessibility(Ok(chunk)) => {
                assert_eq!(chunk.offset, bytes.len());
                assert!(chunk.bytes.len() <= wire::CHUNK_BYTES);
                assert!(chunk.total <= wire::MAX_BYTES);
                revision = Some(chunk.revision);
                bytes.extend_from_slice(&chunk.bytes);
                if bytes.len() == chunk.total {
                    return serde_json::from_slice(&bytes).unwrap();
                }
            }
            Reply::Accessibility(Err(Error::Pending)) => {}
            other => panic!("accessibility unavailable: {other:?}"),
        }
        assert!(
            Instant::now() < deadline,
            "accessible snapshot did not complete"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn server_accessibility_fetches_history_and_rejects_a_disconnected_or_replaced_lease() {
    let mut host = host();
    let lease = attach(&mut host, 1, true);
    command(
        &mut host,
        1,
        lease,
        "stty -echo; printf '\\033[2J\\033[H'; i=0; while [ $i -lt 600 ]; do printf 'accessible-history-%04d\\r\\n' $i; i=$((i+1)); done",
    );
    let deadline = Instant::now() + Duration::from_secs(60);
    let first = loop {
        let model = snapshot(&mut host, lease, None);
        if model.rows.iter().any(|r| {
            r.cells
                .iter()
                .map(|c| c.text.as_str())
                .collect::<String>()
                .contains("accessible-history-0599")
        }) {
            break model;
        }
        assert!(Instant::now() < deadline);
    };
    assert!(first.topology.len() > 600);
    assert!(
        first.visible_lines.start > 0,
        "history must extend outside the viewport"
    );
    let id = first
        .rows
        .iter()
        .find(|r| {
            r.cells
                .iter()
                .map(|c| c.text.as_str())
                .collect::<String>()
                .contains("accessible-history-0001")
        })
        .unwrap()
        .id;
    host.disconnected(lease.connection);
    assert!(matches!(
        host.request(
            [1; 32],
            lease.connection,
            Request::Accessibility {
                lease,
                revision: None,
                offset: 0,
                base: None
            }
        ),
        Reply::Refused(Refusal::Stale)
    ));
    let resumed = attach(&mut host, 2, false);
    let after = snapshot(&mut host, resumed, None);
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
        host.request(
            [1; 32],
            resumed.connection,
            Request::Accessibility {
                lease: resumed,
                revision: None,
                offset: 0,
                base: None
            }
        ),
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
    let deadline = Instant::now() + Duration::from_secs(60);
    let row = loop {
        let model = snapshot(&mut host, lease, None);
        if let Some(row) = model.rows.into_iter().find(|row| {
            row.cells
                .iter()
                .map(|c| c.text.as_str())
                .collect::<String>()
                .starts_with("native-selection-target")
        }) {
            break row;
        }
        assert!(Instant::now() < deadline);
    };
    let reference = |column| wire::CellRef {
        row: row.id,
        row_revision: row.revision,
        column,
    };
    let selection = wire::SelectionRefs {
        start: reference(0),
        end: reference(5),
        rectangle: false,
    };
    assert!(matches!(
        host.request(
            [1; 32],
            lease.connection,
            Request::AccessibilitySelection {
                lease,
                focus_generation: 2,
                selection: Some(selection)
            }
        ),
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
        host.request(
            [1; 32],
            lease.connection,
            Request::AccessibilitySelection {
                lease,
                focus_generation: 2,
                selection: Some(selection)
            }
        ),
        Reply::Unchanged
    ));
    loop {
        if snapshot(&mut host, lease, None).selection == Some(selection) {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "native history selection not published"
        );
    }
    let rectangle = wire::SelectionRefs {
        rectangle: true,
        ..selection
    };
    assert!(matches!(
        host.request(
            [1; 32],
            lease.connection,
            Request::AccessibilitySelection {
                lease,
                focus_generation: 2,
                selection: Some(rectangle)
            }
        ),
        Reply::Refused(Refusal::Unsupported)
    ));
    host.request(
        [1; 32],
        lease.connection,
        Request::AccessibilitySelection {
            lease,
            focus_generation: 2,
            selection: None,
        },
    );
    loop {
        if snapshot(&mut host, lease, None).selection.is_none() {
            break;
        }
        assert!(Instant::now() < deadline);
    }
    let stale = wire::SelectionRefs {
        start: wire::CellRef {
            row_revision: row.revision.wrapping_add(1),
            ..reference(0)
        },
        ..selection
    };
    host.request(
        [1; 32],
        lease.connection,
        Request::AccessibilitySelection {
            lease,
            focus_generation: 2,
            selection: Some(stale),
        },
    );
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
    assert!(snapshot(&mut host, lease, None).selection.is_none());
    host.shutdown().unwrap();

    // Shape-only output changes must advance the wire revision, even with the same UTF-16 range.
    use crate::terminal::accessibility::{
        AccessibilityCell, AccessibilityRowUpdate, AccessibilityUpdate, TerminalAccessibilityState,
    };
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
                .into_iter()
                .map(|cell| AccessibilityCell::at_column(cell.text, cell.column, cell.width_cells))
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

#[test]
fn accessible_transfers_reuse_unchanged_history_and_finish_during_output() {
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
    let original = complete(&mut emulator);
    emulator.feed(b" appended");
    let updated = complete(&mut emulator);
    let mut state = accessibility::Accessibility::default();
    state.update(Arc::clone(&original));
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut revision = None;
    let mut bytes = Vec::new();
    // Change the live model while the first serialization is running. Its captured snapshot
    // must still finish instead of chasing every subsequent terminal update.
    loop {
        let result = state.chunk(revision, bytes.len(), None);
        state.update(Arc::clone(&updated));
        match result {
            Ok(chunk) => {
                revision = Some(chunk.revision);
                bytes.extend_from_slice(&chunk.bytes);
                if bytes.len() == chunk.total {
                    break;
                }
            }
            Err(Error::Pending) => {}
            other => panic!("{other:?}"),
        }
        assert!(Instant::now() < deadline);
        std::thread::yield_now();
    }
    let first: wire::Snapshot = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(first.revision, 1);
    bytes.clear();
    revision = None;
    loop {
        match state.chunk(revision, bytes.len(), Some(first.revision)) {
            Ok(chunk) => {
                revision = Some(chunk.revision);
                bytes.extend_from_slice(&chunk.bytes);
                if bytes.len() == chunk.total {
                    break;
                }
            }
            Err(Error::Pending) => {}
            other => panic!("{other:?}"),
        }
        assert!(Instant::now() < deadline);
        std::thread::yield_now();
    }
    let delta: wire::Snapshot = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(delta.topology, first.topology);
    assert_eq!(
        delta.rows.len(),
        1,
        "unchanged history must not be retransmitted"
    );
    assert!(
        delta.rows[0]
            .cells
            .iter()
            .map(|c| c.text.as_str())
            .collect::<String>()
            .starts_with("second row appended")
    );
}

#[test]
fn takeover_semantics_ignore_models_published_before_the_reading_reset() {
    let old = ScreenSnapshot::empty(crate::local_path::LocalPathSemantics::Posix);
    let mut reset = (*old).clone();
    reset.generation = PresentationGeneration::test(2);
    let mut state = accessibility::Accessibility::default();
    state.update(Arc::new(TerminalAccessibilityModel::from_screen(&old)));
    state.reset();
    state.bind_reset_screen(reset.generation);
    state.update(Arc::new(TerminalAccessibilityModel::from_screen(&old)));
    assert_eq!(state.chunk(None, 0, None).unwrap_err(), Error::Pending);
    state.update(Arc::new(TerminalAccessibilityModel::from_screen(&reset)));
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        match state.chunk(None, 0, None) {
            Ok(_) => break,
            Err(Error::Pending) => std::thread::yield_now(),
            other => panic!("{other:?}"),
        }
        assert!(Instant::now() < deadline);
    }
}
