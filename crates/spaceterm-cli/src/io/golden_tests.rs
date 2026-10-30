//! Pane envelopes use the terminal command renderer, including failures that retain run data.
use super::render;
use crate::contract::{golden::assert_golden, *};
use crate::io::{Done, controlled, cursor::Cursor};

fn server() -> ServerRef {
    ServerRef {
        name: "local".into(),
        route: "local".into(),
        id: "09".repeat(32),
    }
}

fn target() -> Vec<TargetStep> {
    vec![TargetStep {
        level: Level::Pane,
        id: Some("02".repeat(32)),
        path: None,
        label: Some("Terminal".into()),
        source: Source::Flag,
    }]
}

fn rows() -> Rows {
    let cursor = Cursor {
        server: [9; 16],
        pane: [2; 16],
        session: [3; 16],
        epoch: 1,
        row: 1,
    };
    Rows {
        session: "03".repeat(32),
        history: HistoryRef {
            epoch: 1,
            oldest: 0,
            total: 1,
        },
        rows: vec![Row {
            row: 0,
            text: "hello".into(),
            wrapped: false,
        }],
        screen: Some(Screen {
            rows: vec!["$ ".into()],
            cursor: CursorPosition { row: 0, column: 2 },
            size: ScreenSize {
                columns: 80,
                rows: 1,
            },
            alternate_screen: false,
        }),
        next: Next {
            before: Cursor { row: 0, ..cursor }.encode(),
            after: cursor.encode(),
        },
        gap: None,
        truncated: false,
        format: Format::Text,
        coverage: Coverage::Complete,
    }
}

fn check<D: serde::Serialize>(name: &str, command: &str, done: Done<D>, exit: u8) {
    let output = render(command, &server(), done, |_| String::new());
    assert_eq!(output.envelope.exit(), exit);
    assert_golden(name, &output.envelope);
}

#[test]
fn pane_run_success_and_unconfirmed_envelopes() {
    let data = PaneRun {
        target: target(),
        run: Run {
            id: "04".repeat(16),
            state: RunStateName::Finished,
            reason: None,
            exit_status: Some(0),
            duration_ms: Some(25),
            started_shell: true,
            repaired: false,
        },
        output: Some(rows()),
    };
    check(
        "pane_run_finished",
        "pane.run",
        Done::ok(data.clone(), Vec::new()),
        0,
    );
    let reason = RunDoubtName::CmdlineMismatch;
    let unconfirmed = PaneRun {
        run: Run {
            state: RunStateName::Unconfirmed,
            reason: Some(reason),
            exit_status: None,
            duration_ms: None,
            ..data.run
        },
        output: None,
        ..data
    };
    check(
        "pane_run_unconfirmed",
        "pane.run",
        Done {
            data: Some(unconfirmed),
            warnings: Vec::new(),
            failure: Some(Failure::with(
                Code::RunUnconfirmed,
                Details::RunUnconfirmed(RunUnconfirmedDetails { reason }),
            )),
        },
        7,
    );
    check(
        "pane_run_not_at_prompt",
        "pane.run",
        Done::<PaneRun>::failed(Failure::new(Code::NotAtPrompt)),
        4,
    );
    check(
        "pane_run_controlled",
        "pane.run",
        Done::<PaneRun>::failed(controlled(ControlledReason::Other)),
        3,
    );
}

#[test]
fn pane_send_success_and_controlled_envelopes() {
    check(
        "pane_send_delivered",
        "pane.send",
        Done::ok(
            PaneSend {
                target: target(),
                input: Delivery {
                    serial: 1,
                    delivered: true,
                },
            },
            Vec::new(),
        ),
        0,
    );
    check(
        "pane_send_controlled",
        "pane.send",
        Done::<PaneSend>::failed(controlled(ControlledReason::Other)),
        3,
    );
}

#[test]
fn pane_read_success_and_cursor_error_envelopes() {
    check(
        "pane_read",
        "pane.read",
        Done::ok(
            PaneRead {
                target: target(),
                rows: Some(rows()),
                wait: Some(Wait {
                    matched: true,
                    row: Some(0),
                    elapsed_ms: 10,
                }),
            },
            Vec::new(),
        ),
        0,
    );
    for (name, code) in [
        ("pane_read_cursor_mismatch", Code::CursorMismatch),
        ("pane_read_history_hidden", Code::HistoryHidden),
    ] {
        check(
            name,
            "pane.read",
            Done::<PaneRead>::failed(Failure::new(code)),
            4,
        );
    }
}

#[test]
fn pane_release_envelopes() {
    for (name, released) in [("pane_release", true), ("pane_release_other", false)] {
        check(
            name,
            "pane.release",
            Done::ok(
                PaneRelease {
                    target: target(),
                    released,
                },
                Vec::new(),
            ),
            0,
        );
    }
}
