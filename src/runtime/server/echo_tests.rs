use super::echo::KEY_ECHO_WAIT;
use super::tests::{
    attach, await_text, batch_key, command, focus, host, poll_changes, presentation,
};
use super::*;
use pane::presentation::ScreenDecoder;
use std::time::Instant;

fn keys(lease: Lease, generation: u64, text: &str) -> Request {
    Request::Keys {
        lease,
        generation,
        keys: vec![batch_key(text, false)],
    }
}

fn rows(screen: &Screen) -> Vec<String> {
    screen
        .rows
        .iter()
        .map(|row| {
            row.iter()
                .map(|cell| cell.text.as_str())
                .collect::<String>()
        })
        .map(|row| row.trim_end().to_owned())
        .collect()
}

/// Brings the client's baseline up to date and waits until the Pane stops publishing.
fn settle(
    host: &mut DevelopmentPaneHost,
    lease: Lease,
    decoder: &mut ScreenDecoder,
) -> Box<Screen> {
    let mut latest = None;
    let mut quiet = 0;
    let deadline = Instant::now() + Duration::from_secs(10);
    while quiet < 4 {
        let reply = presentation(host.request(
            [1; 32],
            lease.connection,
            poll_changes(lease, decoder.baseline()),
        ));
        match decoder.apply(reply).unwrap() {
            Reply::Screen(screen) => {
                latest = Some(screen);
                quiet = 0;
            }
            Reply::Unchanged | Reply::AccessibilityRevision { .. } => quiet += 1,
            reply => panic!("output check refused: {reply:?}"),
        }
        assert!(Instant::now() < deadline, "the Pane did not settle");
        std::thread::sleep(Duration::from_millis(30));
    }
    latest.expect("no baseline screen")
}

/// A loaded test host can take longer than the production bound to echo. The wait still ends
/// at the first publication, so this only removes scheduling noise from the result.
const LOADED_HOST: Duration = Duration::from_secs(5);

/// Sends a batch, waits as the host does without its lock, and finishes it.
fn echo(host: &mut DevelopmentPaneHost, lease: Lease, generation: u64, text: &str) -> Reply {
    let Handled::Later {
        mut pending,
        deadline,
        finish,
    } = host.handle([1; 32], lease.connection, keys(lease, generation, text))
    else {
        panic!("an accepted visible batch must wait for its echo")
    };
    pending.wait(deadline + LOADED_HOST);
    host.finish([1; 32], lease.connection, finish)
}

fn ready(host: &mut DevelopmentPaneHost) -> (Lease, ScreenDecoder) {
    let lease = attach(host, 1, true);
    command(host, 1, lease, "printf 'ECHO-READY\\r\\n'");
    await_text(host, 1, lease, "ECHO-READY");
    focus(host, lease, 1, FocusTarget::Terminal);
    let mut decoder = ScreenDecoder::default();
    settle(host, lease, &mut decoder);
    (lease, decoder)
}

#[test]
fn an_accepted_key_batch_carries_its_echo_and_the_next_check_continues_from_it() {
    let mut host = host();
    let (lease, mut decoder) = ready(&mut host);
    let reply = echo(&mut host, lease, 1, "x");
    assert!(matches!(&reply, Reply::ScreenChanges(_)), "{reply:?}");
    let Reply::Screen(echoed) = decoder.apply(reply).unwrap() else {
        panic!("the echo did not continue the client's baseline")
    };
    assert!(rows(&echoed).iter().any(|row| row.ends_with("$ x")));
    // The echo advanced the shared encoder once: the next check has nothing to resend.
    assert!(matches!(
        presentation(host.request(
            [1; 32],
            lease.connection,
            poll_changes(lease, decoder.baseline())
        )),
        Reply::Unchanged | Reply::AccessibilityRevision { .. }
    ));
    // A later echo continues from the first one's baseline.
    let Reply::Screen(next) = decoder.apply(echo(&mut host, lease, 1, "y")).unwrap() else {
        panic!("the second echo did not continue the first")
    };
    assert!(rows(&next).iter().any(|row| row.ends_with("$ xy")));
    host.shutdown().unwrap();
}

#[test]
fn an_echo_absent_at_the_deadline_arrives_once_in_the_next_check() {
    let mut host = host();
    let lease = attach(&mut host, 1, true);
    command(
        &mut host,
        1,
        lease,
        "stty -echo; read line; sleep 0.2; printf 'LATE-OUTPUT\\r\\n'",
    );
    focus(&mut host, lease, 1, FocusTarget::Terminal);
    let mut decoder = ScreenDecoder::default();
    settle(&mut host, lease, &mut decoder);
    let mut enter = batch_key("", false);
    enter.physical = PhysicalKey::Enter as u32;
    enter.logical = "enter".into();
    enter.text = None;
    let Handled::Later {
        mut pending,
        deadline,
        finish,
    } = host.handle(
        [1; 32],
        lease.connection,
        Request::Keys {
            lease,
            generation: 1,
            keys: vec![enter],
        },
    )
    else {
        panic!("an accepted visible batch must wait for its echo")
    };
    let started = Instant::now();
    pending.wait(deadline);
    assert!(started.elapsed() + Duration::from_millis(1) >= KEY_ECHO_WAIT);
    assert!(started.elapsed() < Duration::from_millis(150));
    assert!(matches!(
        host.finish([1; 32], lease.connection, finish),
        Reply::Unchanged
    ));
    let deadline = Instant::now() + Duration::from_secs(10);
    let late = loop {
        let reply = presentation(host.request(
            [1; 32],
            lease.connection,
            poll_changes(lease, decoder.baseline()),
        ));
        let delta = matches!(reply, Reply::ScreenChanges(_));
        match decoder.apply(reply).unwrap() {
            Reply::Screen(screen) if rows(&screen).iter().any(|row| row == "LATE-OUTPUT") => {
                break screen;
            }
            // A skipped or repeated delta names a base the client does not hold.
            Reply::Unchanged if delta => panic!("a delta did not continue the client's baseline"),
            _ => {}
        }
        assert!(Instant::now() < deadline, "late output missing");
        std::thread::sleep(Duration::from_millis(10));
    };
    assert_eq!(
        rows(&late)
            .iter()
            .filter(|row| *row == "LATE-OUTPUT")
            .count(),
        1
    );
    host.shutdown().unwrap();
}

#[test]
fn refused_hidden_and_stale_batches_answer_without_waiting() {
    let mut host = host();
    let (lease, _) = ready(&mut host);
    let started = Instant::now();
    assert!(matches!(
        host.handle([1; 32], lease.connection, keys(lease, 7, "x")),
        Handled::Now(Reply::Refused(Refusal::Stale))
    ));
    assert!(matches!(
        host.handle([2; 32], lease.connection, keys(lease, 1, "x")),
        Handled::Now(Reply::Refused(Refusal::Stale))
    ));
    focus(&mut host, lease, 2, FocusTarget::FocusedPane);
    assert!(matches!(
        host.handle([1; 32], lease.connection, keys(lease, 2, "x")),
        Handled::Now(Reply::Refused(Refusal::Stale))
    ));
    focus(&mut host, lease, 3, FocusTarget::Terminal);
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
    assert!(matches!(
        host.handle([1; 32], lease.connection, keys(lease, 3, "x")),
        Handled::Now(Reply::Unchanged)
    ));
    assert!(started.elapsed() < KEY_ECHO_WAIT);
    host.shutdown().unwrap();
}

#[test]
fn a_finish_token_works_once_for_its_own_controller_and_never_refuses() {
    for case in 0..4 {
        let mut host = host();
        let (lease, mut decoder) = ready(&mut host);
        let Handled::Later {
            mut pending,
            deadline,
            finish,
        } = host.handle([1; 32], lease.connection, keys(lease, 1, "x"))
        else {
            panic!("an accepted visible batch must wait for its echo")
        };
        pending.wait(deadline + LOADED_HOST);
        let reply = match case {
            // Another token, device or connection finishes nothing.
            0 => host.finish([1; 32], lease.connection, finish.wrapping_add(1)),
            1 => host.finish([2; 32], lease.connection, finish),
            2 => {
                host.disconnected(lease.connection);
                host.finish([1; 32], lease.connection, finish)
            }
            // A token finishes once.
            _ => {
                let first = host.finish([1; 32], lease.connection, finish);
                assert!(matches!(first, Reply::ScreenChanges(_)), "{first:?}");
                host.finish([1; 32], lease.connection, finish)
            }
        };
        assert!(matches!(reply, Reply::Unchanged), "case {case}: {reply:?}");
        if case < 2 {
            // The unconsumed echo is still delivered exactly once by output checks.
            let screen = settle(&mut host, lease, &mut decoder);
            assert!(rows(&screen).iter().any(|row| row.ends_with("$ x")));
        }
        host.shutdown().unwrap();
    }
}
