use super::tests::{
    attach, await_text, effects, focus, host, keyframe, lines, native_command, screen, size,
};
use super::*;
use pane::effects::{Completion, Effect};
use spaceterm_server::protocol::PaneEvent;
use std::time::Instant;

#[test]
fn clipboard_reads_reply_to_the_originating_pty_and_do_not_block_screen_queries() {
    let mut host = host();
    let lease = attach(&mut host, 1, true);
    focus(&mut host, lease, 1, FocusTarget::Terminal);
    native_command(
        &mut host,
        lease,
        1,
        "set +m; stty raw -echo; printf '\\033[2J\\033[H\\033]52;c;?\\007'; dd bs=1 count=12 2>/dev/null | od -An -tx1 | tr -d ' \\n'; printf '\\r\\n'",
    );
    let [request] = effects(&mut host, lease, 1).try_into().unwrap();
    assert_eq!(request.generation, 1);
    assert!(matches!(request.effect, Effect::ClipboardRead { .. }));
    assert!(screen(&mut host, [1; 32], lease).is_ok());
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
    await_text(&mut host, lease, "1b5d35323b633b59574a6a07");
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
            "set +m; stty raw -echo; printf '\\033[2J\\033[H\\033]52;c;?\\007'; dd bs=1 count=8 2>/dev/null | od -An -tx1 | tr -d ' \\n'; printf '\\r\\n'",
        );
        let [request] = effects(&mut host, lease, 1).try_into().unwrap();
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
        await_text(&mut host, current, "1b5d35323b633b07");
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
    for request in effects(&mut host, lease, 2) {
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
        "printf '\\033[2J\\033[H\\033]7701;permissions=accessibility\\007'; printf 'DONE\\r\\n'",
    );
    // Wait for the output without observing, so the effect stays queued under generation 1.
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let pane = host.pane.as_mut().unwrap();
        pane.drain();
        if pane.screen.as_deref().is_some_and(|screen| {
            lines(&keyframe(screen))
                .iter()
                .any(|line| line.trim_end() == "DONE")
        }) {
            break;
        }
        assert!(Instant::now() < deadline, "DONE did not arrive");
        std::thread::sleep(Duration::from_millis(10));
    }
    focus(&mut host, lease, 2, FocusTarget::Unfocused);
    focus(&mut host, lease, 3, FocusTarget::Terminal);
    let Observed::Live(observation) = host.observe([1; 32], &lease) else {
        panic!("the controller lost its Pane")
    };
    assert!(observation.effects.is_empty());
    assert!(matches!(
        host.observe([2; 32], &lease),
        Observed::Gone {
            event: PaneEvent::Takeover(_),
            ..
        }
    ));
    host.shutdown().unwrap();
}
