//! The `notice:` line about a SpaceTerm Core version that waits for a restart.
//!
//! A command that reached a Core prints at most one notice on stderr, after its result, about
//! that Core. It prints only in text mode, only when stderr is a terminal, and never with
//! `--quiet` or `--json`. After one day the notice escalates: it says how long the version has
//! waited and how many Terminal Sessions a restart ends.
use crate::contract::Mode;
use spaceterm_core::update::facts::{OVERDUE_AFTER, UpdateFacts};

/// Whether a notice prints in this output mode.
pub fn shown(mode: Mode, stderr_terminal: bool) -> bool {
    !mode.json && !mode.quiet && stderr_terminal
}

/// The notice line for `facts` of the Core on `machine` at `now`, Unix seconds, with the
/// command that restarts it. None when no version waits.
pub fn line(facts: &UpdateFacts, machine: &str, restart: &str, now: u64) -> Option<String> {
    let pending = facts.pending.as_ref()?;
    let version = &pending.version;
    let waited = now.saturating_sub(pending.installed_at);
    let overdue = pending.overdue_since.is_some() || waited >= OVERDUE_AFTER;
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
        PendingVersion, Platform, Protocols, UpdateCheck, Version,
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
                overdue_since: overdue.then_some(INSTALLED + OVERDUE_AFTER),
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
    fn after_one_day_the_notice_escalates_with_the_wait_and_the_sessions() {
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
                "notice: SpaceTerm Core 0.7.2 has waited 1 day for a restart on This Mac. \
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
                "notice: SpaceTerm Core 0.7.2 has waited 1 day for a restart on penguin. \
                 Restart to use it: spaceterm --machine penguin restart"
            ),
            "the CLI's clock escalates even before the Core's facts do"
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
}
