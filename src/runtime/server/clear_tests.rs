use super::tests::{command, host, lines, size};
use super::*;
use spaceterm_server::hierarchy::{Axis, Mutation, Outcome};
use spaceterm_server::protocol::interim::ScreenFrame;
use std::time::Instant;

// Exercise the host commands and the server's actual screen frames.
fn exchange(host: &mut impl HostCommands, device: u8, connection: u8, request: Request) -> Reply {
    host.request([device; 32], [connection; 32], request)
}

fn screen(
    host: &mut impl HostCommands,
    lease: Lease,
    ready: impl Fn(&ScreenFrame) -> bool,
) -> Box<ScreenFrame> {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        match exchange(
            host,
            1,
            lease.connection[0],
            Request::PollChanges { lease, base: None },
        ) {
            Reply::Screen(screen) if ready(&screen) => return screen,
            Reply::Screen(_) | Reply::Unchanged => {}
            reply => panic!("screen refused: {reply:?}"),
        }
        assert!(
            Instant::now() < deadline,
            "clear publication did not arrive"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn text(screen: &ScreenFrame) -> String {
    lines(screen).join("\n")
}

#[test]
fn clear_screen_and_scrollback_targets_one_pane_and_fences_control_and_connection() {
    clear_under_lease(false);
}

#[test]
fn clear_screen_and_scrollback_preserves_the_prompt_screen_until_shell_redraw() {
    clear_under_lease(true);
}

fn clear_under_lease(preserve_prompt: bool) {
    let mut host = SharedWorkspaceHost::new(host());
    host.prepare_initial_organization().unwrap();
    let Reply::Inventory { inventory, .. } = exchange(&mut host, 1, 1, Request::Inventory) else {
        panic!("inventory missing")
    };
    let workspace = &inventory.workspaces[0];
    let tab = &workspace.tabs[0];
    let select = |pane| Request::SelectPane {
        pane,
        explicit: true,
        size: size(),
        style: None,
    };
    let Reply::Attached { lease: first } = exchange(&mut host, 1, 1, select(tab.root_pane)) else {
        panic!("first Pane missing")
    };
    let Reply::Outcome(split) = exchange(
        &mut host,
        1,
        1,
        Request::Mutate {
            sequence: 1,
            mutation: Mutation::SplitPane {
                workspace: workspace.id,
                tab: tab.id,
                source: tab.root_pane,
                axis: Axis::Down,
            },
        },
    ) else {
        panic!("split missing")
    };
    let Outcome::Accepted { target: second_id } = split.outcome else {
        panic!("split refused")
    };
    let Reply::Attached { lease: second } = exchange(&mut host, 1, 1, select(second_id)) else {
        panic!("second Pane missing")
    };
    for (lease, marker) in [(first, "FIRST"), (second, "SECOND")] {
        let prompt_marker = if preserve_prompt {
            "\\033]133;A\\007"
        } else {
            ""
        };
        assert!(matches!(
            command(
                &mut host,
                1,
                lease,
                &format!(
                    "stty -echo; i=0; while [ $i -lt 100 ]; do printf '{marker}-history-%s\\r\\n' \"$i\"; i=$((i+1)); done; printf '{prompt_marker}{marker}-prompt'; sleep 60"
                )
            ),
            Reply::Unchanged
        ));
    }
    let first_before = screen(&mut host, first, |s| {
        text(s).lines().any(|r| r.trim_end() == "FIRST-prompt")
    });
    let second_before = screen(&mut host, second, |s| {
        text(s).lines().any(|r| r.trim_end() == "SECOND-prompt")
    });
    assert!(first_before.update.reading.as_ref().unwrap().total_rows > u64::from(size().rows));
    assert!(second_before.update.reading.as_ref().unwrap().total_rows > u64::from(size().rows));

    let mut wrong_control = first;
    wrong_control.control = [99; 32];
    let mut wrong_connection = first;
    wrong_connection.connection = [99; 32];
    let mut wrong_pane = first;
    wrong_pane.session = second.session;
    for (device, connection, lease) in [
        (1, 1, wrong_control),
        (1, 1, wrong_connection),
        (1, 99, first),
        (2, 1, first),
        (1, 1, wrong_pane),
    ] {
        assert!(matches!(
            exchange(
                &mut host,
                device,
                connection,
                Request::ClearScreenAndScrollback { lease }
            ),
            Reply::Refused(Refusal::Stale)
        ));
    }
    let untouched = screen(&mut host, first, |_| true);
    assert_eq!(
        untouched.update.reading.as_ref().unwrap().total_rows,
        first_before.update.reading.as_ref().unwrap().total_rows
    );
    assert_eq!(text(&untouched), text(&first_before));

    let Reply::Attached { lease: resumed } = exchange(&mut host, 1, 3, select(tab.root_pane))
    else {
        panic!("reconnect missing")
    };
    assert!(matches!(
        exchange(
            &mut host,
            1,
            1,
            Request::ClearScreenAndScrollback { lease: first }
        ),
        Reply::Refused(Refusal::Stale)
    ));
    if !preserve_prompt {
        assert!(matches!(
            exchange(
                &mut host,
                1,
                3,
                Request::PresentationDemand {
                    lease: resumed,
                    visible: false
                }
            ),
            Reply::Unchanged
        ));
    }
    assert!(matches!(
        exchange(
            &mut host,
            1,
            3,
            Request::ClearScreenAndScrollback { lease: resumed }
        ),
        Reply::Unchanged
    ));
    if !preserve_prompt {
        assert!(matches!(
            exchange(
                &mut host,
                1,
                3,
                Request::PollChanges {
                    lease: resumed,
                    base: None
                }
            ),
            Reply::Unchanged
        ));
        assert!(matches!(
            exchange(
                &mut host,
                1,
                3,
                Request::PresentationDemand {
                    lease: resumed,
                    visible: true
                }
            ),
            Reply::Unchanged
        ));
    }
    let cleared = screen(&mut host, resumed, |s| {
        s.update.generation > first_before.update.generation
            && s.update
                .reading
                .as_ref()
                .is_some_and(|r| r.total_rows == r.visible_rows)
    });
    assert!(
        text(&cleared).contains("FIRST-prompt"),
        "keep the prompt until shell redraw"
    );
    if preserve_prompt {
        assert_eq!(
            text(&cleared),
            text(&first_before),
            "preserve visible cells pending shell redraw"
        );
    } else {
        assert!(!text(&cleared).contains("FIRST-history"));
    }
    let second_after = screen(&mut host, second, |_| true);
    assert_eq!(text(&second_after), text(&second_before));
    assert_eq!(
        second_after.update.reading.as_ref().unwrap().total_rows,
        second_before.update.reading.as_ref().unwrap().total_rows
    );
    host.shutdown().unwrap();
}
