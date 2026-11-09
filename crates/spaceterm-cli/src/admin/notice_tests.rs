//! The Pending Version notice at the machine verbs' output boundary.
use super::Reply;
use crate::contract::{Envelope, MachineRef, Mode};

const NOTICE: &str = "notice: SpaceTerm Core 0.7.2 is installed on penguin. Update with: \
                      spaceterm --machine penguin restart";

fn reply() -> Reply {
    let machine = MachineRef {
        name: "penguin".into(),
        route: "penguin".into(),
        id: "5b".repeat(32),
    };
    let mut reply = Reply::new(
        &Envelope::ok("network", Some(machine), crate::contract::Empty {}),
        "Network Access: off\n".into(),
    );
    reply.notice = Some(NOTICE.into());
    reply
}

/// What one reply writes on stdout and stderr in `mode`.
fn written(mode: Mode, stderr_terminal: bool) -> (String, String) {
    let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
    assert_eq!(
        reply().write_to(mode, stderr_terminal, &mut stdout, &mut stderr),
        0
    );
    (
        String::from_utf8(stdout).unwrap(),
        String::from_utf8(stderr).unwrap(),
    )
}

#[test]
fn the_notice_is_one_stderr_line_after_the_result_in_interactive_text_mode() {
    for mode in [
        Mode::default(),
        Mode {
            verbose: true,
            ..Mode::default()
        },
    ] {
        let (stdout, stderr) = written(mode, true);
        assert_eq!(stdout, "Network Access: off\n");
        assert_eq!(stderr, format!("{NOTICE}\n"));
    }
}

#[test]
fn json_quiet_and_a_nonterminal_stderr_print_no_notice() {
    for (mode, terminal) in [
        (
            Mode {
                json: true,
                ..Mode::default()
            },
            true,
        ),
        (
            Mode {
                quiet: true,
                ..Mode::default()
            },
            true,
        ),
        (Mode::default(), false),
    ] {
        let (stdout, stderr) = written(mode, terminal);
        assert!(!stdout.contains("notice:"), "{mode:?}: {stdout}");
        assert!(stderr.is_empty(), "{mode:?}: {stderr}");
    }
    let (stdout, _) = written(
        Mode {
            json: true,
            ..Mode::default()
        },
        true,
    );
    let envelope: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(
        envelope["command"], "network",
        "stdout holds the envelope alone"
    );
}
