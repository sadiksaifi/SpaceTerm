//! The `notice:` line about a SpaceTerm Core version that waits for a restart.
//!
//! A command that reached a Core prints at most one notice on stderr, after its result, about
//! that Core. It prints only in text mode, only when stderr is a terminal, and never with
//! `--quiet` or `--json`. After one day the notice escalates: it says how long the version has
//! waited and how many Terminal Sessions a restart ends.
use crate::contract::{EXIT_BROKEN_PIPE, MachineRef, Mode};
use spaceterm_core::update::facts::UpdateFacts;
use std::io::Write;

/// Whether a notice prints in this output mode.
pub fn shown(mode: Mode, stderr_terminal: bool) -> bool {
    !mode.json && !mode.quiet && stderr_terminal
}

/// Writes `notice` on stderr after a result that rendered with exit status `exit`, when the
/// notice may print: never after a broken pipe, and only where [`shown`] allows.
pub fn print(
    notice: Option<&str>,
    exit: u8,
    mode: Mode,
    stderr_terminal: bool,
    stderr: &mut dyn Write,
) {
    if let Some(notice) = notice
        && exit != EXIT_BROKEN_PIPE
        && shown(mode, stderr_terminal)
    {
        let _ = writeln!(stderr, "{notice}");
    }
}

/// The notice after a work command on `machine`, from the update facts its work connection
/// received, if any, at `now`. `agent` is the actor the command ran as, so the restart line
/// repeats it.
pub fn for_work(
    facts: Option<&UpdateFacts>,
    machine: &MachineRef,
    agent: Option<&spaceterm_core::protocol::AgentName>,
    now: u64,
) -> Option<String> {
    line(
        facts?,
        crate::admin::display_name(machine),
        &crate::admin::command_line(&machine.route, agent, &["restart"]),
        now,
    )
}

/// Seconds since the Unix epoch now.
pub fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| since.as_secs())
}

/// The notice line for `facts` of the Core on `machine` at `now`, Unix seconds, with the
/// command that restarts it. None when no version waits.
pub fn line(facts: &UpdateFacts, machine: &str, restart: &str, now: u64) -> Option<String> {
    let pending = facts.pending.as_ref()?;
    let version = &pending.version;
    let waited = now.saturating_sub(pending.installed_at);
    let overdue = pending.overdue(now);
    if !overdue {
        return Some(format!(
            "notice: SpaceTerm Core {version} is installed on {machine}. Restart to use it: {restart}"
        ));
    }
    let days = (waited / 86_400).max(1);
    let days = if days == 1 {
        "1 day".to_owned()
    } else {
        format!("{days} days")
    };
    let ends = match facts.sessions {
        0 => "Restart to use it".to_owned(),
        1 => "Restarting ends 1 Terminal Session".to_owned(),
        sessions => format!("Restarting ends {sessions} Terminal Sessions"),
    };
    Some(format!(
        "notice: SpaceTerm Core {version} has waited {days} for a restart on {machine}. {ends}: {restart}"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use spaceterm_core::update::facts::{
        OVERDUE_AFTER, OVERDUE_SINCE_AFTER, PendingVersion, Platform, Protocols, UpdateCheck,
        Version,
    };

    const INSTALLED: u64 = 1_793_750_400;
    const RESTART: &str = "spaceterm --machine penguin restart";

    fn facts(sessions: u32, overdue: bool) -> UpdateFacts {
        UpdateFacts {
            revision: 1,
            platform: Platform::LinuxX86_64,
            running: Version::parse("0.7.1").unwrap(),
            pending: Some(PendingVersion {
                version: Version::parse("0.7.2").unwrap(),
                protocols: Protocols::CURRENT,
                breaking: false,
                installed_at: INSTALLED,
                overdue_since: overdue.then_some(INSTALLED + OVERDUE_SINCE_AFTER),
                stranded: Vec::new(),
            }),
            sessions,
            operation: None,
            check: UpdateCheck::Never,
            skipped: None,
            last_failure: None,
        }
    }

    #[test]
    fn a_waiting_version_names_the_machine_and_the_restart() {
        assert_eq!(
            line(&facts(4, false), "penguin", RESTART, INSTALLED + 60).as_deref(),
            Some(
                "notice: SpaceTerm Core 0.7.2 is installed on penguin. Restart to use it: \
                 spaceterm --machine penguin restart"
            )
        );
    }

    #[test]
    fn once_overdue_the_notice_escalates_with_the_wait_and_the_sessions() {
        assert_eq!(
            line(
                &facts(4, true),
                "penguin",
                RESTART,
                INSTALLED + 3 * 86_400 + 5
            )
            .as_deref(),
            Some(
                "notice: SpaceTerm Core 0.7.2 has waited 3 days for a restart on penguin. \
                 Restarting ends 4 Terminal Sessions: spaceterm --machine penguin restart"
            )
        );
        assert_eq!(
            line(
                &facts(1, true),
                "This Mac",
                RESTART,
                INSTALLED + OVERDUE_AFTER
            )
            .as_deref(),
            Some(
                "notice: SpaceTerm Core 0.7.2 has waited 2 days for a restart on This Mac. \
                 Restarting ends 1 Terminal Session: spaceterm --machine penguin restart"
            )
        );
        assert_eq!(
            line(
                &facts(0, false),
                "penguin",
                RESTART,
                INSTALLED + OVERDUE_AFTER
            )
            .as_deref(),
            Some(
                "notice: SpaceTerm Core 0.7.2 has waited 2 days for a restart on penguin. \
                 Restart to use it: spaceterm --machine penguin restart"
            ),
            "the CLI's clock escalates even before the Core's facts do"
        );
        assert_eq!(
            line(&facts(0, true), "penguin", RESTART, INSTALLED + 86_400 + 5).as_deref(),
            Some(
                "notice: SpaceTerm Core 0.7.2 is installed on penguin. Restart to use it: \
                 spaceterm --machine penguin restart"
            ),
            "an older Core's one-day fact is not yet Overdue"
        );
    }

    #[test]
    fn no_version_waits_so_no_notice() {
        let mut current = facts(4, false);
        current.pending = None;
        assert_eq!(line(&current, "penguin", RESTART, INSTALLED), None);
    }

    #[test]
    fn notices_print_only_in_interactive_text_mode() {
        let text = Mode::default();
        assert!(shown(text, true));
        assert!(!shown(text, false));
        assert!(!shown(
            Mode {
                quiet: true,
                ..text
            },
            true
        ));
        assert!(!shown(Mode { json: true, ..text }, true));
        assert!(shown(
            Mode {
                verbose: true,
                ..text
            },
            true
        ));
    }

    #[test]
    fn a_work_command_names_its_machine_and_repeats_its_actor_in_the_restart() {
        let machine = MachineRef {
            name: "penguin".into(),
            route: "penguin".into(),
            id: "5b".repeat(32),
        };
        let agent: spaceterm_core::protocol::AgentName = "builder".parse().unwrap();
        assert_eq!(
            for_work(
                Some(&facts(0, false)),
                &machine,
                Some(&agent),
                INSTALLED + 60
            )
            .as_deref(),
            Some(
                "notice: SpaceTerm Core 0.7.2 is installed on penguin. Restart to use it: \
                 spaceterm --machine penguin --as builder restart"
            )
        );
        assert_eq!(
            for_work(None, &machine, None, INSTALLED),
            None,
            "a Core that sent no facts"
        );
    }

    #[test]
    fn a_work_output_writes_its_notice_after_the_result_only_where_it_may() {
        let machine = MachineRef {
            name: "penguin".into(),
            route: "penguin".into(),
            id: "5b".repeat(32),
        };
        let mut output = crate::work::output::Output::ok(
            "list",
            machine,
            serde_json::json!({}),
            "Workspaces: none\n".into(),
            &[],
        );
        output.notice = Some("notice: waiting".into());
        let written = |mode: Mode, terminal: bool| {
            let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
            assert_eq!(output.write_to(mode, terminal, &mut stdout, &mut stderr), 0);
            (
                String::from_utf8(stdout).unwrap(),
                String::from_utf8(stderr).unwrap(),
            )
        };
        assert_eq!(
            written(Mode::default(), true),
            ("Workspaces: none\n".into(), "notice: waiting\n".into())
        );
        let json = Mode {
            json: true,
            ..Mode::default()
        };
        assert!(written(json, true).1.is_empty());
        assert!(written(Mode::default(), false).1.is_empty());
    }
}
