//! The `notice:` line about a Pending Version that SpaceTerm Core has not restarted into yet.
//!
//! A command that reached a Core prints at most one notice on stderr, after its result, about
//! that Core. It prints only in text mode, only when stderr is a terminal, and never with
//! `--quiet` or `--json`. The notice states the update ladder of ADR 0065 as the desktop does:
//! how the Pending Version finishes, and from the Ask stage how long an update has waited and
//! how many Terminal Sessions restarting ends.
use crate::contract::{EXIT_BROKEN_PIPE, MachineRef, Mode};
use spaceterm_core::update::continuation::{Continuation, continuation};
use spaceterm_core::update::facts::{DetailedUpdateFacts, UpdateFacts};
use std::io::Write;

/// The update facts a command shows, with the continuation their Core's engine reported. A
/// Core of work and service 1.2 reports it in its detailed update facts; an older Core reports
/// none, and [`finish`] derives it with [`continuation`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Reported {
    pub facts: UpdateFacts,
    pub continuation: Option<Continuation>,
}

impl Reported {
    /// What a work connection received: [`Self::of`] its detailed update facts and its facts.
    pub fn from_link(link: &spaceterm_core::connection::link::CoreLink) -> Option<Self> {
        Self::of(
            link.detailed_update_facts()
                .map(|detailed| (*detailed).clone()),
            link.update_facts().map(|facts| (*facts).clone()),
        )
    }

    /// From what a Core sent: its detailed update facts when it sends them, which hold its
    /// facts and its continuation as one snapshot, otherwise its facts.
    pub fn of(detailed: Option<DetailedUpdateFacts>, facts: Option<UpdateFacts>) -> Option<Self> {
        match detailed {
            Some(detailed) => Some(Self {
                facts: detailed.facts,
                continuation: detailed.continuation,
            }),
            None => facts.map(|facts| Self {
                facts,
                continuation: None,
            }),
        }
    }
}

/// The continuation `detailed` reports for the Pending Version of `facts`, which another reply
/// carried. None when `detailed` is absent or describes another Pending Version.
pub fn reported_for(
    detailed: Option<&DetailedUpdateFacts>,
    facts: &UpdateFacts,
) -> Option<Continuation> {
    let detailed = detailed?;
    match (&detailed.facts.pending, &facts.pending) {
        (Some(reported), Some(shown)) if reported.version == shown.version => detailed.continuation,
        _ => None,
    }
}

/// ADR 0065: an update asks after it has waited this long, in Unix seconds. The desktop's
/// `src/updates/ladder.rs` holds the same value, and a test keeps the two equal.
pub const ASK_AFTER: u64 = 2 * 86_400;
/// ADR 0065: an update insists after it has waited this long, in Unix seconds. The desktop's
/// `src/updates/ladder.rs` holds the same value, and a test keeps the two equal.
pub const INSIST_AFTER: u64 = 7 * 86_400;

/// The stage of ADR 0065's update ladder.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Stage {
    /// From the download.
    Offer,
    /// After [`ASK_AFTER`].
    Ask,
    /// After [`INSIST_AFTER`].
    Insist,
}

/// The stage of a Pending Version installed at `installed_at` at `now`, Unix seconds, read
/// against this computer's clock as the desktop reads it. A clock behind `installed_at` reads
/// Offer.
pub fn stage(installed_at: u64, now: u64) -> Stage {
    match now.saturating_sub(installed_at) {
        waited if waited >= INSIST_AFTER => Stage::Insist,
        waited if waited >= ASK_AFTER => Stage::Ask,
        _ => Stage::Offer,
    }
}

/// How the CLI says a Pending Version finishes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Finish {
    /// The sentences that follow a sentence naming the downloaded version.
    Downloaded(String),
    /// From the Ask stage: the sentences name the wait, the Machine and the version themselves.
    Waited(String),
}

impl Finish {
    /// The sentences, whichever form they take.
    pub fn sentences(self) -> String {
        match self {
            Self::Downloaded(sentences) | Self::Waited(sentences) => sentences,
        }
    }
}

/// How the Pending Version of `facts` on `machine` finishes at `now`, Unix seconds, ending
/// with `restart`, the command line that restarts SpaceTerm Core. `reported` is the
/// continuation a Core of work and service 1.2 reports, which takes precedence over the one
/// [`continuation`] derives from `facts`. None when no version waits.
pub fn finish(
    facts: &UpdateFacts,
    reported: Option<Continuation>,
    machine: &str,
    restart: &str,
    now: u64,
) -> Option<Finish> {
    let pending = facts.pending.as_ref()?;
    let how = reported.or_else(|| continuation(facts))?;
    let applies = |when: &str| {
        Finish::Downloaded(format!(
            "SpaceTerm Core restarts into it {when}. Restart now with: {restart}"
        ))
    };
    match how {
        Continuation::Soon => return Some(applies("in a moment")),
        Continuation::WhenNothingRuns => {
            return Some(applies("after an hour with nothing running"));
        }
        Continuation::WhenSessionsEnd
        | Continuation::NeedsConfirmation
        | Continuation::StartFailed
        | Continuation::Unknown => {}
    }
    if stage(pending.installed_at, now) >= Stage::Ask {
        let ends = match facts.sessions {
            0 => "No Terminal Session is open.".to_owned(),
            sessions => format!(
                "Restarting into SpaceTerm Core {} ends {}.",
                pending.version,
                crate::admin::lifecycle::terminal_sessions(sessions)
            ),
        };
        return Some(Finish::Waited(format!(
            "An update has waited {} on {machine}. {ends} Restart with: {restart}",
            days(now.saturating_sub(pending.installed_at))
        )));
    }
    Some(match how {
        Continuation::WhenSessionsEnd => applies("when no Terminal Session is open"),
        _ => Finish::Downloaded(match facts.sessions {
            0 => format!("No Terminal Session is open. Restart with: {restart}"),
            sessions => format!(
                "Restarting ends {}. Restart with: {restart}",
                crate::admin::lifecycle::terminal_sessions(sessions)
            ),
        }),
    })
}

/// `waited` seconds in whole days, at least one.
fn days(waited: u64) -> String {
    match (waited / 86_400).max(1) {
        1 => "1 day".to_owned(),
        days => format!("{days} days"),
    }
}

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
/// received, if any, at `now`. Build `reported` with [`Reported::from_link`]. `agent` is the actor the command ran as, so the restart line
/// repeats it.
pub fn for_work(
    reported: Option<&Reported>,
    machine: &MachineRef,
    agent: Option<&spaceterm_core::protocol::AgentName>,
    now: u64,
) -> Option<String> {
    let reported = reported?;
    line(
        &reported.facts,
        reported.continuation,
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
/// command that restarts it. `reported` is as in [`finish`]. None when no version waits.
pub fn line(
    facts: &UpdateFacts,
    reported: Option<Continuation>,
    machine: &str,
    restart: &str,
    now: u64,
) -> Option<String> {
    let version = &facts.pending.as_ref()?.version;
    Some(match finish(facts, reported, machine, restart, now)? {
        Finish::Downloaded(sentences) => {
            format!("notice: SpaceTerm Core {version} is downloaded on {machine}. {sentences}")
        }
        Finish::Waited(sentences) => format!("notice: {sentences}"),
    })
}

/// The desktop's pure ladder module, compiled here so a test compares its thresholds with
/// this module's.
// `unused` rather than `dead_code`, which the file may allow itself.
#[cfg(test)]
#[allow(unused, reason = "only the thresholds are compared")]
#[path = "../../../src/updates/ladder.rs"]
mod desktop_ladder;

#[cfg(test)]
mod tests {
    use super::*;
    use spaceterm_core::update::facts::{
        FailureKind, InstallFailure, OVERDUE_SINCE_AFTER, PendingVersion, Platform, Protocols,
        StartFailure, UpdateCheck, UpdateFailure, Version,
    };

    const INSTALLED: u64 = 1_793_750_400;
    const RESTART: &str = "spaceterm --machine penguin restart";

    fn facts(sessions: u32, breaking: bool) -> UpdateFacts {
        UpdateFacts {
            revision: 1,
            platform: Platform::LinuxX86_64,
            running: Version::parse("0.7.1").unwrap(),
            pending: Some(PendingVersion {
                version: Version::parse("0.7.2").unwrap(),
                protocols: Protocols::CURRENT,
                breaking,
                installed_at: INSTALLED,
                overdue_since: None,
                stranded: Vec::new(),
            }),
            sessions,
            operation: None,
            check: UpdateCheck::Never,
            skipped: None,
            last_failure: None,
        }
    }

    fn failed(sessions: u32, version: &str, kind: FailureKind) -> UpdateFacts {
        UpdateFacts {
            last_failure: Some(UpdateFailure {
                id: 1,
                at: INSTALLED + 30,
                version: Version::parse(version).unwrap(),
                kind,
            }),
            ..facts(sessions, false)
        }
    }

    fn notice(facts: &UpdateFacts, reported: Option<Continuation>, at: u64) -> String {
        line(facts, reported, "penguin", RESTART, at).unwrap()
    }

    #[test]
    fn an_update_that_restarts_by_itself_says_when() {
        assert_eq!(
            notice(&facts(2, false), None, INSTALLED + 60),
            "notice: SpaceTerm Core 0.7.2 is downloaded on penguin. SpaceTerm Core restarts into \
             it when no Terminal Session is open. Restart now with: spaceterm --machine penguin \
             restart"
        );
        assert_eq!(
            notice(&facts(0, false), None, INSTALLED + 60),
            "notice: SpaceTerm Core 0.7.2 is downloaded on penguin. SpaceTerm Core restarts into \
             it in a moment. Restart now with: spaceterm --machine penguin restart"
        );
        assert_eq!(
            notice(&facts(0, false), None, INSTALLED + INSIST_AFTER),
            "notice: SpaceTerm Core 0.7.2 is downloaded on penguin. SpaceTerm Core restarts into \
             it in a moment. Restart now with: spaceterm --machine penguin restart",
            "a restart seconds away needs no wait named"
        );
        assert_eq!(
            notice(
                &facts(2, false),
                Some(Continuation::WhenNothingRuns),
                INSTALLED + ASK_AFTER
            ),
            "notice: SpaceTerm Core 0.7.2 is downloaded on penguin. SpaceTerm Core restarts into \
             it after an hour with nothing running. Restart now with: spaceterm --machine \
             penguin restart",
            "a 1.2 Core's reported continuation takes precedence over the derived one"
        );
    }

    #[test]
    fn an_update_that_waits_for_the_person_says_what_restarting_ends() {
        let at = INSTALLED + 60;
        assert_eq!(
            notice(&facts(2, true), None, at),
            "notice: SpaceTerm Core 0.7.2 is downloaded on penguin. Restarting ends 2 Terminal \
             Sessions. Restart with: spaceterm --machine penguin restart"
        );
        assert_eq!(
            notice(&facts(1, true), None, at),
            "notice: SpaceTerm Core 0.7.2 is downloaded on penguin. Restarting ends 1 Terminal \
             Session. Restart with: spaceterm --machine penguin restart"
        );
        assert_eq!(
            notice(&facts(0, true), None, at),
            "notice: SpaceTerm Core 0.7.2 is downloaded on penguin. No Terminal Session is open. \
             Restart with: spaceterm --machine penguin restart"
        );
        let start = FailureKind::Start(StartFailure::NotReady);
        let install = FailureKind::Install(InstallFailure::Download);
        for (facts, why) in [
            (failed(0, "0.7.2", start), "a start into it failed"),
            (
                failed(0, "0.7.3", install),
                "an install failure hides any earlier start failure",
            ),
        ] {
            assert_eq!(
                notice(&facts, None, at),
                "notice: SpaceTerm Core 0.7.2 is downloaded on penguin. No Terminal Session is \
                 open. Restart with: spaceterm --machine penguin restart",
                "{why}"
            );
        }
        assert_eq!(
            notice(&failed(0, "0.7.1", start), None, at),
            "notice: SpaceTerm Core 0.7.2 is downloaded on penguin. SpaceTerm Core restarts into \
             it in a moment. Restart now with: spaceterm --machine penguin restart",
            "a start failure of another version promises nothing about this one"
        );
    }

    #[test]
    fn from_the_ask_stage_the_notice_names_how_long_an_update_has_waited() {
        assert_eq!(
            notice(&facts(4, false), None, INSTALLED + ASK_AFTER - 1),
            "notice: SpaceTerm Core 0.7.2 is downloaded on penguin. SpaceTerm Core restarts into \
             it when no Terminal Session is open. Restart now with: spaceterm --machine penguin \
             restart"
        );
        assert_eq!(
            notice(&facts(4, false), None, INSTALLED + ASK_AFTER),
            "notice: An update has waited 2 days on penguin. Restarting into SpaceTerm Core 0.7.2 \
             ends 4 Terminal Sessions. Restart with: spaceterm --machine penguin restart"
        );
        assert_eq!(
            notice(&facts(1, true), None, INSTALLED + INSIST_AFTER + 5),
            "notice: An update has waited 7 days on penguin. Restarting into SpaceTerm Core 0.7.2 \
             ends 1 Terminal Session. Restart with: spaceterm --machine penguin restart",
            "Insist adds no words"
        );
        assert_eq!(
            notice(&facts(0, true), None, INSTALLED + 3 * 86_400),
            "notice: An update has waited 3 days on penguin. No Terminal Session is open. Restart \
             with: spaceterm --machine penguin restart"
        );
        let mut older = facts(4, false);
        older.pending.as_mut().unwrap().overdue_since = Some(INSTALLED + OVERDUE_SINCE_AFTER);
        assert_eq!(
            notice(&older, None, INSTALLED + 86_400 + 5),
            "notice: SpaceTerm Core 0.7.2 is downloaded on penguin. SpaceTerm Core restarts into \
             it when no Terminal Session is open. Restart now with: spaceterm --machine penguin \
             restart",
            "the CLI ignores an older Core's one-day overdue_since"
        );
    }

    #[test]
    fn the_stage_follows_the_ladder_thresholds() {
        for (waited, stage_then) in [
            (0, Stage::Offer),
            (ASK_AFTER - 1, Stage::Offer),
            (ASK_AFTER, Stage::Ask),
            (INSIST_AFTER - 1, Stage::Ask),
            (INSIST_AFTER, Stage::Insist),
        ] {
            assert_eq!(stage(INSTALLED, INSTALLED + waited), stage_then, "{waited}");
        }
        assert_eq!(
            stage(INSTALLED, INSTALLED - 60),
            Stage::Offer,
            "a clock behind"
        );
    }

    #[test]
    fn the_ladder_thresholds_equal_the_desktop_ones() {
        assert_eq!(ASK_AFTER, super::desktop_ladder::ASK_AFTER);
        assert_eq!(INSIST_AFTER, super::desktop_ladder::INSIST_AFTER);
    }

    #[test]
    fn no_version_waits_so_no_notice() {
        let mut current = facts(4, false);
        current.pending = None;
        assert_eq!(line(&current, None, "penguin", RESTART, INSTALLED), None);
        assert_eq!(finish(&current, None, "penguin", RESTART, INSTALLED), None);
    }

    fn detailed(facts: UpdateFacts, continuation: Option<Continuation>) -> DetailedUpdateFacts {
        DetailedUpdateFacts {
            facts,
            continuation,
            working: 0,
            restart: None,
        }
    }

    #[test]
    fn a_1_2_cores_detailed_facts_carry_its_continuation_and_an_older_cores_facts_none() {
        let waiting = facts(2, false);
        let newer = Reported::of(
            Some(detailed(
                waiting.clone(),
                Some(Continuation::WhenNothingRuns),
            )),
            Some(facts(5, true)),
        )
        .unwrap();
        assert_eq!(newer.facts, waiting, "one snapshot: the detailed facts win");
        assert_eq!(newer.continuation, Some(Continuation::WhenNothingRuns));
        let older = Reported::of(None, Some(waiting.clone())).unwrap();
        assert_eq!(older.continuation, None);
        assert_eq!(
            notice(&older.facts, older.continuation, INSTALLED + ASK_AFTER),
            "notice: An update has waited 2 days on penguin. Restarting into SpaceTerm Core 0.7.2 \
             ends 2 Terminal Sessions. Restart with: spaceterm --machine penguin restart",
            "an older Core's continuation is derived from its facts"
        );
        assert_eq!(Reported::of(None, None), None);
    }

    #[test]
    fn a_reported_continuation_applies_only_to_the_pending_version_it_describes() {
        let shown = facts(2, false);
        let same = detailed(shown.clone(), Some(Continuation::WhenNothingRuns));
        assert_eq!(
            reported_for(Some(&same), &shown),
            Some(Continuation::WhenNothingRuns)
        );
        let mut other = shown.clone();
        other.pending.as_mut().unwrap().version = Version::parse("0.7.3").unwrap();
        let moved = detailed(other, Some(Continuation::Soon));
        assert_eq!(reported_for(Some(&moved), &shown), None);
        assert_eq!(reported_for(None, &shown), None);
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
                Reported::of(None, Some(facts(2, false))).as_ref(),
                &machine,
                Some(&agent),
                INSTALLED + 60
            )
            .as_deref(),
            Some(
                "notice: SpaceTerm Core 0.7.2 is downloaded on penguin. SpaceTerm Core restarts \
                 into it when no Terminal Session is open. Restart now with: spaceterm --machine \
                 penguin --as builder restart"
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
