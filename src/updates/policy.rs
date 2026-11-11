//! SpaceTerm's own place on the update ladder and its reminders, independent of transport and
//! presentation.

use super::ladder::{self, Stage};
use super::stable_version;
use serde::{Deserialize, Serialize};

pub(crate) const DAY: u64 = 24 * 60 * 60;

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum CheckInterval {
    Hourly,
    EverySixHours,
    #[default]
    Daily,
}
impl CheckInterval {
    pub(crate) fn seconds(self) -> u64 {
        match self {
            Self::Hourly => 3600,
            Self::EverySixHours => 6 * 3600,
            Self::Daily => DAY,
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct UpdatePreferences {
    pub(crate) automatic_downloads: bool,
    pub(crate) check_interval: CheckInterval,
}
impl Default for UpdatePreferences {
    fn default() -> Self {
        Self {
            automatic_downloads: true,
            check_interval: CheckInterval::Daily,
        }
    }
}

/// The stage of the last reminder as `update-history.json` stores it. The three stored names
/// predate the update ladder and keep the file's format; [`Stage`] is the one meaning:
/// `Optional` is Offer, `Warning` is Ask and `Overdue` is Insist.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) enum UpdateStage {
    #[default]
    Optional,
    Warning,
    Overdue,
}
impl From<Stage> for UpdateStage {
    fn from(stage: Stage) -> Self {
        match stage {
            Stage::Offer => Self::Optional,
            Stage::Ask => Self::Warning,
            Stage::Insist => Self::Overdue,
        }
    }
}
impl From<UpdateStage> for Stage {
    fn from(stage: UpdateStage) -> Self {
        match stage {
            UpdateStage::Optional => Self::Offer,
            UpdateStage::Warning => Self::Ask,
            UpdateStage::Overdue => Self::Insist,
        }
    }
}

/// Cached observations are never installation authority. Sparkle revalidates the signed feed
/// and archive. These bounded facts retain the pending version, elapsed time, and reminder cadence.
/// Whether `value` is an `update-history.json` this build reads.
pub(crate) fn readable_history(value: &serde_json::Value) -> bool {
    spaceterm_core::migration::strict::<UpdateHistory>(value)
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct UpdateHistory {
    version: Option<String>,
    latest_version: Option<String>,
    published_at: u64,
    high_water: u64,
    last_reminder: Option<u64>,
    reminder_stage: UpdateStage,
    pub(crate) last_check: Option<u64>,
    /// The newest version whose release notes the person has read. A first install has read its
    /// own.
    notes_read: Option<String>,
    /// The version the last launch ran, so the next launch can tell it came by an update.
    launched: Option<String>,
    /// Finished updates, newest first, at most [`RECENT_LIMIT`].
    recent: Vec<RecentUpdate>,
}

/// The most entries Recent Updates keeps, besides the newest update of SpaceTerm itself.
pub(crate) const RECENT_LIMIT: usize = 4;

/// How Recent Updates names an update of SpaceTerm itself.
const DESKTOP_UPDATED: &str = "SpaceTerm updated to ";

/// One finished update, as Recent Updates lists it.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RecentUpdate {
    /// "SpaceTerm updated to 0.9.0" or "penguin updated to 0.9.0".
    pub(crate) text: String,
    /// Unix seconds.
    pub(crate) at: u64,
}
impl UpdateHistory {
    pub(crate) fn observe(&mut self, installed: &str, version: &str, published_at: u64, now: u64) {
        let Some(version_parts) = stable_version(version) else {
            return;
        };
        let installed_parts = stable_version(installed);
        if installed_parts.is_some_and(|installed| installed >= version_parts) {
            return;
        }
        // Installing any observed release settles its deadline; later releases start a new one.
        let settled = self
            .version
            .as_deref()
            .and_then(stable_version)
            .is_some_and(|previous| installed_parts.is_some_and(|installed| installed >= previous));
        if settled {
            self.clear();
        }
        let publication = if published_at == 0 {
            now
        } else {
            published_at.min(now)
        };
        if self.version.is_none() {
            self.published_at = publication;
        } else {
            self.published_at = self.published_at.min(publication);
        }
        if self.version.is_none() {
            self.version = Some(version.to_owned());
        }
        self.latest_version = Some(version.to_owned());
        self.high_water = self.high_water.max(now);
    }

    pub(crate) fn pending_version(&self, installed: &str) -> Option<&str> {
        let version = self.latest_version.as_deref()?;
        let pending = stable_version(version)?;
        (!stable_version(installed).is_some_and(|installed| installed >= pending))
            .then_some(version)
    }

    /// When the waiting SpaceTerm update started waiting: the earliest publication among the
    /// versions observed since the installed version passed the first one. `None` when nothing
    /// waits.
    pub(crate) fn waiting_since(&self) -> Option<u64> {
        self.version.as_ref().map(|_| self.published_at)
    }

    /// The ladder stage of the waiting SpaceTerm update, Offer when nothing waits. A clock set
    /// back keeps the stage already reached.
    pub(crate) fn stage(&mut self, now: u64) -> Stage {
        self.high_water = self.high_water.max(now);
        match self.waiting_since() {
            Some(since) => ladder::stage(since, self.high_water),
            None => Stage::Offer,
        }
    }

    /// Whether a reminder is due: never at Offer, otherwise by the stage's cadence since the
    /// last one.
    pub(crate) fn reminder_due(&mut self, now: u64) -> bool {
        let stage = self.stage(now);
        ladder::reminder_due(stage, self.last_reminder, self.high_water)
    }

    /// The first time after `now` at which [`Self::stage`] or [`Self::reminder_due`] changes:
    /// the update reaching Ask or Insist, or the stage's cadence passing since the last
    /// reminder. A reminder that is already due changes only when the person sees it, so it adds
    /// no time.
    pub(crate) fn next_change(&mut self, now: u64) -> Option<u64> {
        let stage = self.stage(now);
        let since = self.waiting_since()?;
        // Both rules read the high-water time, which follows the clock once it passes the mark.
        let ahead = |moment: u64| (moment > self.high_water).then_some(moment);
        let reminder = self
            .last_reminder
            .zip(ladder::cadence(stage))
            .and_then(|(last, cadence)| ahead(last.saturating_add(cadence)));
        [
            ahead(since.saturating_add(ladder::ASK_AFTER)),
            ahead(since.saturating_add(ladder::INSIST_AFTER)),
            reminder,
        ]
        .into_iter()
        .flatten()
        .min()
    }

    pub(crate) fn reminded(&mut self, now: u64) {
        self.reminder_stage = self.stage(now).into();
        self.last_reminder = Some(self.high_water);
    }

    /// Forgets the pending release. The last check, the notes read, the last launch and Recent
    /// stay.
    pub(crate) fn clear(&mut self) {
        *self = Self {
            last_check: self.last_check,
            notes_read: self.notes_read.take(),
            launched: self.launched.take(),
            recent: std::mem::take(&mut self.recent),
            ..Self::default()
        };
    }

    /// Records that this launch runs `installed` at `now`, and returns whether it is the first
    /// run on a newer version than the last launch, which an update brought. The first launch
    /// ever has nothing new to read. A launch that came by an update is listed in Recent.
    pub(crate) fn launched(&mut self, installed: &str, now: u64) -> bool {
        self.notes_read.get_or_insert_with(|| installed.to_owned());
        let updated = self
            .launched
            .as_deref()
            .is_some_and(|previous| newer(installed, previous));
        if updated {
            self.record(format!("{DESKTOP_UPDATED}{installed}"), now);
        }
        self.launched = Some(installed.to_owned());
        updated
    }

    /// Adds a finished update to Recent. One release updates SpaceTerm and then each Machine, so
    /// the newest update of SpaceTerm itself stays while the Machines' updates fill the rest.
    pub(crate) fn record(&mut self, text: impl Into<String>, at: u64) {
        self.recent.insert(
            0,
            RecentUpdate {
                text: text.into(),
                at,
            },
        );
        let desktop = self
            .recent
            .iter()
            .position(|entry| entry.text.starts_with(DESKTOP_UPDATED));
        let mut machines = 0;
        let mut index = 0;
        self.recent.retain(|_| {
            let keep = Some(index) == desktop || {
                machines += 1;
                machines <= RECENT_LIMIT
            };
            index += 1;
            keep
        });
    }

    /// Newest first.
    pub(crate) fn recent(&self) -> &[RecentUpdate] {
        &self.recent
    }

    /// Whether `installed` is newer than every version whose notes the person has opened.
    pub(crate) fn notes_unread(&self, installed: &str) -> bool {
        self.notes_read
            .as_deref()
            .is_some_and(|read| newer(installed, read))
    }

    /// Records that the person opened the notes of `installed`. Older notes read later leave the
    /// newer record in place.
    pub(crate) fn read_notes(&mut self, installed: &str) {
        if self
            .notes_read
            .as_deref()
            .is_none_or(|read| newer(installed, read))
        {
            self.notes_read = Some(installed.to_owned());
        }
    }
}

/// Whether `version` follows `than`. Development versions have no order, so any other one is newer.
fn newer(version: &str, than: &str) -> bool {
    match (stable_version(version), stable_version(than)) {
        (Some(version), Some(than)) => version > than,
        _ => version != than,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notes_wait_for_the_person_after_an_update_but_not_after_a_first_install() {
        let mut history = UpdateHistory::default();
        history.launched("0.8.0", 0);
        assert!(
            !history.notes_unread("0.8.0"),
            "a first install has nothing new"
        );
        history.observe("0.8.0", "0.9.0", 1_000, 1_000);
        let bytes = serde_json::to_vec(&history).unwrap();
        let mut updated: UpdateHistory = serde_json::from_slice(&bytes).unwrap();
        updated.launched("0.9.0", 0);
        assert!(updated.notes_unread("0.9.0"));
        updated.observe("0.9.0", "0.9.1", 2_000, 2_000);
        updated.clear();
        assert!(
            updated.notes_unread("0.9.0"),
            "settling or clearing the pending release keeps the notes waiting"
        );
        updated.read_notes("0.9.0");
        updated.launched("0.9.0", 0);
        assert!(!updated.notes_unread("0.9.0"));
    }

    #[test]
    fn recent_keeps_the_newest_finished_updates_across_relaunch() {
        let mut history = UpdateHistory::default();
        history.launched("0.8.0", 100);
        assert!(
            history.recent().is_empty(),
            "a first install updated nothing"
        );
        history.launched("0.8.0", 200);
        assert!(
            history.recent().is_empty(),
            "relaunching the same version updated nothing"
        );
        history.record("penguin updated to 0.9.0", 300);
        history.launched("0.9.0", 400);
        let bytes = serde_json::to_vec(&history).unwrap();
        let mut relaunched: UpdateHistory = serde_json::from_slice(&bytes).unwrap();
        relaunched.clear();
        let texts = |history: &UpdateHistory| -> Vec<(String, u64)> {
            history
                .recent()
                .iter()
                .map(|entry| (entry.text.clone(), entry.at))
                .collect()
        };
        assert_eq!(
            texts(&relaunched),
            [
                ("SpaceTerm updated to 0.9.0".to_owned(), 400),
                ("penguin updated to 0.9.0".to_owned(), 300),
            ]
        );
        // The Machines' updates of one release fill Recent, and SpaceTerm's own update stays.
        for at in 0..RECENT_LIMIT as u64 {
            relaunched.record(format!("build-box updated to 0.9.{at}"), 500 + at);
        }
        assert_eq!(relaunched.recent().len(), RECENT_LIMIT + 1);
        assert_eq!(relaunched.recent()[0].text, "build-box updated to 0.9.3");
        assert_eq!(
            relaunched.recent()[RECENT_LIMIT].text,
            "SpaceTerm updated to 0.9.0"
        );
        relaunched.launched("0.9.1", 550);
        relaunched.record("penguin updated to 0.9.1", 560);
        assert_eq!(
            texts(&relaunched)
                .iter()
                .filter(|(text, _)| text.starts_with("SpaceTerm"))
                .count(),
            1,
            "only the newest update of SpaceTerm stays beyond the limit"
        );
        assert_eq!(relaunched.recent().len(), RECENT_LIMIT + 1);
        // Going back is no update.
        relaunched.launched("0.8.0", 600);
        assert_eq!(relaunched.recent()[0].text, "penguin updated to 0.9.1");
    }

    #[test]
    fn only_the_first_run_on_a_newer_version_came_by_an_update() {
        let mut history = UpdateHistory::default();
        assert!(!history.launched("0.8.0", 0), "a first install");
        assert!(!history.launched("0.8.0", 1), "the same version again");
        assert!(history.launched("0.9.0", 2));
        assert!(!history.launched("0.9.0", 3), "a later run of it");
        assert!(!history.launched("0.8.0", 4), "going back");
    }

    #[test]
    fn going_back_to_an_older_version_revives_no_notes() {
        let mut history = UpdateHistory::default();
        history.launched("0.8.0", 0);
        history.launched("0.9.0", 0);
        history.read_notes("0.9.0");
        history.launched("0.8.0", 0);
        assert!(
            !history.notes_unread("0.8.0"),
            "0.8.0 is older than the notes read"
        );
        history.read_notes("0.8.0");
        history.launched("0.9.0", 0);
        assert!(
            !history.notes_unread("0.9.0"),
            "opening older notes keeps newer ones read"
        );
        history.launched("0.10.0", 0);
        assert!(history.notes_unread("0.10.0"));
    }

    #[test]
    fn the_clock_inherits_across_newer_releases_and_restarts_and_resets_once_installed() {
        use ladder::{ASK_AFTER, INSIST_AFTER};
        let mut history = UpdateHistory::default();
        assert_eq!(history.stage(1_000), Stage::Offer, "nothing waits");
        history.observe("0.1.0", "0.1.1", 1_000, 1_000);
        assert_eq!(history.stage(1_000 + ASK_AFTER - 1), Stage::Offer);
        assert_eq!(history.stage(1_000 + ASK_AFTER), Stage::Ask);
        // A newer release replaces the waiting one and keeps its clock, across a relaunch.
        history.observe("0.1.0", "0.1.2", 1_000 + ASK_AFTER, 1_000 + ASK_AFTER);
        let bytes = serde_json::to_vec(&history).unwrap();
        let mut restored: UpdateHistory = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(restored.waiting_since(), Some(1_000));
        assert_eq!(restored.stage(1_000 + INSIST_AFTER - 1), Stage::Ask);
        assert_eq!(restored.stage(1_000 + INSIST_AFTER), Stage::Insist);
        assert_eq!(
            restored.stage(2_000),
            Stage::Insist,
            "a clock set back keeps the stage reached"
        );
        assert_eq!(restored.pending_version("0.1.0"), Some("0.1.2"));
        assert_eq!(restored.pending_version("0.1.2"), None);
        // Installing the first waiting version settles the clock; the next release starts its own.
        let later = 1_000 + INSIST_AFTER + 10;
        restored.observe("0.1.2", "0.1.3", later, later);
        assert_eq!(restored.waiting_since(), Some(later));
        assert_eq!(restored.stage(later), Stage::Offer);
    }

    #[test]
    fn the_stored_stage_names_map_to_the_ladder_one_to_one() {
        for stage in [Stage::Offer, Stage::Ask, Stage::Insist] {
            assert_eq!(Stage::from(UpdateStage::from(stage)), stage);
        }
        let mut history = UpdateHistory::default();
        history.observe("0.1.0", "0.1.1", 1_000, 1_000);
        history.reminded(1_000 + ladder::ASK_AFTER);
        let stored = serde_json::to_value(&history).unwrap();
        assert_eq!(stored["reminder_stage"], "Warning");
        assert!(readable_history(&stored));
    }

    /// Each moment `next_change` names is the first at which the stage or a due reminder
    /// differs, so a scheduler that waits for it misses nothing and wakes for nothing.
    #[test]
    fn the_next_change_is_the_first_moment_the_stage_or_a_reminder_changes() {
        use ladder::{ASK_AFTER, ASK_CADENCE, INSIST_AFTER, INSIST_CADENCE};
        let mut history = UpdateHistory::default();
        assert_eq!(history.next_change(1_000), None, "no release, no change");
        history.observe("0.1.0", "0.1.1", 1_000, 1_000);
        let ask = 1_000 + ASK_AFTER;
        assert_eq!(history.next_change(1_001), Some(ask));
        assert!(!history.clone().reminder_due(ask - 1));
        assert!(history.clone().reminder_due(ask));
        assert_eq!(
            history.next_change(ask),
            Some(1_000 + INSIST_AFTER),
            "a due reminder waits for the person, not for time"
        );
        history.reminded(ask);
        let again = ask + ASK_CADENCE;
        assert_eq!(history.next_change(ask), Some(again));
        assert!(!history.clone().reminder_due(again - 1));
        assert!(history.clone().reminder_due(again));
        // A clock set back changes nothing until it passes the high-water time again.
        assert_eq!(history.next_change(10), Some(again));

        let insist = 1_000 + INSIST_AFTER;
        history.reminded(insist);
        assert_eq!(history.next_change(insist), Some(insist + INSIST_CADENCE));
        assert_eq!(
            history.next_change(insist + INSIST_CADENCE),
            None,
            "Insist is the last stage"
        );
    }

    #[test]
    fn reminders_start_at_ask_repeat_daily_and_every_four_hours_from_insist() {
        use ladder::{ASK_AFTER, ASK_CADENCE, INSIST_AFTER, INSIST_CADENCE};
        let mut history = UpdateHistory::default();
        history.observe("0.1.0", "0.1.1", 1_000, 1_000);
        assert!(!history.reminder_due(1_001));
        assert!(
            !history.reminder_due(1_000 + ASK_AFTER - 1),
            "Offer sends no reminder"
        );
        let ask = 1_000 + ASK_AFTER;
        assert!(history.reminder_due(ask));
        history.reminded(ask);
        assert!(!history.reminder_due(ask + ASK_CADENCE - 1));
        assert!(history.reminder_due(ask + ASK_CADENCE));

        let insist = 1_000 + INSIST_AFTER;
        history.reminded(insist);
        assert!(!history.reminder_due(insist + INSIST_CADENCE - 1));
        assert!(history.reminder_due(insist + INSIST_CADENCE));
    }
}
