use super::tests::{
    attach, await_screen, await_text, command, create_shared, effects, focus, host, input, lines,
    native_command, screen, shared_inventory, size, text_key,
};
use super::*;
use spaceterm_server::protocol::{PaneEvent, screen::ScreenUpdate};
use std::time::{Duration, Instant};

#[test]
fn native_clipboard_requests_reach_only_the_current_controller() {
    let mut host = host();
    let lease = attach(&mut host, 1, true);
    focus(&mut host, lease, 1, FocusTarget::Terminal);
    native_command(&mut host, lease, 1, "printf '\\033]52;c;aGVsbG8=\\007'");
    let [request] = effects(&mut host, lease, 1).try_into().unwrap();
    assert!(
        matches!(request.effect, pane::effects::Effect::ClipboardWrite { text, .. } if text == "hello")
    );
    host.shutdown().unwrap();
}

#[test]
fn interactive_input_requires_current_focus_and_legacy_keys_cannot_bypass_it() {
    let mut host = host();
    let lease = attach(&mut host, 1, true);
    let key = Key {
        physical: PhysicalKey::Enter as u32,
        action: 0,
        logical: "enter".into(),
        text: None,
        unshifted: None,
        modifiers: 0,
        consumed: 0,
        option_as_alt: false,
    };
    assert!(matches!(
        host.request(
            [1; 32],
            [1; 32],
            Request::Focus {
                lease,
                generation: 1,
                target: FocusTarget::Unfocused
            }
        ),
        Reply::Unchanged
    ));
    assert!(matches!(
        host.request(
            [1; 32],
            [1; 32],
            Request::Key {
                lease,
                key: key.clone()
            }
        ),
        Reply::Refused(Refusal::Unsupported)
    ));
    assert!(matches!(
        host.request(
            [1; 32],
            [1; 32],
            Request::Input {
                lease,
                generation: 1,
                input: InteractiveInput::Key(key.clone())
            }
        ),
        Reply::Refused(Refusal::Stale)
    ));
    assert!(matches!(
        host.request(
            [1; 32],
            [1; 32],
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
            [1; 32],
            Request::Input {
                lease,
                generation: 2,
                input: InteractiveInput::Key(key)
            }
        ),
        Reply::Unchanged
    ));
    host.shutdown().unwrap();
}

fn capture(host: &mut DevelopmentPaneHost, lease: Lease, bytes: usize, modes: &str) {
    // macOS sh can race process-group creation when a short pipeline leader exits.
    // Keep this byte-capture pipeline in the shell's group so job-control diagnostics cannot corrupt its witness.
    command(
        host,
        1,
        lease,
        &format!(
            "set +m; stty raw -echo; printf '{modes}\\033[2J\\033[HREADY\\r\\n'; dd bs=1 count={bytes} 2>/dev/null | od -v -An -tx1 | tr -d ' \\n'; printf '\\r\\n'"
        ),
    );
    await_text(host, lease, "READY");
}

fn batch_key(text: &str, ime: bool) -> Key {
    let InteractiveInput::Key(key) = text_key(text, ime) else {
        unreachable!()
    };
    key
}

fn key_batch(host: &mut impl HostCommands, lease: Lease, generation: u64, keys: Vec<Key>) -> Reply {
    host.request(
        [1; 32],
        lease.connection,
        Request::Keys {
            lease,
            generation,
            keys,
        },
    )
}

#[test]
fn key_batches_reject_invalid_bounds_keys_and_authority_without_a_prefix() {
    let mut host = host();
    let lease = attach(&mut host, 1, true);
    capture(&mut host, lease, 33, "");
    focus(&mut host, lease, 1, FocusTarget::Terminal);
    let key = batch_key("A", false);
    let mut malformed = key.clone();
    malformed.action = 3;
    let mut overlong = key.clone();
    overlong.text = Some("a".repeat(4097));
    let mut overframe = key.clone();
    overframe.text = Some("a".repeat(spaceterm_server::protocol::limits::MAX_INPUT_BYTES + 1));
    for keys in [
        vec![],
        vec![key.clone(); MAX_KEY_BATCH + 1],
        vec![key.clone(), malformed],
        vec![key.clone(), overlong],
        vec![key.clone(), overframe],
    ] {
        assert!(matches!(
            key_batch(&mut host, lease, 1, keys),
            Reply::Refused(Refusal::Unsupported)
        ));
    }
    for generation in [0, 2] {
        assert!(matches!(
            key_batch(&mut host, lease, generation, vec![key.clone()]),
            Reply::Refused(Refusal::Stale)
        ));
    }
    let mut stale = lease;
    stale.control = [9; 32];
    assert!(matches!(
        key_batch(&mut host, stale, 1, vec![key.clone()]),
        Reply::Refused(Refusal::Stale)
    ));
    for (device, connection) in [([2; 32], lease.connection), ([1; 32], [2; 32])] {
        assert!(matches!(
            host.request(
                device,
                connection,
                Request::Keys {
                    lease,
                    generation: 1,
                    keys: vec![key.clone()]
                }
            ),
            Reply::Refused(Refusal::Stale)
        ));
    }
    focus(&mut host, lease, 2, FocusTarget::Unfocused);
    assert!(matches!(
        key_batch(&mut host, lease, 2, vec![key.clone()]),
        Reply::Refused(Refusal::Stale)
    ));
    focus(&mut host, lease, 3, FocusTarget::Terminal);
    assert!(matches!(
        key_batch(&mut host, lease, 3, vec![key; MAX_KEY_BATCH]),
        Reply::Unchanged
    ));
    assert!(matches!(
        key_batch(&mut host, lease, 3, vec![batch_key("Z", false)]),
        Reply::Unchanged
    ));
    // The first 33 actual PTY bytes must be exactly these accepted events. Any prefix
    // from an earlier refused request makes this independent byte witness impossible.
    await_text(&mut host, lease, &format!("{}5a", "41".repeat(32)));
    host.shutdown().unwrap();
}

#[test]
fn key_batches_preserve_kitty_press_repeat_release_order_at_the_pty() {
    let mut host = host();
    let lease = attach(&mut host, 1, true);
    capture(&mut host, lease, 23, "\\033[>11u");
    focus(&mut host, lease, 1, FocusTarget::Terminal);
    let keys = (0..=2)
        .map(|action| Key {
            physical: PhysicalKey::A as u32,
            action,
            logical: "a".into(),
            text: Some("a".into()),
            unshifted: Some('a'),
            modifiers: 0,
            consumed: 0,
            option_as_alt: false,
        })
        .collect();
    assert!(matches!(
        key_batch(&mut host, lease, 1, keys),
        Reply::Unchanged
    ));
    // Kitty flag 11 reports every key, including repeat (2) and release (3).
    await_text(
        &mut host,
        lease,
        "1b5b3937751b5b39373b313a32751b5b39373b313a3375",
    );
    host.shutdown().unwrap();
}

#[test]
fn key_batches_prevalidate_focused_pane_ime_eligibility_before_any_commit() {
    let mut host = host();
    let lease = attach(&mut host, 1, true);
    capture(&mut host, lease, 3, "");
    focus(&mut host, lease, 1, FocusTarget::FocusedPane);
    assert!(matches!(
        key_batch(
            &mut host,
            lease,
            1,
            vec![batch_key("X", true), batch_key("A", false)]
        ),
        Reply::Refused(Refusal::Stale)
    ));
    assert!(matches!(
        key_batch(&mut host, lease, 1, vec![batch_key("界", true)]),
        Reply::Unchanged
    ));
    await_text(&mut host, lease, "e7958c");
    host.shutdown().unwrap();
}

#[test]
fn focus_edges_and_focused_pane_ime_are_observable_at_the_pty() {
    let mut host = host();
    let lease = attach(&mut host, 1, true);
    capture(&mut host, lease, 16, "\\033[?1004h");
    focus(&mut host, lease, 1, FocusTarget::Terminal);
    focus(&mut host, lease, 2, FocusTarget::FocusedPane);
    assert!(matches!(
        input(&mut host, lease, 2, text_key("discarded", false)),
        Reply::Refused(Refusal::Stale)
    ));
    assert!(matches!(
        input(&mut host, lease, 2, text_key("界", true)),
        Reply::Unchanged
    ));
    focus(&mut host, lease, 3, FocusTarget::Unfocused);
    assert!(matches!(
        input(&mut host, lease, 3, text_key("discarded", true)),
        Reply::Refused(Refusal::Stale)
    ));
    focus(&mut host, lease, 4, FocusTarget::Terminal);
    assert!(matches!(
        input(&mut host, lease, 1, text_key("discarded", false)),
        Reply::Refused(Refusal::Stale)
    ));
    assert!(matches!(
        input(&mut host, lease, 4, text_key("K", false)),
        Reply::Unchanged
    ));
    await_text(&mut host, lease, "1b5b4f1b5b491b5b4fe7958c1b5b494b");
    host.shutdown().unwrap();
}

fn unsafe_paste(host: &mut DevelopmentPaneHost, lease: Lease, generation: u64) -> u64 {
    let Reply::Paste(PasteOutcome::ConfirmationRequired(confirmation)) = input(
        host,
        lease,
        generation,
        InteractiveInput::Paste {
            text: "a\nb".into(),
        },
    ) else {
        panic!("expected unsafe paste confirmation")
    };
    assert_eq!(confirmation.byte_len, 3);
    assert_eq!(confirmation.line_count, 2);
    assert!(confirmation.multiline);
    confirmation.id
}

#[test]
fn paste_confirmation_preserves_exact_modal_authority_and_focus_loss_cancels_it() {
    let mut host = host();
    let lease = attach(&mut host, 1, true);
    capture(&mut host, lease, 5, "");
    focus(&mut host, lease, 1, FocusTarget::Terminal);
    let first = unsafe_paste(&mut host, lease, 1);
    focus(
        &mut host,
        lease,
        1,
        FocusTarget::PasteConfirmation { id: first },
    );
    focus(&mut host, lease, 2, FocusTarget::Unfocused);
    focus(&mut host, lease, 3, FocusTarget::Terminal);
    assert!(matches!(
        input(
            &mut host,
            lease,
            3,
            InteractiveInput::ResolvePaste {
                id: first,
                confirm: true
            }
        ),
        Reply::Paste(PasteOutcome::Stale)
    ));
    let cancelled = unsafe_paste(&mut host, lease, 3);
    assert!(matches!(
        input(
            &mut host,
            lease,
            3,
            InteractiveInput::ResolvePaste {
                id: cancelled,
                confirm: false
            }
        ),
        Reply::Paste(PasteOutcome::Cancelled)
    ));
    assert!(matches!(
        input(
            &mut host,
            lease,
            3,
            InteractiveInput::Paste { text: "ok".into() }
        ),
        Reply::Paste(PasteOutcome::Written)
    ));
    let id = unsafe_paste(&mut host, lease, 3);
    assert_ne!(id, first);
    focus(&mut host, lease, 3, FocusTarget::PasteConfirmation { id });
    assert!(matches!(
        input(
            &mut host,
            lease,
            3,
            InteractiveInput::ResolvePaste { id, confirm: true }
        ),
        Reply::Paste(PasteOutcome::Written)
    ));
    assert!(matches!(
        input(
            &mut host,
            lease,
            3,
            InteractiveInput::ResolvePaste { id, confirm: true }
        ),
        Reply::Paste(PasteOutcome::Stale)
    ));
    await_text(&mut host, lease, "6f6b610d62");
    host.shutdown().unwrap();
}

fn screen_with_marker(host: &mut DevelopmentPaneHost, lease: Lease, marker: &str) -> ScreenUpdate {
    await_screen(host, [1; 32], lease, |update| {
        lines(update)
            .iter()
            .any(|row| row.trim_end().starts_with(marker))
    })
}

#[test]
fn redraw_between_pointer_press_and_release_does_not_swallow_the_next_click() {
    let mut host = host();
    let lease = attach(&mut host, 1, true);
    command(
        &mut host,
        1,
        lease,
        "set +m; stty raw -echo; printf '\\033[?1002h\\033[?1006h\\033[2J\\033[HREADY\\r\\n'; dd bs=1 count=9 >/dev/null 2>/dev/null; printf 'CHANGED\\r\\n'; reports=; while :; do byte=$(dd bs=1 count=1 2>/dev/null | od -An -tx1 | tr -d ' \\n'); [ \"$byte\" = 21 ] && break; reports=$reports$byte; done; printf 'R:%s\\r\\n' \"$reports\"",
    );
    let original = screen_with_marker(&mut host, lease, "READY");
    focus(&mut host, lease, 1, FocusTarget::Terminal);
    let press = Pointer {
        screen_generation: original.generation,
        phase: pane::PointerPhase::Press,
        button: Some(pane::PointerButton::Left),
        x: 4.0,
        y: 5.0,
        modifiers: 0,
        shift_selection: false,
    };
    assert!(matches!(
        input(&mut host, lease, 1, InteractiveInput::Pointer(press)),
        Reply::Unchanged
    ));
    let changed = screen_with_marker(&mut host, lease, "CHANGED");
    assert_ne!(changed.generation, original.generation);
    // An accepted application gesture continues across output in the same geometry.
    assert!(matches!(
        input(
            &mut host,
            lease,
            1,
            InteractiveInput::Pointer(Pointer {
                phase: pane::PointerPhase::Motion,
                x: 20.0,
                y: 23.0,
                ..press
            })
        ),
        Reply::Unchanged
    ));
    assert!(matches!(
        input(
            &mut host,
            lease,
            1,
            InteractiveInput::Pointer(Pointer {
                phase: pane::PointerPhase::Release,
                x: 24.0,
                y: 41.0,
                ..press
            })
        ),
        Reply::Unchanged
    ));
    let fresh = Pointer {
        screen_generation: changed.generation,
        x: 12.0,
        y: 23.0,
        ..press
    };
    for phase in [pane::PointerPhase::Press, pane::PointerPhase::Release] {
        assert!(matches!(
            input(
                &mut host,
                lease,
                1,
                InteractiveInput::Pointer(Pointer { phase, ..fresh })
            ),
            Reply::Unchanged
        ));
    }
    // With no active gesture, the old snapshot cannot start another click.
    input(&mut host, lease, 1, InteractiveInput::Pointer(press));
    input(&mut host, lease, 1, text_key("!", false));
    let screen = screen_with_marker(&mut host, lease, "R:");
    let report = lines(&screen)
        .into_iter()
        .find(|row| row.starts_with("R:"))
        .unwrap();
    assert_eq!(
        report.trim_end(),
        "R:1b5b3c33323b333b324d1b5b3c303b343b336d1b5b3c303b323b324d1b5b3c303b323b326d"
    );
    assert!(
        !host
            .pane
            .as_ref()
            .unwrap()
            .screen
            .as_ref()
            .unwrap()
            .selection_present
    );
    host.shutdown().unwrap();
}

#[test]
fn geometry_change_balances_pointer_release_and_publishes_without_program_output() {
    let mut host = host();
    let lease = attach(&mut host, 1, true);
    command(
        &mut host,
        1,
        lease,
        "set +m; stty raw -echo; printf '\\033[?1000h\\033[?1006h\\033[2J\\033[HREADY\\r\\n'; reports=; while :; do byte=$(dd bs=1 count=1 2>/dev/null | od -An -tx1 | tr -d ' \\n'); [ \"$byte\" = 21 ] && break; reports=$reports$byte; done; printf 'R:%s\\r\\n' \"$reports\"",
    );
    let original = screen_with_marker(&mut host, lease, "READY");
    focus(&mut host, lease, 1, FocusTarget::Terminal);
    let press = Pointer {
        screen_generation: original.generation,
        phase: pane::PointerPhase::Press,
        button: Some(pane::PointerButton::Left),
        x: 4.0,
        y: 5.0,
        modifiers: 0,
        shift_selection: false,
    };
    input(&mut host, lease, 1, InteractiveInput::Pointer(press));
    assert!(matches!(
        host.request(
            [1; 32],
            [1; 32],
            Request::Resize {
                lease,
                size: PaneSize {
                    cell_width: 9,
                    ..size()
                }
            }
        ),
        Reply::Unchanged
    ));
    let resized = await_screen(&mut host, [1; 32], lease, |update| {
        update.generation > original.generation
    });
    assert!(resized.generation > original.generation);
    let geometry = resized.geometry.unwrap();
    assert_eq!((geometry.columns, geometry.rows), (80, 24));
    // A discarded gesture cannot be resumed by its delayed release or a stale press.
    input(
        &mut host,
        lease,
        1,
        InteractiveInput::Pointer(Pointer {
            phase: pane::PointerPhase::Release,
            x: 24.0,
            y: 41.0,
            ..press
        }),
    );
    input(&mut host, lease, 1, InteractiveInput::Pointer(press));
    let fresh = Pointer {
        screen_generation: resized.generation,
        x: 12.0,
        y: 23.0,
        ..press
    };
    for phase in [pane::PointerPhase::Press, pane::PointerPhase::Release] {
        input(
            &mut host,
            lease,
            1,
            InteractiveInput::Pointer(Pointer { phase, ..fresh }),
        );
    }
    input(&mut host, lease, 1, text_key("!", false));
    let screen = screen_with_marker(&mut host, lease, "R:");
    let report = lines(&screen)
        .into_iter()
        .find(|row| row.starts_with("R:"))
        .unwrap();
    assert_eq!(
        report.trim_end(),
        "R:1b5b3c303b313b314d1b5b3c303b313b316d1b5b3c303b323b324d1b5b3c303b323b326d"
    );
    host.shutdown().unwrap();
}

#[test]
fn application_pointer_and_wheel_encode_at_the_pty_and_stale_screens_are_dropped() {
    let mut host = host();
    let lease = attach(&mut host, 1, true);
    capture(&mut host, lease, 28, "\\033[?1000h\\033[?1006h");
    focus(&mut host, lease, 1, FocusTarget::Terminal);
    // Observe again after a deliberate resize to obtain a fresh authoritative mapping.
    host.request(
        [1; 32],
        [1; 32],
        Request::Resize {
            lease,
            size: PaneSize {
                columns: 81,
                ..size()
            },
        },
    );
    let screen = await_screen(&mut host, [1; 32], lease, |update| {
        update
            .geometry
            .is_some_and(|geometry| geometry.columns == 81)
    });
    assert!(screen.modes.unwrap().mouse_tracking);
    let pointer = Pointer {
        screen_generation: screen.generation,
        phase: pane::PointerPhase::Press,
        button: Some(pane::PointerButton::Left),
        x: 4.0,
        y: 5.0,
        modifiers: 0,
        shift_selection: true,
    };
    assert!(matches!(
        input(
            &mut host,
            lease,
            1,
            InteractiveInput::Pointer(Pointer {
                screen_generation: u64::MAX,
                ..pointer
            })
        ),
        Reply::Unchanged
    ));
    assert!(matches!(
        input(&mut host, lease, 1, InteractiveInput::Pointer(pointer)),
        Reply::Unchanged
    ));
    assert!(matches!(
        input(
            &mut host,
            lease,
            1,
            InteractiveInput::Pointer(Pointer {
                screen_generation: screen.generation,
                phase: pane::PointerPhase::Release,
                ..pointer
            })
        ),
        Reply::Unchanged
    ));
    assert!(matches!(
        input(
            &mut host,
            lease,
            1,
            InteractiveInput::Wheel(Wheel {
                screen_generation: screen.generation,
                horizontal_steps: 0,
                vertical_steps: 1,
                phase: pane::WheelPhase::GestureChanged,
                x: 4.0,
                y: 5.0,
                modifiers: 0,
                shift_selection: true
            })
        ),
        Reply::Unchanged
    ));
    await_text(
        &mut host,
        lease,
        "1b5b3c303b313b314d1b5b3c303b313b316d1b5b3c36343b313b314d",
    );
    assert!(
        !host
            .pane
            .as_ref()
            .unwrap()
            .screen
            .as_ref()
            .unwrap()
            .selection_present
    );
    host.shutdown().unwrap();
}

#[test]
fn bracketed_paste_reuses_bounds_normalization_and_control_sanitization() {
    let mut host = host();
    let lease = attach(&mut host, 1, true);
    capture(&mut host, lease, 30, "\\033[?2004h");
    focus(&mut host, lease, 1, FocusTarget::Terminal);
    for (text, expected) in [
        (String::new(), pane::PasteRejection::Empty),
        (
            "x".repeat(1_048_577),
            pane::PasteRejection::TooLarge { limit: 1_048_576 },
        ),
    ] {
        assert!(
            matches!(input(&mut host, lease, 1, InteractiveInput::Paste { text }), Reply::Paste(PasteOutcome::Rejected(reason)) if reason == expected)
        );
    }
    let Reply::Paste(PasteOutcome::ConfirmationRequired(confirmation)) = input(
        &mut host,
        lease,
        1,
        InteractiveInput::Paste {
            text: "x\x1b[201~y".into(),
        },
    ) else {
        panic!("closing fence must require confirmation")
    };
    assert!(confirmation.closing_fence);
    input(
        &mut host,
        lease,
        1,
        InteractiveInput::ResolvePaste {
            id: confirmation.id,
            confirm: false,
        },
    );
    for text in ["a\r\nb", "a\x1bb"] {
        assert!(matches!(
            input(
                &mut host,
                lease,
                1,
                InteractiveInput::Paste { text: text.into() }
            ),
            Reply::Paste(PasteOutcome::Written)
        ));
    }
    await_text(
        &mut host,
        lease,
        "1b5b3230307e610a621b5b3230317e1b5b3230307e6120621b5b3230317e",
    );
    host.shutdown().unwrap();
}

#[test]
fn reconnect_takeover_and_wrong_device_invalidate_paste_and_input_authority() {
    let mut host = host();
    let original = attach(&mut host, 1, true);
    capture(&mut host, original, 2, "");
    focus(&mut host, original, 1, FocusTarget::Terminal);
    let pending = unsafe_paste(&mut host, original, 1);
    host.disconnected(original.connection);
    assert!(matches!(
        input(&mut host, original, 1, text_key("bad", false)),
        Reply::Refused(Refusal::Stale)
    ));
    let resumed = attach(&mut host, 2, false);
    assert_eq!(resumed.session, original.session);
    assert!(matches!(
        input(&mut host, resumed, 1, text_key("bad", false)),
        Reply::Refused(Refusal::Stale)
    ));
    focus(&mut host, resumed, 1, FocusTarget::Terminal);
    assert!(matches!(
        input(
            &mut host,
            resumed,
            1,
            InteractiveInput::ResolvePaste {
                id: pending,
                confirm: true
            }
        ),
        Reply::Paste(PasteOutcome::Stale)
    ));
    unsafe_paste(&mut host, resumed, 1);
    let Reply::Attached { lease: successor } = host.request(
        [2; 32],
        [3; 32],
        Request::TakeOver {
            target: resumed.into(),
            size: size(),
            style: None,
        },
    ) else {
        panic!("takeover failed")
    };
    for request in [
        Request::Focus {
            lease: resumed,
            generation: 2,
            target: FocusTarget::Terminal,
        },
        Request::Input {
            lease: resumed,
            generation: 1,
            input: text_key("bad", false),
        },
        Request::Input {
            lease: resumed,
            generation: 1,
            input: InteractiveInput::ResolvePaste {
                id: pending,
                confirm: true,
            },
        },
    ] {
        assert!(matches!(
            host.request([1; 32], [2; 32], request),
            Reply::Refused(Refusal::Stale)
        ));
    }
    assert!(matches!(
        host.request(
            [1; 32],
            [3; 32],
            Request::Focus {
                lease: successor,
                generation: 1,
                target: FocusTarget::Terminal
            }
        ),
        Reply::Refused(Refusal::Stale)
    ));
    assert!(matches!(
        host.request(
            [2; 32],
            [3; 32],
            Request::Focus {
                lease: successor,
                generation: 1,
                target: FocusTarget::Terminal
            }
        ),
        Reply::Unchanged
    ));
    assert!(matches!(
        host.request(
            [2; 32],
            [3; 32],
            Request::Input {
                lease: successor,
                generation: 1,
                input: InteractiveInput::ResolvePaste {
                    id: pending,
                    confirm: true
                }
            }
        ),
        Reply::Paste(PasteOutcome::Stale)
    ));
    assert!(matches!(
        host.request(
            [2; 32],
            [3; 32],
            Request::Input {
                lease: successor,
                generation: 1,
                input: text_key("ok", false)
            }
        ),
        Reply::Unchanged
    ));
    // The new device has exactly the same running PTY, with no cancelled payload bytes.
    await_screen(&mut host, [2; 32], successor, |update| {
        lines(update).iter().any(|row| row.trim_end() == "6f6b")
    });
    host.shutdown().unwrap();
}

#[test]
fn malformed_interactive_values_never_reach_the_pty() {
    let mut host = host();
    let lease = attach(&mut host, 1, true);
    capture(&mut host, lease, 2, "");
    focus(&mut host, lease, 1, FocusTarget::Terminal);
    let generation = host.pane.as_ref().unwrap().screen_generation;
    for invalid in [
        InteractiveInput::Key(Key {
            physical: u32::MAX,
            action: 0,
            logical: "x".into(),
            text: Some("bad".into()),
            unshifted: None,
            modifiers: 0,
            consumed: 0,
            option_as_alt: false,
        }),
        InteractiveInput::Pointer(Pointer {
            screen_generation: generation,
            phase: pane::PointerPhase::Press,
            button: None,
            x: 1.0,
            y: 1.0,
            modifiers: 0,
            shift_selection: false,
        }),
        InteractiveInput::Pointer(Pointer {
            screen_generation: generation,
            phase: pane::PointerPhase::Press,
            button: Some(pane::PointerButton::Left),
            x: f32::NAN,
            y: 1.0,
            modifiers: 0,
            shift_selection: false,
        }),
        InteractiveInput::Wheel(Wheel {
            screen_generation: generation,
            horizontal_steps: i32::MIN,
            vertical_steps: 0,
            phase: pane::WheelPhase::GestureChanged,
            x: 1.0,
            y: 1.0,
            modifiers: 0,
            shift_selection: false,
        }),
    ] {
        assert!(matches!(
            input(&mut host, lease, 1, invalid),
            Reply::Refused(Refusal::Unsupported)
        ));
    }
    input(&mut host, lease, 1, text_key("ok", false));
    await_text(&mut host, lease, "6f6b");
    host.shutdown().unwrap();
}

#[test]
fn application_input_never_starts_selection_or_scrolls_server_history() {
    for modes in ["", "\\033[?1049h\\033[?1000h\\033[?1007h"] {
        let mut host = host();
        let lease = attach(&mut host, 1, true);
        capture(&mut host, lease, 2, modes);
        focus(&mut host, lease, 1, FocusTarget::Terminal);
        let generation = host.pane.as_ref().unwrap().screen_generation;
        let pointer = Pointer {
            screen_generation: generation,
            phase: pane::PointerPhase::Press,
            button: Some(pane::PointerButton::Left),
            x: 4.0,
            y: 5.0,
            modifiers: 1,
            shift_selection: true,
        };
        input(&mut host, lease, 1, InteractiveInput::Pointer(pointer));
        input(
            &mut host,
            lease,
            1,
            InteractiveInput::Pointer(Pointer {
                phase: pane::PointerPhase::Release,
                ..pointer
            }),
        );
        input(
            &mut host,
            lease,
            1,
            InteractiveInput::Wheel(Wheel {
                screen_generation: generation,
                horizontal_steps: 0,
                vertical_steps: 3,
                phase: pane::WheelPhase::GestureChanged,
                x: 4.0,
                y: 5.0,
                modifiers: 1,
                shift_selection: true,
            }),
        );
        input(&mut host, lease, 1, text_key("ok", false));
        await_text(&mut host, lease, "6f6b");
        let screen = host.pane.as_ref().unwrap().screen.as_ref().unwrap();
        assert!(!screen.selection_present);
        assert_eq!(
            screen.viewport.offset_rows + screen.viewport.visible_rows,
            screen.scrollbar.total_rows
        );
        host.shutdown().unwrap();
    }
}

#[test]
fn shared_interactive_input_uses_controlling_size_and_terminal_visible_appearance() {
    let mut host = SharedWorkspaceHost::new(host());
    create_shared(&mut host, 1);
    let pane = shared_inventory(&mut host).workspaces[0].tabs[0].root_pane;
    let Reply::Attached { lease } = host.request(
        [1; 32],
        [1; 32],
        Request::SelectPane {
            pane,
            explicit: true,
            size: size(),
            style: None,
        },
    ) else {
        panic!("shared Pane missing")
    };
    let style = PresentationStyle {
        light: true,
        foreground: [20; 3],
        background: [240; 3],
        palette: [[0; 3]; 16],
        cursor: [20; 3],
        cursor_text: None,
        bold_as_bright: false,
    };
    assert!(matches!(
        host.request(
            [2; 32],
            [2; 32],
            Request::Style {
                lease,
                style: style.clone()
            }
        ),
        Reply::Refused(Refusal::Stale)
    ));
    assert!(matches!(
        host.request([1; 32], [1; 32], Request::Style { lease, style }),
        Reply::Unchanged
    ));
    host.request(
        [1; 32],
        [1; 32],
        Request::Resize {
            lease,
            size: PaneSize {
                columns: 91,
                rows: 31,
                cell_width: 9,
                cell_height: 19,
            },
        },
    );
    focus(&mut host, lease, 1, FocusTarget::Terminal);
    input(
        &mut host,
        lease,
        1,
        text_key(
            // Keep terminal-mode setup, readiness, query and read in one process. Starting
            // another foreground reader after the query can reset or flush its queued reply.
            r#"python3 -c 'import os,sys,tty,fcntl,struct,termios; rows,cols,_,_=struct.unpack("HHHH",fcntl.ioctl(0,termios.TIOCGWINSZ,b"\0"*8)); tty.setraw(0,when=termios.TCSANOW); os.write(1,b"\033[2J\033[HSIZE:%d %d\r\nQUERY-READY\r\n"%(rows,cols)); sys.stdin.buffer.read(1); os.write(1,b"\033[?996n"); reply=sys.stdin.buffer.read(9); os.write(1,b"APPEARANCE:"+reply.hex().encode()+b"\r\n")'"#,
            false,
        ),
    );
    input(
        &mut host,
        lease,
        1,
        InteractiveInput::Key(Key {
            physical: PhysicalKey::Enter as u32,
            action: 0,
            logical: "enter".into(),
            text: None,
            unshifted: None,
            modifiers: 0,
            consumed: 0,
            option_as_alt: false,
        }),
    );
    await_text(&mut host, lease, "QUERY-READY");
    input(&mut host, lease, 1, text_key(" ", false));
    let screen = await_screen(&mut host, [1; 32], lease, |update| {
        lines(update)
            .iter()
            .any(|row| row.trim_end() == "APPEARANCE:1b5b3f3939373b326e")
    });
    let geometry = screen.geometry.unwrap();
    assert_eq!((geometry.columns, geometry.rows), (91, 31));
    assert_eq!(screen.colors.as_ref().unwrap().background, [240; 3]);
    assert!(
        lines(&screen)
            .iter()
            .any(|row| row.trim_end() == "SIZE:31 91")
    );
    host.shutdown().unwrap();
}

fn select(host: &mut DevelopmentPaneHost, device: u8, connection: u8) -> Reply {
    host.request(
        [device; 32],
        [connection; 32],
        Request::Select {
            create: true,
            size: size(),
            resume: None,
            style: None,
        },
    )
}

#[test]
fn a_restored_pane_waits_for_explicit_first_use_and_starts_only_once() {
    let mut host = host();
    host.organization = Some(Organization::first(host.directory.clone()).unwrap());
    let pane_id = host.organization.as_ref().unwrap().pane;
    for resume in [
        None,
        Some(PaneTarget {
            server: [8; 32],
            session: [9; 32],
        }),
    ] {
        assert!(matches!(host.request([1; 32], [1; 32], Request::Select {
            create: false, size: size(), resume, style: None,
        }), Reply::Restored { pane, .. } if pane == pane_id));
        host.tick(std::time::Instant::now());
        assert!(host.pane.is_none());
    }
    let request = Request::StartRestored {
        server: host.boot,
        pane: pane_id,
        size: size(),
        style: None,
    };
    let Reply::Attached { lease } = host.request([1; 32], [1; 32], request.clone()) else {
        panic!("first use");
    };
    assert!(
        matches!(host.request([2; 32], [2; 32], request), Reply::Takeover { target, .. } if target == lease.into())
    );
    assert_eq!(host.organization.as_ref().unwrap().pane, pane_id);
    host.shutdown().unwrap();
}

#[test]
fn a_missing_confirmed_directory_refuses_startup_without_falling_back() {
    let mut host = host();
    let mut organization = Organization::first(host.directory.clone()).unwrap();
    organization.confirmed_directory = Some(host.directory.join(format!(
        "missing-{:x}",
        u128::from_be_bytes(nonce().unwrap()[..16].try_into().unwrap())
    )));
    host.organization = Some(organization.clone());
    assert!(matches!(
        host.request(
            [1; 32],
            [1; 32],
            Request::StartRestored {
                server: host.boot,
                pane: organization.pane,
                size: size(),
                style: None,
            }
        ),
        Reply::Refused(Refusal::DirectoryUnavailable)
    ));
    assert!(host.pane.is_none());
    assert!(host.organization.as_ref() == Some(&organization));
}

#[test]
fn ordinary_exit_retires_organization_without_a_connected_client() {
    let mut host = host();
    let lease = attach(&mut host, 1, true);
    command(&mut host, 1, lease, "exit 0");
    host.disconnected([1; 32]);
    let until = Instant::now() + Duration::from_secs(3);
    while host.organization.is_some() && Instant::now() < until {
        host.tick(std::time::Instant::now());
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(
        host.organization.is_none(),
        "ordinary exit closes the final Pane even without a client polling"
    );
}

#[test]
fn only_owning_shell_directory_confirmation_changes_restoration() {
    let mut host = host();
    let lease = attach(&mut host, 1, true);
    command(
        &mut host,
        1,
        lease,
        "stty -echo; printf '\\033]7;file://localhost/\\007\\033]133;A;spaceterm=%s\\007' \"$SPACETERM_PROMPT_OWNER\"; printf '\\033[2J\\033[HCONFIRMED\\n'",
    );
    await_text(&mut host, lease, "CONFIRMED");
    host.tick(std::time::Instant::now());
    assert_eq!(
        host.organization
            .as_ref()
            .unwrap()
            .confirmed_directory
            .as_deref(),
        Some(std::path::Path::new("/"))
    );
    command(
        &mut host,
        1,
        lease,
        "printf '\\033]7;file://localhost/tmp\\007\\033]133;A;spaceterm=foreign\\007'; printf 'NESTED\\n'",
    );
    await_text(&mut host, lease, "NESTED");
    host.tick(std::time::Instant::now());
    assert_eq!(
        host.organization
            .as_ref()
            .unwrap()
            .confirmed_directory
            .as_deref(),
        Some(std::path::Path::new("/"))
    );
    host.shutdown().unwrap();
}

#[test]
fn takeover_preserves_work_and_rejects_former_controller_operations() {
    let mut host = host();
    let original = attach(&mut host, 2, true);
    command(
        &mut host,
        2,
        original,
        "stty -echo; printf '\\033[2J\\033[HSTARTED\\n'; sleep 1; printf 'CONTINUED\\n'\n",
    );
    await_text(&mut host, original, "STARTED");
    host.disconnected([2; 32]);
    let Reply::Takeover { target, .. } = select(&mut host, 9, 9) else {
        panic!("selection must not replace a disconnected controller");
    };
    let Reply::Attached { lease: transferred } = host.request(
        [9; 32],
        [9; 32],
        Request::TakeOver {
            target,
            size: PaneSize {
                columns: 90,
                ..size()
            },
            style: None,
        },
    ) else {
        panic!("explicit takeover must transfer control");
    };
    assert_eq!(original.session, transferred.session);
    assert_ne!(original.control, transferred.control);
    assert!(matches!(
        screen(&mut host, [1; 32], original),
        Err(PaneEvent::Takeover(_))
    ));
    for request in [
        Request::Resize {
            lease: original,
            size: size(),
        },
        Request::Close { lease: original },
        Request::Release { lease: original },
    ] {
        assert!(matches!(
            host.request([1; 32], [2; 32], request),
            Reply::Refused(Refusal::Stale)
        ));
    }
    assert!(matches!(
        command(&mut host, 2, original, "printf 'STALE'\n"),
        Reply::Refused(Refusal::Stale)
    ));
    assert!(matches!(
        host.request(
            [1; 32],
            [3; 32],
            Request::Select {
                create: false,
                size: size(),
                resume: Some(original.into()),
                style: None,
            }
        ),
        Reply::Takeover { .. }
    ));
    // Take it back explicitly, keeping the running command and the same Session.
    let Reply::Attached { lease: returned } = host.request(
        [1; 32],
        [3; 32],
        Request::TakeOver {
            target,
            size: size(),
            style: None,
        },
    ) else {
        panic!("take back");
    };
    assert_eq!(original.session, returned.session);
    await_text(&mut host, returned, "CONTINUED");
    host.shutdown().unwrap();
}

#[test]
fn detached_attachment_fences_lost_claim_replies_and_late_release_preserves_reattachment() {
    use spaceterm_server::{attachments::Attachment, hierarchy::Mutation};
    let mut host = SharedWorkspaceHost::new(host());
    host.request(
        [1; 32],
        [1; 32],
        Request::Mutate {
            sequence: 1,
            mutation: Mutation::CreateWorkspace {
                name: "work".into(),
                directory: None,
            },
        },
    );
    let inventory = shared_inventory(&mut host);
    let workspace = inventory.workspaces[0].id;
    let pane = inventory.workspaces[0].tabs[0].root_pane;
    let original = Attachment {
        workspace,
        generation: 1,
    };
    let claim = |attachment, explicit| Request::ClaimPane {
        attachment,
        pane,
        claim: AttachmentClaim::Select { explicit },
        size: size(),
        style: None,
    };
    let Reply::Attached { lease: first } = host.request([1; 32], [2; 32], claim(original, true))
    else {
        panic!("claim");
    };
    host.request(
        [1; 32],
        [1; 32],
        Request::Mutate {
            sequence: 2,
            mutation: Mutation::SplitPane {
                workspace,
                tab: inventory.workspaces[0].root_tab,
                source: pane,
                axis: spaceterm_server::hierarchy::Axis::Right,
            },
        },
    );
    let split = shared_inventory(&mut host).workspaces[0].tabs[0].panes[1].id;
    let Reply::Attached { lease: split_lease } = host.request(
        [1; 32],
        [6; 32],
        Request::ClaimPane {
            attachment: original,
            pane: split,
            claim: AttachmentClaim::Select { explicit: true },
            size: size(),
            style: None,
        },
    ) else {
        panic!("second Pane claim");
    };
    // The acquisition committed, but the desktop lost its reply before offline detach.
    host.disconnected([2; 32]);
    host.disconnected([6; 32]);
    assert!(matches!(
        host.request(
            [1; 32],
            [3; 32],
            Request::ReleaseAttachment {
                attachment: original
            }
        ),
        Reply::Unchanged
    ));
    assert!(matches!(
        host.request([1; 32], [2; 32], claim(original, true)),
        Reply::Refused(Refusal::Stale)
    ));
    assert!(
        matches!(
            screen(&mut host, [1; 32], split_lease),
            Err(PaneEvent::Takeover(_))
        ),
        "detach releases every original Pane grant"
    );
    let later = Attachment {
        workspace,
        generation: 2,
    };
    assert!(matches!(
        host.request([1; 32], [4; 32], claim(later, false)),
        Reply::Takeover { .. }
    ));
    let Reply::Attached { lease: current } = host.request([1; 32], [4; 32], claim(later, true))
    else {
        panic!("reattach selects without resuming old control");
    };
    assert_eq!(current.session, first.session);
    assert_ne!(current.control, first.control);
    // Lost release acknowledgement is safely retried after a later grant exists.
    assert!(matches!(
        host.request(
            [1; 32],
            [5; 32],
            Request::ReleaseAttachment {
                attachment: original
            }
        ),
        Reply::Unchanged
    ));
    assert!(screen(&mut host, [1; 32], current).is_ok());
    let Reply::Attached { lease: other } = host.request(
        [9; 32],
        [9; 32],
        Request::TakeOver {
            target: current.into(),
            size: size(),
            style: None,
        },
    ) else {
        panic!("takeover");
    };
    host.request(
        [1; 32],
        [5; 32],
        Request::ReleaseAttachment { attachment: later },
    );
    assert!(screen(&mut host, [9; 32], other).is_ok());
    assert_eq!(shared_inventory(&mut host).workspaces.len(), 1);
    host.shutdown().unwrap();
}

#[test]
fn revoking_a_device_releases_control_without_ending_its_terminal_session() {
    let mut host = host();
    host.admitted([1; 32], "Revoked desktop".into());
    let original = attach(&mut host, 2, true);
    host.revoked([1; 32]);
    assert!(matches!(
        screen(&mut host, [1; 32], original),
        Err(PaneEvent::Takeover(_))
    ));
    let Reply::Attached { lease: later } = select(&mut host, 9, 9) else {
        panic!("revocation must leave the Pane unclaimed");
    };
    assert_eq!(later.session, original.session);
    assert_ne!(later.control, original.control);
    host.revoked([1; 32]);
    assert!(screen(&mut host, [9; 32], later).is_ok());
    host.shutdown().unwrap();
}

#[test]
fn released_control_has_one_selection_winner_and_never_resumes_old_grants() {
    let mut host = host();
    let original = attach(&mut host, 2, true);
    assert!(matches!(
        host.request([1; 32], [2; 32], Request::Release { lease: original }),
        Reply::Unchanged
    ));
    assert!(matches!(
        host.request(
            [1; 32],
            [3; 32],
            Request::Select {
                create: false,
                size: size(),
                resume: Some(original.into()),
                style: None,
            }
        ),
        Reply::Takeover { .. }
    ));
    let host = Arc::new(std::sync::Mutex::new(host));
    let start = Arc::new(std::sync::Barrier::new(3));
    let workers = [(9, 9), (8, 8)].map(|(device, connection)| {
        let host = Arc::clone(&host);
        let start = Arc::clone(&start);
        std::thread::spawn(move || {
            start.wait();
            select(&mut host.lock().unwrap(), device, connection)
        })
    });
    start.wait();
    let replies = workers.map(|worker| worker.join().unwrap());
    assert_eq!(
        replies
            .iter()
            .filter(|reply| matches!(reply, Reply::Attached { .. }))
            .count(),
        1
    );
    assert_eq!(
        replies
            .iter()
            .filter(|reply| matches!(reply, Reply::Takeover { .. }))
            .count(),
        1
    );
    let mut host = host.lock().unwrap();
    assert!(matches!(select(&mut host, 1, 3), Reply::Takeover { .. }));
    assert!(matches!(
        screen(&mut *host, [1; 32], original),
        Err(PaneEvent::Takeover(_))
    ));
    host.shutdown().unwrap();
}

#[test]
fn reconnect_keeps_running_work_and_fences_the_old_connection() {
    let mut host = host();
    let first = attach(&mut host, 2, true);
    command(
        &mut host,
        2,
        first,
        "stty -echo; printf '\\033[2J\\033[HSTARTED\\n'; sleep 1; printf 'CONTINUED\\n'\n",
    );
    await_text(&mut host, first, "STARTED");
    host.disconnected([2; 32]);
    std::thread::sleep(Duration::from_millis(1100));
    let second = attach(&mut host, 3, false);
    assert_eq!(first.session, second.session);
    assert!(matches!(
        command(&mut host, 2, first, "printf 'STALE'\n"),
        Reply::Refused(Refusal::Stale)
    ));
    assert!(matches!(
        host.request(
            [9; 32],
            [9; 32],
            Request::Attach {
                create: false,
                size: size(),
                resume: None,
                style: None
            }
        ),
        Reply::Refused(Refusal::Controlled)
    ));
    await_text(&mut host, second, "CONTINUED");
    host.shutdown().unwrap();
}

#[test]
fn reconnect_and_stale_generations_cannot_start_or_mutate_a_replacement() {
    let mut host = host();
    assert!(matches!(
        host.request(
            [1; 32],
            [2; 32],
            Request::Attach {
                create: false,
                size: size(),
                resume: None,
                style: None
            }
        ),
        Reply::Refused(Refusal::Missing)
    ));
    let old = attach(&mut host, 2, true);
    assert!(matches!(
        host.request([1; 32], [2; 32], Request::Close { lease: old }),
        Reply::Closed
    ));
    assert!(matches!(
        host.request(
            [1; 32],
            [2; 32],
            Request::Attach {
                create: false,
                size: size(),
                resume: None,
                style: None
            }
        ),
        Reply::Refused(Refusal::Missing)
    ));
    let live = attach(&mut host, 2, true);
    assert!(matches!(
        host.request(
            [1; 32],
            [2; 32],
            Request::Attach {
                create: false,
                size: PaneSize {
                    columns: 90,
                    ..size()
                },
                style: None,
                resume: Some(old)
            }
        ),
        Reply::Refused(Refusal::Stale)
    ));
    for stale in [
        Lease {
            server: old.server.map(|b| b ^ 1),
            ..live
        },
        Lease {
            session: old.session,
            ..live
        },
        Lease {
            control: old.control,
            ..live
        },
    ] {
        assert!(matches!(
            host.request([1; 32], [2; 32], Request::Close { lease: stale }),
            Reply::Refused(Refusal::Stale)
        ));
        assert!(matches!(
            host.request(
                [1; 32],
                [2; 32],
                Request::Resize {
                    lease: stale,
                    size: PaneSize {
                        columns: 90,
                        ..size()
                    }
                }
            ),
            Reply::Refused(Refusal::Stale)
        ));
    }
    command(
        &mut host,
        2,
        live,
        "stty -echo; printf '\\033[2J\\033[HLIVE\\n'; stty size\n",
    );
    await_text(&mut host, live, "24 80");
    host.shutdown().unwrap();
}

#[test]
fn a_waiting_client_observes_exit_without_the_controller_polling() {
    let mut host = host();
    let lease = attach(&mut host, 2, true);
    command(
        &mut host,
        2,
        lease,
        "stty -echo; printf '\\033[2J\\033[HEXITING\\n'; sleep 0.2; exit\n",
    );
    await_text(&mut host, lease, "EXITING");
    host.disconnected([2; 32]);
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let reply = host.request(
            [9; 32],
            [9; 32],
            Request::Select {
                create: false,
                size: size(),
                resume: Some(lease.into()),
                style: None,
            },
        );
        if matches!(reply, Reply::Refused(Refusal::Missing)) {
            break;
        }
        assert!(matches!(reply, Reply::Takeover { .. }));
        assert!(
            Instant::now() < deadline,
            "an exited shell still offers takeover"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(matches!(
        host.request(
            [9; 32],
            [9; 32],
            Request::TakeOver {
                target: lease.into(),
                size: size(),
                style: None,
            }
        ),
        Reply::Refused(Refusal::Missing)
    ));
    let replacement = attach(&mut host, 3, true);
    assert_ne!(replacement.session, lease.session);
    host.shutdown().unwrap();
}

#[test]
fn ordinary_exit_allows_a_later_explicit_launch_but_never_a_reconnect() {
    let mut host = host();
    let old = attach(&mut host, 2, true);
    command(&mut host, 2, old, "exit 0\n");
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if matches!(screen(&mut host, [1; 32], old), Err(PaneEvent::Ended)) {
            break;
        }
        assert!(Instant::now() < deadline, "shell exit did not arrive");
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(matches!(
        host.request(
            [1; 32],
            [3; 32],
            Request::Attach {
                create: false,
                size: size(),
                resume: Some(old),
                style: None,
            }
        ),
        Reply::Refused(Refusal::Missing)
    ));
    let fresh = attach(&mut host, 3, true);
    assert_ne!(fresh.session, old.session);
    command(
        &mut host,
        3,
        fresh,
        "stty -echo; printf '\\033[2J\\033[HNEW-SHELL\\n'\n",
    );
    await_text(&mut host, fresh, "NEW-SHELL");
    host.shutdown().unwrap();
}
