//! The update ladder: the stages an update climbs by how long it has waited, for SpaceTerm and
//! every SpaceTerm Core alike. Each stage adds presence, emphasis or reminder cadence, never words.
//!
//! Times are Unix seconds, like `PendingVersion.installed_at` and `UpdateHistory.published_at`.
//! SpaceTerm's clock is `UpdateHistory.published_at`; a SpaceTerm Core's clock is
//! `PendingVersion.installed_at`. Both keep the earliest time across a replacing version. The
//! desktop ignores `overdue_since`.

/// How long an update waits at Offer before it asks.
pub(crate) const ASK_AFTER: u64 = 2 * 86_400;
/// How long an update waits before it insists.
pub(crate) const INSIST_AFTER: u64 = 7 * 86_400;
/// The time between reminders at Ask.
pub(crate) const ASK_CADENCE: u64 = 86_400;
/// The time between reminders at Insist.
pub(crate) const INSIST_CADENCE: u64 = 4 * 3_600;
/// How long Cancel in a confirmation hides an Ask step on this desktop.
pub(crate) const ASK_DEFERRAL: u64 = 86_400;
/// How long nothing may happen before an update from Ask applies itself.
pub(crate) const IDLE_PERIOD: u64 = 3_600;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Stage {
    /// From the download.
    Offer,
    /// After [`ASK_AFTER`].
    Ask,
    /// After [`INSIST_AFTER`].
    Insist,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Emphasis {
    Plain,
    Accent,
    Warning,
}

/// The stage of an update that has waited since `waiting_since`. A clock behind `waiting_since`
/// reads as Offer.
pub(crate) fn stage(waiting_since: u64, now: u64) -> Stage {
    match now.saturating_sub(waiting_since) {
        age if age >= INSIST_AFTER => Stage::Insist,
        age if age >= ASK_AFTER => Stage::Ask,
        _ => Stage::Offer,
    }
}

/// The emphasis of an action step: accent at Offer and Ask, warning at Insist or for a hard stop
/// (a protocol mismatch), which sits above the ladder.
pub(crate) fn emphasis(stage: Stage, hard_stop: bool) -> Emphasis {
    if hard_stop || stage == Stage::Insist {
        Emphasis::Warning
    } else {
        Emphasis::Accent
    }
}

/// The time between reminders at `stage`, or `None` at Offer, which sends none.
pub(crate) fn cadence(stage: Stage) -> Option<u64> {
    match stage {
        Stage::Offer => None,
        Stage::Ask => Some(ASK_CADENCE),
        Stage::Insist => Some(INSIST_CADENCE),
    }
}

/// Whether a reminder is due at `now`: never at Offer; otherwise when none was sent for this
/// update or the stage's cadence has passed since the last one. A clock behind the last reminder
/// waits until it passes it again.
pub(crate) fn reminder_due(stage: Stage, last_reminder: Option<u64>, now: u64) -> bool {
    let Some(cadence) = cadence(stage) else {
        return false;
    };
    last_reminder.is_none_or(|last| now >= last.saturating_add(cadence))
}

/// Whether Cancel at `deferred_at` still hides the step at `now`. Only an Ask step hides, and for
/// exactly [`ASK_DEFERRAL`]; an Insist step never hides.
pub(crate) fn deferral_hides(stage: Stage, deferred_at: Option<u64>, now: u64) -> bool {
    stage == Stage::Ask && deferred_at.is_some_and(|at| now < at.saturating_add(ASK_DEFERRAL))
}

#[cfg(test)]
mod tests {
    use super::*;

    const START: u64 = 1_700_000_000;

    #[test]
    fn an_update_asks_after_two_days_and_insists_after_seven() {
        assert_eq!(stage(START, START), Stage::Offer);
        assert_eq!(stage(START, START + ASK_AFTER - 1), Stage::Offer);
        assert_eq!(stage(START, START + ASK_AFTER), Stage::Ask);
        assert_eq!(stage(START, START + INSIST_AFTER - 1), Stage::Ask);
        assert_eq!(stage(START, START + INSIST_AFTER), Stage::Insist);
        assert_eq!(stage(START, START + 365 * 86_400), Stage::Insist);
        assert_eq!(stage(START, START - 10), Stage::Offer, "a clock set back");
        assert!(Stage::Offer < Stage::Ask && Stage::Ask < Stage::Insist);
    }

    #[test]
    fn emphasis_is_accent_until_insist_and_warning_for_a_hard_stop() {
        assert_eq!(emphasis(Stage::Offer, false), Emphasis::Accent);
        assert_eq!(emphasis(Stage::Ask, false), Emphasis::Accent);
        assert_eq!(emphasis(Stage::Insist, false), Emphasis::Warning);
        for stage in [Stage::Offer, Stage::Ask, Stage::Insist] {
            assert_eq!(emphasis(stage, true), Emphasis::Warning);
        }
    }

    #[test]
    fn reminders_never_come_at_offer_daily_at_ask_and_every_four_hours_at_insist() {
        assert!(!reminder_due(Stage::Offer, None, START));
        assert!(!reminder_due(Stage::Offer, Some(0), START));

        assert!(
            reminder_due(Stage::Ask, None, START),
            "the first one is due at once"
        );
        assert!(!reminder_due(
            Stage::Ask,
            Some(START),
            START + ASK_CADENCE - 1
        ));
        assert!(reminder_due(Stage::Ask, Some(START), START + ASK_CADENCE));

        assert!(reminder_due(Stage::Insist, None, START));
        assert!(!reminder_due(
            Stage::Insist,
            Some(START),
            START + INSIST_CADENCE - 1
        ));
        assert!(reminder_due(
            Stage::Insist,
            Some(START),
            START + INSIST_CADENCE
        ));

        assert!(
            !reminder_due(Stage::Insist, Some(START), START - 86_400),
            "a clock set back waits until it passes the last reminder"
        );
    }

    #[test]
    fn cancel_hides_an_ask_step_for_exactly_one_day_and_never_an_insist_step() {
        assert!(!deferral_hides(Stage::Ask, None, START));
        assert!(deferral_hides(Stage::Ask, Some(START), START));
        assert!(deferral_hides(
            Stage::Ask,
            Some(START),
            START + ASK_DEFERRAL - 1
        ));
        assert!(!deferral_hides(
            Stage::Ask,
            Some(START),
            START + ASK_DEFERRAL
        ));
        assert!(!deferral_hides(Stage::Offer, Some(START), START));
        assert!(!deferral_hides(Stage::Insist, Some(START), START));
    }
}
