#![cfg(feature = "runtime")]
mod support;
use spaceterm_core::{
    native_requests::*,
    observations::*,
    organization::{FailureClass, RetirementReason},
};
use spaceterm_terminal::runtime::{budget::RuntimeLimits, owner::RuntimeFactory};
use spaceterm_terminal::runtime_contracts::*;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use support::*;

#[test]
fn fragmented_osc52_keeps_original_authority() {
    let factory = factory(RuntimeLimits::default());
    let request = request(1, grid(24, 80));
    let pty = Pty::default();
    let mut owner = running(&factory, request.clone(), &pty);
    let mut native = owner.take_native_requests().unwrap();
    pty.output(b"prefix\x1b]52;c;");
    pty.until(|state| state.engine_bytes == b"prefix");
    owner
        .transition_authority(takeover(request.authority()))
        .unwrap()
        .receive_blocking()
        .unwrap();
    pty.output(b"?\x07");
    pty.until(|state| state.written == b"\x1b]52;c;\x07");
    assert!(native.try_next().unwrap().is_none());
    owner.retire(RetirementReason::Stop);
    owner.join().unwrap();
}
#[test]
fn no_native_route_denies_clipboard_read_without_stalling_output() {
    let factory = factory(RuntimeLimits::default());
    let request = request(1, grid(24, 80));
    let pty = Pty::default();
    let mut owner = running(&factory, request, &pty);
    pty.output(b"\x1b]52;c;?\x07query");
    pty.until(|state| state.written.ends_with(b"reply"));
    assert_eq!(
        pty.inspect(|state| state.written.clone()),
        b"\x1b]52;c;\x07reply"
    );
    owner.retire(RetirementReason::Stop);
    owner.join().unwrap();
}
#[test]
fn native_clipboard_reply_is_ordered_before_later_input() {
    let factory = factory(RuntimeLimits::default());
    let request = request(1, grid(24, 80));
    let pty = Pty::default();
    let mut owner = running(&factory, request.clone(), &pty);
    let mut native = owner.take_native_requests().unwrap();
    pty.output(b"\x1b]52;c;?\x07");
    let read = native.next_blocking().unwrap().unwrap();
    let input = owner
        .handle()
        .try_submit(fence(request.authority()), ime("later"))
        .unwrap();
    let completion = NativeCompletion::new(
        &read,
        NativeCompletionKind::ClipboardText(NativeText::new("clipboard".into()).unwrap()),
    )
    .unwrap();
    owner
        .handle()
        .try_submit(
            read.fence(),
            RuntimeCommand::CompleteNativeRequest(completion),
        )
        .unwrap()
        .receive_blocking()
        .unwrap();
    input.receive_blocking().unwrap();
    assert_eq!(
        pty.inspect(|state| state.written.clone()),
        b"\x1b]52;c;Y2xpcGJvYXJk\x07later"
    );
    owner.retire(RetirementReason::Stop);
    owner.join().unwrap();
}
#[test]
fn clipboard_deadline_discards_late_native_text() {
    let clock = Arc::new(Clock::default());
    let factory = RuntimeFactory::new(RuntimeLimits::default(), clock.clone()).unwrap();
    let request = request(1, grid(24, 80));
    let pty = Pty::default();
    let mut owner = running(&factory, request, &pty);
    let mut native = owner.take_native_requests().unwrap();
    pty.output(b"\x1b]52;c;?\x07");
    let read = native.next_blocking().unwrap().unwrap();
    clock.0.store(1000, Ordering::Release);
    let completion = NativeCompletion::new(
        &read,
        NativeCompletionKind::ClipboardText(NativeText::new("must-not-escape".into()).unwrap()),
    )
    .unwrap();
    assert_eq!(
        owner
            .handle()
            .try_submit(
                read.fence(),
                RuntimeCommand::CompleteNativeRequest(completion)
            )
            .unwrap()
            .receive_blocking()
            .unwrap(),
        EffectDisposition::StaleAuthority
    );
    pty.until(|state| state.written == b"\x1b]52;c;\x07");
    owner.retire(RetirementReason::Stop);
    owner.join().unwrap();
}
#[test]
fn takeover_denies_pending_clipboard_and_preserves_native_route() {
    let factory = factory(RuntimeLimits::default());
    let request = request(1, grid(24, 80));
    let pty = Pty::default();
    let mut owner = running(&factory, request.clone(), &pty);
    let mut native = owner.take_native_requests().unwrap();
    pty.output(b"\x1b]52;c;?\x07");
    let old = native.next_blocking().unwrap().unwrap();
    let transition = takeover(request.authority());
    owner
        .transition_authority(transition)
        .unwrap()
        .receive_blocking()
        .unwrap();
    let completion = NativeCompletion::new(
        &old,
        NativeCompletionKind::ClipboardText(
            NativeText::new("old-controller-secret".into()).unwrap(),
        ),
    )
    .unwrap();
    assert!(
        owner
            .handle()
            .try_submit(
                old.fence(),
                RuntimeCommand::CompleteNativeRequest(completion)
            )
            .is_err()
    );
    pty.until(|state| state.written == b"\x1b]52;c;\x07");
    pty.output(b"\x1b]52;c;?\x07");
    let fresh = native.next_blocking().unwrap().unwrap();
    assert_eq!(fresh.fence(), fence(transition.next()));
    owner
        .handle()
        .try_submit(
            fresh.fence(),
            RuntimeCommand::CompleteNativeRequest(
                NativeCompletion::new(&fresh, NativeCompletionKind::Denied).unwrap(),
            ),
        )
        .unwrap()
        .receive_blocking()
        .unwrap();
    owner.retire(RetirementReason::Stop);
    owner.join().unwrap();
}
#[test]
fn hidden_input_denies_clipboard_read() {
    let factory = factory(RuntimeLimits::default());
    let request = request(1, grid(24, 80));
    let pty = Pty::default();
    pty.update(|state| state.hidden = true);
    let mut owner = running(&factory, request, &pty);
    let mut native = owner.take_native_requests().unwrap();
    pty.output(b"\x1b]52;c;?\x07");
    pty.until(|state| state.written == b"\x1b]52;c;\x07");
    assert!(native.try_next().unwrap().is_none());
    owner.retire(RetirementReason::Stop);
    owner.join().unwrap();
}
#[test]
fn wrong_paste_approval_does_not_consume_current_sanitized_payload() {
    let factory = factory(RuntimeLimits::default());
    let request = request(1, grid(24, 80));
    let pty = Pty::default();
    let mut owner = running(&factory, request.clone(), &pty);
    let mut native = owner.take_native_requests().unwrap();
    owner
        .handle()
        .try_submit(
            fence(request.authority()),
            RuntimeCommand::RequestPaste(NativeText::new("one\r\ntwo\x1b[201~".into()).unwrap()),
        )
        .unwrap()
        .receive_blocking()
        .unwrap();
    let request = native.next_blocking().unwrap().unwrap();
    let NativeRequestKind::UnsafePaste { approval, text } = request.kind() else {
        panic!("expected paste approval")
    };
    let approval = *approval;
    assert_eq!(text.as_str(), "one\ntwo[201~");
    assert_eq!(
        owner
            .handle()
            .try_submit(
                request.fence(),
                RuntimeCommand::ResolvePaste(
                    PasteApprovalId::new(approval.get() + 1).unwrap(),
                    PasteDecision::Confirm
                )
            )
            .unwrap()
            .receive_blocking()
            .unwrap(),
        EffectDisposition::StaleAuthority
    );
    owner
        .handle()
        .try_submit(
            request.fence(),
            RuntimeCommand::ResolvePaste(approval, PasteDecision::Confirm),
        )
        .unwrap()
        .receive_blocking()
        .unwrap();
    assert_eq!(pty.inspect(|state| state.written.clone()), b"one\ntwo[201~");
    owner.retire(RetirementReason::Stop);
    owner.join().unwrap();
}
#[test]
fn unsafe_paste_expires_at_thirty_seconds_and_retirement_clears_approval() {
    let clock = Arc::new(Clock::default());
    let factory = RuntimeFactory::new(RuntimeLimits::default(), clock.clone()).unwrap();
    let request = request(1, grid(24, 80));
    let pty = Pty::default();
    let mut owner = running(&factory, request.clone(), &pty);
    let mut native = owner.take_native_requests().unwrap();
    owner
        .handle()
        .try_submit(
            fence(request.authority()),
            RuntimeCommand::RequestPaste(NativeText::new("one\ntwo".into()).unwrap()),
        )
        .unwrap()
        .receive_blocking()
        .unwrap();
    let pending = native.next_blocking().unwrap().unwrap();
    let NativeRequestKind::UnsafePaste { approval, .. } = pending.kind() else {
        panic!("expected paste approval")
    };
    clock.0.store(30_000, Ordering::Release);
    assert_eq!(
        owner
            .handle()
            .try_submit(
                pending.fence(),
                RuntimeCommand::ResolvePaste(*approval, PasteDecision::Confirm)
            )
            .unwrap()
            .receive_blocking()
            .unwrap(),
        EffectDisposition::StaleAuthority
    );
    owner.retire(RetirementReason::EndAllSessions);
    owner.join().unwrap();
    assert!(native.next_blocking().unwrap().is_none());
    assert!(pty.inspect(|state| state.written.is_empty()));
}
#[test]
fn permission_requests_are_bounded_delayed_and_do_not_steal_focus() {
    let clock = Arc::new(Clock::default());
    let factory = RuntimeFactory::new(RuntimeLimits::default(), clock.clone()).unwrap();
    let request = request(1, grid(24, 80));
    let pty = Pty::default();
    let mut owner = running(&factory, request.clone(), &pty);
    let mut native = owner.take_native_requests().unwrap();
    pty.output(
        b"\x1b]7701;permissions=accessibility\x1b\\\x1b]7701;permissions=accessibility\x1b\\",
    );
    let notice = native.next_blocking().unwrap().unwrap();
    assert_eq!(notice.not_before(), MonotonicTick::new(500));
    assert!(native.try_next().unwrap().is_none());
    let completion =
        NativeCompletion::new(&notice, NativeCompletionKind::PermissionAcknowledged).unwrap();
    assert_eq!(
        owner
            .handle()
            .try_submit(
                notice.fence(),
                RuntimeCommand::CompleteNativeRequest(completion.clone())
            )
            .unwrap()
            .receive_blocking()
            .unwrap(),
        EffectDisposition::StaleAuthority
    );
    clock.0.store(500, Ordering::Release);
    owner
        .handle()
        .try_submit(
            notice.fence(),
            RuntimeCommand::CompleteNativeRequest(completion),
        )
        .unwrap()
        .receive_blocking()
        .unwrap();
    owner
        .handle()
        .try_submit(fence(request.authority()), ime("focused"))
        .unwrap()
        .receive_blocking()
        .unwrap();
    assert_eq!(pty.inspect(|state| state.written.clone()), b"focused");
    owner.retire(RetirementReason::Stop);
    owner.join().unwrap();
}
#[test]
fn interrupted_permission_requests_preserve_following_terminal_output() {
    for oversized in [false, true] {
        for split_escape in [false, true] {
            let factory = factory(RuntimeLimits::default());
            let request = request(1, grid(24, 80));
            let pty = Pty::default();
            let mut owner = running(&factory, request, &pty);
            let mut native = owner.take_native_requests().unwrap();
            let mut notice = b"before\x1b]7701;permissions=accessibility".to_vec();
            if oversized {
                notice.extend(std::iter::repeat_n(b'a', 512));
            }
            if split_escape {
                pty.output_chunks(&[&notice, b"\x1b", b"[32mprompt$ ls\r\n"]);
            } else {
                pty.output_chunks(&[&notice, b"\x1b[32mprompt$ ls\r\n"]);
            }
            pty.until(|state| state.engine_bytes.ends_with(b"prompt$ ls\r\n"));
            assert_eq!(
                pty.inspect(|state| state.engine_bytes.clone()),
                b"before\x1b[32mprompt$ ls\r\n"
            );
            assert!(native.try_next().unwrap().is_none());
            owner.retire(RetirementReason::Stop);
            owner.join().unwrap();
        }
    }
}

#[test]
fn interrupted_permission_request_keeps_the_restarting_escape_authority() {
    let factory = factory(RuntimeLimits::default());
    let request = request(1, grid(24, 80));
    let pty = Pty::default();
    let mut owner = running(&factory, request.clone(), &pty);
    let mut native = owner.take_native_requests().unwrap();
    pty.output_chunks(&[b"prefix\x1b]7701;permissions=acc\x1b"]);
    pty.until(|state| {
        state.reader_waiting && state.output_chunks.is_empty() && state.engine_bytes == b"prefix"
    });
    owner
        .transition_authority(takeover(request.authority()))
        .unwrap()
        .receive_blocking()
        .unwrap();
    pty.output(b"]52;c;?\x07");
    pty.until(|state| state.written == b"\x1b]52;c;\x07");
    assert!(native.try_next().unwrap().is_none());
    owner.retire(RetirementReason::Stop);
    owner.join().unwrap();
}

#[test]
fn native_debug_and_error_metadata_exclude_sensitive_sentinels() {
    let factory = factory(RuntimeLimits::default());
    let request = request(1, grid(24, 80));
    let pty = Pty::default();
    let mut owner = running(&factory, request.clone(), &pty);
    let mut native = owner.take_native_requests().unwrap();
    owner
        .handle()
        .try_submit(
            fence(request.authority()),
            RuntimeCommand::RequestPaste(
                NativeText::new("sensitive-sentinel\nclipboard-path-secret".into()).unwrap(),
            ),
        )
        .unwrap()
        .receive_blocking()
        .unwrap();
    let pending = native.next_blocking().unwrap().unwrap();
    let debug = format!("{pending:?}");
    assert!(!debug.contains("sensitive-sentinel"));
    assert!(!debug.contains("clipboard-path-secret"));
    let error = owner
        .handle()
        .try_submit(
            fence(takeover(request.authority()).next()),
            ime("sensitive-sentinel"),
        )
        .err()
        .unwrap();
    assert_eq!(error.class, FailureClass::StaleAuthority);
    assert!(!format!("{error:?} {error}").contains("sensitive-sentinel"));
    owner.retire(RetirementReason::Stop);
    owner.join().unwrap();
}

#[test]
fn clipboard_reply_rechecks_deadline_after_would_block_before_final_write() {
    let clock = Arc::new(Clock::default());
    let factory = RuntimeFactory::new(RuntimeLimits::default(), clock.clone()).unwrap();
    let request = request(1, grid(24, 80));
    let pty = Pty::default();
    let mut owner = running(&factory, request.clone(), &pty);
    let mut native = owner.take_native_requests().unwrap();
    pty.output(b"\x1b]52;c;?\x07");
    let read = native.next_blocking().unwrap().unwrap();
    pty.update(|state| state.blocked = true);
    let completion = NativeCompletion::new(
        &read,
        NativeCompletionKind::ClipboardText(NativeText::new("late-secret".into()).unwrap()),
    )
    .unwrap();
    let reply = owner
        .handle()
        .try_submit(
            read.fence(),
            RuntimeCommand::CompleteNativeRequest(completion),
        )
        .unwrap();
    pty.until(|state| !state.attempted_write.is_empty());
    clock.0.store(1000, Ordering::Release);
    // Keep the PTY blocked until an actual retry has selected the empty reply.
    // A command acknowledgement is not a barrier for an in-flight write guard.
    pty.until(|state| state.attempted_write == b"\x1b]52;c;\x07");
    pty.update(|state| state.blocked = false);
    let result = reply.receive_blocking().unwrap();
    owner.retire(RetirementReason::Stop);
    owner.join().unwrap();
    assert_eq!(result, EffectDisposition::StaleAuthority);
    assert_eq!(
        pty.inspect(|state| state.written.clone()),
        b"\x1b]52;c;\x07"
    );
}

#[test]
fn unsupported_clipboard_targets_are_denied_not_redirected_to_standard_clipboard() {
    let factory = factory(RuntimeLimits::default());
    let request = request(1, grid(24, 80));
    let pty = Pty::default();
    let mut owner = running(&factory, request, &pty);
    let mut native = owner.take_native_requests().unwrap();
    pty.output(b"\x1b]52;p;?\x07\x1b]52;s;?\x07\x1b]52;p;c2Vuc2l0aXZl\x07");
    pty.until(|state| state.written == b"\x1b]52;p;\x07\x1b]52;s;\x07");
    assert!(native.try_next().unwrap().is_none());
    owner.retire(RetirementReason::Stop);
    owner.join().unwrap();
}

#[test]
fn fragmented_osc52_cannot_reacquire_the_same_fence_after_focus_restoration() {
    let factory = factory(RuntimeLimits::default());
    let request = request(1, grid(24, 80));
    let pty = Pty::default();
    let mut owner = running(&factory, request.clone(), &pty);
    let mut native = owner.take_native_requests().unwrap();
    pty.output(b"prefix\x1b]52;c;");
    pty.until(|state| state.engine_bytes == b"prefix");
    let mut parts = request.authority().parts();
    parts.focused_pane = false;
    parts.terminal_input_focus = false;
    parts.native_effects_eligible = false;
    let unfocused = RuntimeAuthority::new(parts).unwrap();
    owner
        .transition_authority(
            RuntimeAuthorityTransition::new(request.authority(), unfocused).unwrap(),
        )
        .unwrap()
        .receive_blocking()
        .unwrap();
    owner
        .transition_authority(
            RuntimeAuthorityTransition::new(unfocused, request.authority()).unwrap(),
        )
        .unwrap()
        .receive_blocking()
        .unwrap();
    pty.output(b"?\x07");
    pty.until(|state| state.written == b"\x1b]52;c;\x07");
    assert!(native.try_next().unwrap().is_none());
    owner.retire(RetirementReason::Stop);
    owner.join().unwrap();
}

#[test]
fn native_completion_keeps_input_parked_across_output_work_quanta() {
    for effects in [126, 254, 510] {
        let factory = factory(RuntimeLimits::default());
        let request = request(1, grid(24, 80));
        let pty = Pty::default();
        let mut owner = running(&factory, request.clone(), &pty);
        let mut native = owner.take_native_requests().unwrap();
        pty.output(b"\x1b]52;c;?\x07");
        let read = native.next_blocking().unwrap().unwrap();
        pty.update(|state| state.poll_block_at = Some(state.poll_count + 1));
        pty.until(|state| state.poll_block_at == Some(state.poll_count));

        // Keep the query in a separate, already-published reader chunk. The ST
        // left by OSC 52, the first chunk dequeue and its terminal effects use
        // whole output-work quanta without leaving an effect in the deque.
        let output = b"\x1b[0m".repeat(effects);
        pty.output_chunks(&[&output, b"query"]);
        pty.until(|state| state.reader_waiting && state.output_chunks.is_empty());
        let input = owner
            .handle()
            .try_submit(fence(request.authority()), ime("later"))
            .unwrap();
        let completion = NativeCompletion::new(
            &read,
            NativeCompletionKind::ClipboardText(NativeText::new("clipboard".into()).unwrap()),
        )
        .unwrap();
        let completed = owner
            .handle()
            .try_submit(
                read.fence(),
                RuntimeCommand::CompleteNativeRequest(completion),
            )
            .unwrap();
        pty.update(|state| state.poll_block_at = None);
        completed.receive_blocking().unwrap();
        input.receive_blocking().unwrap();
        let expected = b"\x1b]52;c;Y2xpcGJvYXJk\x07replylater";
        pty.until(|state| state.written.len() == expected.len());
        owner.retire(RetirementReason::Stop);
        owner.join().unwrap();
        assert_eq!(pty.inspect(|state| state.written.clone()), expected);
    }
}

#[test]
fn native_clipboard_wait_releases_input_while_output_continues() {
    for expired in [false, true] {
        let clock = Arc::new(Clock::default());
        let factory = RuntimeFactory::new(RuntimeLimits::default(), clock.clone()).unwrap();
        let request = request(1, grid(24, 80));
        let pty = Pty::default();
        let mut owner = running(&factory, request.clone(), &pty);
        let mut native = owner.take_native_requests().unwrap();
        pty.output(b"\x1b]52;c;?\x07");
        let read = native.next_blocking().unwrap().unwrap();
        pty.update(|state| state.poll_block_at = Some(state.poll_count + 1));
        pty.until(|state| state.poll_block_at == Some(state.poll_count));

        // The query must precede Ctrl-C even across several output quanta. The
        // reader then replenishes ANSI output forever, including after the wait.
        let output = b"\x1b[0m".repeat(510);
        pty.output_chunks(&[&output, b"query"]);
        pty.update(|state| state.continuous_output = Some(output));
        pty.until(|state| state.continuous_reads >= 7);
        let interrupt = owner
            .handle()
            .try_submit(
                fence(request.authority()),
                RuntimeCommand::Key(spaceterm_terminal::KeyInput::text_input("\u{3}")),
            )
            .unwrap();
        // Park the interrupt before either completing or expiring the request.
        let before = pty.inspect(|state| state.poll_count);
        pty.update(|state| state.poll_block_at = Some(before + 1));
        pty.until(|state| state.poll_count == before + 1);
        let reads_before = pty.inspect(|state| state.continuous_reads);
        if expired {
            clock.0.store(1000, Ordering::Release);
        } else {
            let completion = NativeCompletion::new(
                &read,
                NativeCompletionKind::ClipboardText(NativeText::new("clipboard".into()).unwrap()),
            )
            .unwrap();
            let completed = owner
                .handle()
                .try_submit(
                    read.fence(),
                    RuntimeCommand::CompleteNativeRequest(completion),
                )
                .unwrap();
            pty.update(|state| state.poll_block_at = None);
            completed.receive_blocking().unwrap();
        }
        pty.update(|state| state.poll_block_at = None);
        let later = owner
            .handle()
            .try_submit(fence(request.authority()), ime("later"))
            .unwrap();
        let resize = owner
            .handle()
            .try_submit(
                fence(request.authority()),
                RuntimeCommand::Resize(grid(30, 100)),
            )
            .unwrap();
        pty.until(|state| state.written.ends_with(b"\x03later") && state.sizes.len() == 2);
        assert_eq!(
            interrupt.receive_blocking().unwrap(),
            EffectDisposition::Applied
        );
        assert_eq!(
            later.receive_blocking().unwrap(),
            EffectDisposition::Applied
        );
        assert_eq!(
            resize.receive_blocking().unwrap(),
            EffectDisposition::Applied
        );
        let expected = if expired {
            b"\x1b]52;c;\x07reply\x03later".as_slice()
        } else {
            b"\x1b]52;c;Y2xpcGJvYXJk\x07reply\x03later".as_slice()
        };
        assert_eq!(pty.inspect(|state| state.written.clone()), expected);
        pty.until(|state| state.continuous_reads > reads_before + 8);
        owner.retire(RetirementReason::Stop);
        owner.join().unwrap();
    }
}

#[test]
fn native_completion_preserves_preceding_eof_before_deferred_input() {
    for read_failure in [false, true] {
        let factory = factory(RuntimeLimits::default());
        let request = request(1, grid(24, 80));
        let pty = Pty::default();
        let mut owner = running(&factory, request.clone(), &pty);
        let mut native = owner.take_native_requests().unwrap();
        pty.output(b"\x1b]52;c;?\x07");
        let read = native.next_blocking().unwrap().unwrap();
        pty.update(|state| state.poll_block_at = Some(state.poll_count + 1));
        pty.until(|state| state.poll_block_at == Some(state.poll_count));
        let input = owner
            .handle()
            .try_submit(fence(request.authority()), ime("must-not-write"))
            .unwrap();
        let before = pty.inspect(|state| state.poll_count);
        pty.update(|state| state.poll_block_at = Some(before + 1));
        pty.until(|state| state.poll_count == before + 1);
        pty.update(|state| {
            state.eof = !read_failure;
            state.read_io_failure = read_failure;
        });
        // Reader destruction proves Finished was published before completion.
        pty.until(|state| state.reader_dropped);
        let completed = owner
            .handle()
            .try_submit(
                read.fence(),
                RuntimeCommand::CompleteNativeRequest(
                    NativeCompletion::new(&read, NativeCompletionKind::Denied).unwrap(),
                ),
            )
            .unwrap();
        pty.update(|state| state.poll_block_at = None);
        completed.receive_blocking().unwrap();
        let disposition = input.receive_blocking().unwrap();
        owner.join().unwrap();
        assert_eq!(disposition, EffectDisposition::Retired);
        assert!(!pty.inspect(|state| state.written.ends_with(b"must-not-write")));
        assert_eq!(
            owner
                .lifecycle_snapshot()
                .terminal_result()
                .unwrap()
                .status(),
            if read_failure {
                RuntimeTerminalStatus::RuntimeFailed {
                    class: FailureClass::RuntimeFault,
                }
            } else {
                RuntimeTerminalStatus::OrdinaryExit {
                    exit_status: Some(0),
                }
            }
        );
    }
}

#[test]
fn native_completion_preserves_deferred_terminal_replies_before_later_input() {
    let factory = factory(RuntimeLimits::default());
    let request = request(1, grid(24, 80));
    let pty = Pty::default();
    let mut owner = running(&factory, request.clone(), &pty);
    let mut native = owner.take_native_requests().unwrap();
    pty.output(b"\x1b]52;c;?\x07query");
    let read = native.next_blocking().unwrap().unwrap();
    let input = owner
        .handle()
        .try_submit(fence(request.authority()), ime("later"))
        .unwrap();
    let completion = NativeCompletion::new(
        &read,
        NativeCompletionKind::ClipboardText(NativeText::new("clipboard".into()).unwrap()),
    )
    .unwrap();
    owner
        .handle()
        .try_submit(
            read.fence(),
            RuntimeCommand::CompleteNativeRequest(completion),
        )
        .unwrap()
        .receive_blocking()
        .unwrap();
    input.receive_blocking().unwrap();
    let expected = b"\x1b]52;c;Y2xpcGJvYXJk\x07replylater";
    pty.until(|state| state.written.len() == expected.len());
    owner.retire(RetirementReason::Stop);
    owner.join().unwrap();
    assert_eq!(pty.inspect(|state| state.written.clone()), expected);
}
