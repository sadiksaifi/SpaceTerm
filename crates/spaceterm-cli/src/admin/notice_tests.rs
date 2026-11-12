//! The Pending Version notice at the machine verbs' output boundary.
use super::Reply;
use crate::contract::{Envelope, MachineRef, Mode};

const NOTICE: &str = "notice: SpaceTerm Core 0.7.2 is downloaded on penguin. Restarting ends 2 \
                      Terminal Sessions. Restart with: spaceterm --machine penguin restart";

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

/// A service lane that answers as a Core of service 1.2 when it has detailed facts, and as an
/// older Core otherwise, and records what it was asked.
struct Lane {
    detailed: Option<spaceterm_core::update::facts::DetailedUpdateFacts>,
    facts: spaceterm_core::update::facts::UpdateFacts,
    asked: Vec<&'static str>,
}

impl super::UpdateLane for Lane {
    fn detailed_facts(
        &mut self,
    ) -> Result<
        Option<spaceterm_core::update::facts::DetailedUpdateFacts>,
        spaceterm_core::service::lane::client::LaneError,
    > {
        self.asked.push("detailed facts");
        Ok(self.detailed.clone())
    }
    fn facts(
        &mut self,
    ) -> Result<
        spaceterm_core::update::facts::UpdateFacts,
        spaceterm_core::service::lane::client::LaneError,
    > {
        self.asked.push("facts");
        Ok(self.facts.clone())
    }
}

const INSTALLED: u64 = 1_793_697_120;
const RESTART: &str = "spaceterm --machine penguin restart";

fn waiting() -> spaceterm_core::update::facts::UpdateFacts {
    use spaceterm_core::update::facts::{
        PendingVersion, Platform, Protocols, UpdateCheck, UpdateFacts, Version,
    };
    UpdateFacts {
        revision: 2,
        platform: Platform::LinuxX86_64,
        running: Version::parse("0.7.0").unwrap(),
        pending: Some(PendingVersion {
            version: Version::parse("0.8.0").unwrap(),
            protocols: Protocols::CURRENT,
            breaking: false,
            installed_at: INSTALLED,
            overdue_since: None,
            stranded: Vec::new(),
        }),
        sessions: 2,
        operation: None,
        check: UpdateCheck::Never,
        skipped: None,
        last_failure: None,
    }
}

#[test]
fn a_1_2_core_s_reported_continuation_reaches_the_notice_and_an_older_core_s_is_derived() {
    use spaceterm_core::update::continuation::Continuation;
    use spaceterm_core::update::facts::DetailedUpdateFacts;
    let at = INSTALLED + crate::notice::ASK_AFTER;
    let mut newer = Lane {
        detailed: Some(DetailedUpdateFacts {
            facts: waiting(),
            continuation: Some(Continuation::WhenNothingRuns),
            working: 0,
            restart: None,
        }),
        facts: waiting(),
        asked: Vec::new(),
    };
    let reported = super::reported(&mut newer).unwrap();
    assert_eq!(newer.asked, ["detailed facts"], "one snapshot is enough");
    assert_eq!(
        crate::notice::line(
            &reported.facts,
            reported.continuation,
            "penguin",
            RESTART,
            at
        )
        .as_deref(),
        Some(
            "notice: SpaceTerm Core 0.8.0 is downloaded on penguin. SpaceTerm Core restarts into \
             it after an hour with nothing running. Restart now with: spaceterm --machine \
             penguin restart"
        )
    );
    let mut older = Lane {
        detailed: None,
        facts: waiting(),
        asked: Vec::new(),
    };
    let reported = super::reported(&mut older).unwrap();
    assert_eq!(older.asked, ["detailed facts", "facts"]);
    assert_eq!(
        crate::notice::line(
            &reported.facts,
            reported.continuation,
            "penguin",
            RESTART,
            at
        )
        .as_deref(),
        Some(
            "notice: A SpaceTerm Core update to 0.8.0 has waited 2 days on penguin. Restarting \
             into it ends 2 Terminal Sessions. Restart with: spaceterm --machine penguin restart"
        )
    );
}

#[test]
fn status_says_what_a_1_2_core_reports_and_keeps_its_envelope() {
    use spaceterm_core::update::continuation::Continuation;
    let lane = super::golden_tests::lane_status(waiting());
    let commands = super::status::Commands {
        restart: RESTART,
        local: false,
    };
    let at = INSTALLED + crate::notice::ASK_AFTER;
    let core = || MachineRef {
        name: "penguin".into(),
        route: "penguin".into(),
        id: "5b".repeat(32),
    };
    let render = |continuation| {
        super::status::envelope(
            core(),
            &lane,
            Vec::new(),
            Vec::new(),
            &commands,
            continuation,
            at,
        )
    };
    let (newer, text) = render(Some(Continuation::WhenNothingRuns));
    assert!(
        text.contains(
            "Downloaded: 0.8.0 at 2026-11-03T09:12:00Z. SpaceTerm Core restarts into it after an \
             hour with nothing running. Restart now with: spaceterm --machine penguin restart\n"
        ),
        "{text}"
    );
    let (older, text) = render(None);
    assert!(
        text.contains("A SpaceTerm Core update to 0.8.0 has waited 2 days on penguin."),
        "{text}"
    );
    assert_eq!(
        serde_json::to_value(&newer).unwrap(),
        serde_json::to_value(&older).unwrap(),
        "the continuation changes only the text"
    );
}
