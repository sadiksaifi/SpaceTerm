use super::tests::{await_screen, command, host, lines, size};
use super::*;
use spaceterm_server::hierarchy::{Axis, Mutation, Outcome};
use spaceterm_server::protocol::screen::ScreenUpdate;

// Exercise the host commands and the server's actual captured screens.
fn exchange(host: &mut impl HostCommands, device: u8, connection: u8, request: Request) -> Reply {
    host.request([device; 32], [connection; 32], request)
}

fn text(update: &ScreenUpdate) -> String {
    lines(update).join("\n")
}

/// The Scrollback rows the server retains above the screen.
fn retained(update: &ScreenUpdate) -> u64 {
    update.history.total - update.history.oldest
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
    let mut host = WorkspaceHost::new(host());
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
                control: spaceterm_server::hierarchy::CreateControl::Creator,
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
    let first_before = await_screen(&mut host, [1; 32], first, |s| {
        text(s).lines().any(|r| r.trim_end() == "FIRST-prompt")
    });
    let second_before = await_screen(&mut host, [1; 32], second, |s| {
        text(s).lines().any(|r| r.trim_end() == "SECOND-prompt")
    });
    assert!(retained(&first_before) > 0);
    assert!(retained(&second_before) > 0);

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
    let untouched = await_screen(&mut host, [1; 32], first, |_| true);
    assert_eq!(retained(&untouched), retained(&first_before));
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
        host.view(&resumed, false, 60);
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
        // A hidden Pane's Terminal Session publishes nothing, so the captured screen stays.
        std::thread::sleep(Duration::from_millis(100));
        let hidden = await_screen(&mut host, [1; 32], resumed, |_| true);
        assert_eq!(text(&hidden), text(&first_before));
        assert_eq!(retained(&hidden), retained(&first_before));
        host.view(&resumed, true, 60);
    }
    let cleared = await_screen(&mut host, [1; 32], resumed, |s| {
        s.generation > first_before.generation && retained(s) == 0
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
    let second_after = await_screen(&mut host, [1; 32], second, |_| true);
    assert_eq!(text(&second_after), text(&second_before));
    assert_eq!(retained(&second_after), retained(&second_before));
    host.shutdown().unwrap();
}
