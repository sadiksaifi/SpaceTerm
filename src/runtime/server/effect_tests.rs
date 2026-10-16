use super::tests::{attach, await_text, focus, host, native_command, size};
use super::*;
use pane::effects::{Completion, Effect};
use std::time::Instant;

fn next(host: &mut impl PaneService, lease: Lease, generation: u64) -> pane::effects::Request {
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        match host.request(
            [1; 32],
            lease.connection,
            Request::NativeEffects { lease, generation },
        ) {
            Reply::NativeEffects {
                request: Some(request),
                ..
            } => return request,
            Reply::NativeEffects { request: None, .. } => {}
            reply => panic!("native request refused: {reply:?}"),
        }
        assert!(Instant::now() < deadline, "native request missing");
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn clipboard_reads_reply_to_the_originating_pty_and_do_not_block_screen_queries() {
    let mut host = host();
    let lease = attach(&mut host, 1, true);
    focus(&mut host, lease, 1, FocusTarget::Terminal);
    native_command(
        &mut host,
        lease,
        1,
        "stty raw -echo; printf '\\033]52;c;?\\007'; dd bs=1 count=12 2>/dev/null | od -An -tx1 | tr -d ' \\n'; printf '\\r\\n'",
    );
    let request = next(&mut host, lease, 1);
    assert!(matches!(request.effect, Effect::ClipboardRead { .. }));
    assert!(matches!(
        host.request([1; 32], lease.connection, Request::Poll { lease }),
        Reply::Screen(_) | Reply::Unchanged
    ));
    assert!(matches!(
        host.request(
            [1; 32],
            lease.connection,
            Request::CompleteClipboard {
                lease,
                generation: 1,
                id: request.id,
                completion: Completion::Text("abc".into())
            }
        ),
        Reply::Unchanged
    ));
    await_text(&mut host, 1, lease, "1b5d35323b633b59574a6a07");
    host.shutdown().unwrap();
}

#[test]
fn clipboard_completions_cannot_cross_focus_loss_takeover_or_disconnect() {
    for invalidate in 0..3 {
        let mut host = host();
        let lease = attach(&mut host, 1, true);
        focus(&mut host, lease, 1, FocusTarget::Terminal);
        native_command(
            &mut host,
            lease,
            1,
            "stty raw -echo; printf '\\033]52;c;?\\007'; dd bs=1 count=8 2>/dev/null | od -An -tx1 | tr -d ' \\n'; printf '\\r\\n'",
        );
        let request = next(&mut host, lease, 1);
        match invalidate {
            0 => {
                focus(&mut host, lease, 2, FocusTarget::Unfocused);
                focus(&mut host, lease, 3, FocusTarget::Terminal);
            }
            1 => {
                assert!(matches!(
                    host.request(
                        [2; 32],
                        [2; 32],
                        Request::TakeOver {
                            target: lease.into(),
                            size: size(),
                            style: None
                        }
                    ),
                    Reply::Attached { .. }
                ));
            }
            _ => host.disconnected(lease.connection),
        }
        assert!(matches!(
            host.request(
                [1; 32],
                lease.connection,
                Request::CompleteClipboard {
                    lease,
                    generation: 1,
                    id: request.id,
                    completion: Completion::Text("secret".into())
                }
            ),
            Reply::Refused(Refusal::Stale)
        ));
        let current = if invalidate == 1 {
            let Reply::Attached { lease } = host.request(
                [1; 32],
                [3; 32],
                Request::TakeOver {
                    target: lease.into(),
                    size: size(),
                    style: None,
                },
            ) else {
                panic!()
            };
            lease
        } else if invalidate == 2 {
            attach(&mut host, 3, false)
        } else {
            lease
        };
        await_text(
            &mut host,
            if current.connection == [3; 32] { 3 } else { 1 },
            current,
            "1b5d35323b633b07",
        );
        host.shutdown().unwrap();
    }
}

#[test]
fn permission_and_background_attention_use_the_controlling_clients_current_generation() {
    let mut host = host();
    let lease = attach(&mut host, 1, true);
    focus(&mut host, lease, 1, FocusTarget::Terminal);
    native_command(
        &mut host,
        lease,
        1,
        "sleep 0.1; printf '\\033]7701;permissions=screen-recording,accessibility\\007\\007'",
    );
    focus(&mut host, lease, 2, FocusTarget::Unfocused);
    let mut permission = false;
    let mut bell = false;
    for _ in 0..2 {
        let request = next(&mut host, lease, 2);
        assert_eq!(request.generation, 2);
        match request.effect {
            Effect::PermissionSetup {
                screen_recording: true,
                accessibility: true,
            } => permission = true,
            Effect::Bell => bell = true,
            other => panic!("unexpected effect: {other:?}"),
        }
    }
    assert!(permission && bell);
    host.shutdown().unwrap();
}

#[test]
fn stale_native_events_and_absent_clients_never_acquire_a_later_grant() {
    let mut host = host();
    let lease = attach(&mut host, 1, true);
    focus(&mut host, lease, 1, FocusTarget::Terminal);
    native_command(
        &mut host,
        lease,
        1,
        "printf '\\033]7701;permissions=accessibility\\007'; printf 'DONE\\r\\n'",
    );
    await_text(&mut host, 1, lease, "DONE");
    focus(&mut host, lease, 2, FocusTarget::Unfocused);
    focus(&mut host, lease, 3, FocusTarget::Terminal);
    assert!(matches!(
        host.request(
            [1; 32],
            lease.connection,
            Request::NativeEffects {
                lease,
                generation: 3
            }
        ),
        Reply::NativeEffects { request: None, .. }
    ));
    assert!(matches!(
        host.request(
            [2; 32],
            lease.connection,
            Request::NativeEffects {
                lease,
                generation: 3
            }
        ),
        Reply::Refused(Refusal::Stale)
    ));
    host.shutdown().unwrap();
}
