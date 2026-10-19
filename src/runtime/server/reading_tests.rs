use super::tests::{attach, command, host};
use super::*;
use pane::reading::{Operation, SELECTION_CHUNK_BYTES, Scroll};
use std::time::Instant;

// Real PTY output and selection formatting share CPU with the parallel portable suite.
// These waits check completion, not a terminal throughput or response-time guarantee.
const COMPLETION_TIMEOUT: Duration = Duration::from_secs(60);

fn read(host: &mut DevelopmentPaneHost, lease: Lease, operation: Operation) {
    let deadline = Instant::now() + COMPLETION_TIMEOUT;
    loop {
        match host.request(
            [1; 32],
            lease.connection,
            Request::Read {
                lease,
                operation: operation.clone(),
            },
        ) {
            Reply::Reading { accepted: true } => return,
            Reply::Reading { accepted: false } => {}
            reply => panic!("reading refused: {reply:?}"),
        }
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(5));
    }
}
#[track_caller]
fn screen(
    host: &mut DevelopmentPaneHost,
    lease: Lease,
    matches: impl Fn(&Screen) -> bool,
) -> Box<Screen> {
    let deadline = Instant::now() + COMPLETION_TIMEOUT;
    let mut last_progress = None;
    loop {
        match host.request([1; 32], lease.connection, Request::Poll { lease }) {
            Reply::Screen(screen) if matches(&screen) => return screen,
            Reply::Screen(screen) => {
                last_progress = Some((
                    screen.generation,
                    screen.reading.as_ref().map(|reading| reading.total_rows),
                ));
            }
            Reply::Unchanged => {}
            reply => panic!("screen refused: {reply:?}"),
        }
        assert!(
            Instant::now() < deadline,
            "reading screen missing; last generation and retained rows: {last_progress:?}"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}
fn pointer(
    host: &mut DevelopmentPaneHost,
    lease: Lease,
    generation: u64,
    phase: pane::PointerPhase,
    y: f32,
) {
    read(
        host,
        lease,
        Operation::Pointer(pane::Pointer {
            screen_generation: generation,
            phase,
            button: Some(pane::PointerButton::Left),
            x: 1.0,
            y,
            modifiers: 0,
            shift_selection: false,
        }),
    );
}

#[test]
fn old_scrollback_find_and_multichunk_copy_use_the_retained_terminal_content() {
    let gate = std::env::temp_dir().join(format!(
        "spaceterm-reading-{:x}",
        u128::from_ne_bytes(nonce().unwrap()[..16].try_into().unwrap())
    ));
    let quoted_gate = format!("'{}'", gate.to_string_lossy().replace('\'', "'\\''"));
    let mut host = host();
    let lease = attach(&mut host, 1, true);
    command(
        &mut host,
        1,
        lease,
        &format!(
            "stty -echo; printf '\\033[2J\\033[H'; i=0; while [ $i -lt 2000 ]; do printf 'retained-%04d-abcdefghijklmnopqrstuvwxyz\\n' $i; i=$((i+1)); done; printf 'FINISHED\\n'; while [ ! -f {quoted_gate} ]; do sleep 0.01; done; i=0; while [ -f {quoted_gate} ]; do printf 'live-%s\\n' \"$i\"; i=$((i+1)); sleep 0.02; done"
        ),
    );
    screen(&mut host, lease, |s| {
        s.reading.as_ref().unwrap().total_rows >= 2000
    });
    read(
        &mut host,
        lease,
        Operation::Query {
            generation: 7,
            text: "retained-0000".into(),
        },
    );
    let found = screen(&mut host, lease, |s| {
        s.reading
            .as_ref()
            .unwrap()
            .find
            .as_ref()
            .is_some_and(|f| f.generation == 7 && f.total == 1)
    });
    assert!(found.reading.as_ref().unwrap().offset_rows > 0);
    read(
        &mut host,
        lease,
        Operation::Navigate {
            generation: 7,
            backwards: false,
        },
    );
    let found = screen(&mut host, lease, |s| {
        s.reading
            .as_ref()
            .unwrap()
            .find
            .as_ref()
            .is_some_and(|f| f.current == Some(1))
    });
    assert!(found.rows.iter().any(|r| {
        r.iter()
            .map(|c| c.text.as_str())
            .collect::<String>()
            .contains("retained-0000")
    }));
    let top = if found.reading.as_ref().unwrap().offset_rows == 0 {
        found
    } else {
        read(&mut host, lease, Operation::Scroll(Scroll::Top));
        screen(&mut host, lease, |s| {
            s.reading.as_ref().unwrap().offset_rows == 0
        })
    };
    pointer(
        &mut host,
        lease,
        top.generation,
        pane::PointerPhase::Press,
        1.0,
    );
    // Reading acknowledgements fence worker admission, not formatting on the dispatch thread.
    read(&mut host, lease, Operation::Scroll(Scroll::Bottom));
    let bottom = screen(&mut host, lease, |s| {
        let r = s.reading.as_ref().unwrap();
        r.offset_rows + r.visible_rows == r.total_rows
    });
    pointer(
        &mut host,
        lease,
        bottom.generation,
        pane::PointerPhase::Motion,
        400.0,
    );
    let selected = screen(&mut host, lease, |s| {
        s.reading.as_ref().unwrap().selection_present
    });
    pointer(
        &mut host,
        lease,
        selected.generation,
        pane::PointerPhase::Release,
        400.0,
    );
    // Fence the release on the existing worker barrier before preparing the copy.
    let deadline = Instant::now() + COMPLETION_TIMEOUT;
    let mut generation = loop {
        match host.request([1; 32], lease.connection, Request::ReadingStatus { lease }) {
            Reply::ReadingReady {
                selection_revision,
                selection_present: true,
                ..
            } => break selection_revision,
            Reply::SelectionPending => {}
            reply => panic!("selection release refused: {reply:?}"),
        }
        assert!(
            Instant::now() < deadline,
            "selection release did not complete"
        );
        std::thread::sleep(Duration::from_millis(5));
    };
    let initial_rows = selected.reading.as_ref().unwrap().total_rows;
    let mut bytes = Vec::new();
    let mut chunks = 0;
    let mut output_started = false;
    let mut output_during_transfer = false;
    let deadline = Instant::now() + COMPLETION_TIMEOUT;
    loop {
        let update = if !bytes.is_empty() && !output_started {
            // Start real output only after receiving a partial copy, and witness it before
            // requesting the remaining chunks. Transfer speed cannot skip the interleaving.
            std::fs::write(&gate, []).unwrap();
            output_started = true;
            Some(screen(&mut host, lease, |s| {
                s.reading.as_ref().unwrap().total_rows > initial_rows
            }))
        } else if let Reply::Screen(s) =
            host.request([1; 32], lease.connection, Request::Poll { lease })
        {
            Some(s)
        } else {
            None
        };
        if let Some(s) = update {
            let reading = s.reading.as_ref().unwrap();
            output_during_transfer |= !bytes.is_empty() && reading.total_rows > initial_rows;
            let current = reading.selection_revision;
            if generation != current {
                generation = current;
                bytes.clear();
                chunks = 0;
            }
        }
        match host.request(
            [1; 32],
            lease.connection,
            Request::SelectionChunk {
                lease,
                generation,
                offset: bytes.len(),
            },
        ) {
            Reply::SelectionChunk {
                generation: actual,
                offset,
                total,
                bytes: part,
            } => {
                assert_eq!(actual, generation);
                assert_eq!(offset, bytes.len());
                assert!(part.len() <= SELECTION_CHUNK_BYTES);
                bytes.extend(part);
                chunks += 1;
                if bytes.len() == total {
                    break;
                }
            }
            Reply::SelectionPending | Reply::Refused(Refusal::Stale) => {}
            reply => panic!("selection refused: {reply:?}"),
        }
        assert!(Instant::now() < deadline, "complete selection missing");
        std::thread::sleep(Duration::from_millis(40));
    }
    let copy: pane::reading::Selection = serde_json::from_slice(&bytes).unwrap();
    assert!(chunks > 2);
    assert!(
        output_during_transfer,
        "output must continue during this transfer"
    );
    assert!(
        copy.plain_text
            .contains("retained-0000-abcdefghijklmnopqrstuvwxyz")
    );
    assert!(
        copy.plain_text
            .contains("retained-1999-abcdefghijklmnopqrstuvwxyz")
    );
    assert!(copy.html.is_some());
    std::fs::remove_file(gate).unwrap();
    let Reply::Attached { lease: other } = host.request(
        [2; 32],
        [2; 32],
        Request::TakeOver {
            target: lease.into(),
            size: super::tests::size(),
            style: None,
        },
    ) else {
        panic!("takeover refused")
    };
    assert!(matches!(
        host.request(
            [1; 32],
            lease.connection,
            Request::SelectionChunk {
                lease,
                generation,
                offset: 0
            }
        ),
        Reply::Refused(Refusal::Stale)
    ));
    let deadline = Instant::now() + COMPLETION_TIMEOUT;
    loop {
        if let Reply::Screen(screen) =
            host.request([2; 32], other.connection, Request::Poll { lease: other })
        {
            let reading = screen.reading.unwrap();
            assert!(!reading.selection_present);
            assert!(reading.find.is_none());
            break;
        }
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(5));
    }
    host.shutdown().unwrap();
}

#[test]
fn copy_shortcuts_use_the_authoritative_keyboard_mode_and_focus_fence() {
    use super::tests::{await_text, focus};
    for (modifiers, expected) in [
        (8, "1b5b39393b39751b5b39393b393a3375"),
        (5, "1b5b39393b36751b5b39393b363a3375"),
    ] {
        let mut host = host();
        let lease = attach(&mut host, 1, true);
        command(
            &mut host,
            1,
            lease,
            "set +m; stty raw -echo; printf '\\033[2J\\033[HREADY\\r\\n'; dd bs=1 count=1 >/dev/null 2>/dev/null; printf '\\033[>11uENHANCED\\r\\n'; dd bs=1 count=16 2>/dev/null | od -An -tx1 | tr -d ' \\n'; printf '\\r\\n'",
        );
        await_text(&mut host, 1, lease, "READY");
        // Finish the command's Enter gesture before enabling keyboard release reports.
        focus(&mut host, lease, 1, FocusTarget::Terminal);
        focus(&mut host, lease, 2, FocusTarget::FocusedPane);
        focus(&mut host, lease, 3, FocusTarget::Terminal);
        assert!(matches!(
            host.request(
                [1; 32],
                lease.connection,
                Request::Input {
                    lease,
                    generation: 3,
                    input: InteractiveInput::Key(Key {
                        physical: PhysicalKey::Unidentified as u32,
                        action: 0,
                        logical: "text-input".into(),
                        text: Some("!".into()),
                        unshifted: None,
                        modifiers: 0,
                        consumed: 0,
                        option_as_alt: false,
                    }),
                }
            ),
            Reply::Unchanged
        ));
        await_text(&mut host, 1, lease, "ENHANCED");
        focus(&mut host, lease, 4, FocusTarget::FocusedPane);
        let request = |generation| Request::Input {
            lease,
            generation,
            input: InteractiveInput::CopyOrForward { modifiers },
        };
        assert!(matches!(
            host.request([1; 32], lease.connection, request(3)),
            Reply::Refused(Refusal::Stale)
        ));
        assert!(matches!(
            host.request([1; 32], lease.connection, request(4)),
            Reply::Refused(Refusal::Stale)
        ));
        focus(&mut host, lease, 5, FocusTarget::Terminal);
        assert!(matches!(
            host.request([1; 32], lease.connection, request(5)),
            Reply::Unchanged
        ));
        await_text(&mut host, 1, lease, expected);
        host.shutdown().unwrap();
    }
}

#[test]
fn reading_completion_refuses_orphan_motion_resize_and_retired_leases() {
    fn complete(host: &mut DevelopmentPaneHost, lease: Lease) -> Result<(), Refusal> {
        let deadline = Instant::now() + COMPLETION_TIMEOUT;
        loop {
            match host.request([1; 32], lease.connection, Request::ReadingStatus { lease }) {
                Reply::ReadingReady { .. } => return Ok(()),
                Reply::Refused(refusal) => return Err(refusal),
                Reply::SelectionPending => {}
                other => panic!("unexpected completion: {other:?}"),
            }
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(5));
        }
    }
    let mut host = host();
    let lease = attach(&mut host, 1, true);
    let observed = screen(&mut host, lease, |_| true);
    let operation = |phase| {
        Operation::Pointer(pane::Pointer {
            screen_generation: observed.generation,
            phase,
            button: Some(pane::PointerButton::Left),
            x: 1.0,
            y: 1.0,
            modifiers: 0,
            shift_selection: false,
        })
    };
    read(&mut host, lease, operation(pane::PointerPhase::Motion));
    assert!(matches!(complete(&mut host, lease), Err(Refusal::Stale)));
    let mut size = super::tests::size();
    size.columns += 1;
    assert!(matches!(
        host.request([1; 32], lease.connection, Request::Resize { lease, size }),
        Reply::Unchanged
    ));
    screen(&mut host, lease, |screen| screen.columns == size.columns);
    read(&mut host, lease, operation(pane::PointerPhase::Press));
    assert!(matches!(complete(&mut host, lease), Err(Refusal::Stale)));
    host.disconnected(lease.connection);
    assert!(matches!(
        host.request(
            [1; 32],
            lease.connection,
            Request::Read {
                lease,
                operation: operation(pane::PointerPhase::Press)
            }
        ),
        Reply::Refused(Refusal::Stale)
    ));
    let resumed = attach(&mut host, 3, false);
    screen(&mut host, resumed, |_| true);
    assert!(matches!(
        host.request(
            [1; 32],
            lease.connection,
            Request::Read {
                lease,
                operation: operation(pane::PointerPhase::Press)
            }
        ),
        Reply::Refused(Refusal::Stale)
    ));
    assert!(matches!(
        host.request(
            [2; 32],
            [2; 32],
            Request::TakeOver {
                target: resumed.into(),
                size,
                style: None
            }
        ),
        Reply::Attached { .. }
    ));
    assert!(matches!(
        host.request(
            [1; 32],
            resumed.connection,
            Request::Read {
                lease: resumed,
                operation: operation(pane::PointerPhase::Press)
            }
        ),
        Reply::Refused(Refusal::Stale)
    ));
    host.shutdown().unwrap();
}
