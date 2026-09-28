//! Release age and reminders, independent of transport and presentation.

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

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ReminderInterval {
    #[default]
    TwoHours,
    FourHours,
    EightHours,
    Daily,
}
impl ReminderInterval {
    pub(crate) fn seconds(self) -> u64 {
        match self {
            Self::TwoHours => 7200,
            Self::FourHours => 4 * 3600,
            Self::EightHours => 8 * 3600,
            Self::Daily => DAY,
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct UpdatePreferences {
    pub(crate) automatic_downloads: bool,
    pub(crate) check_interval: CheckInterval,
    pub(crate) reminder_interval: ReminderInterval,
}
impl Default for UpdatePreferences {
    fn default() -> Self {
        Self {
            automatic_downloads: true,
            check_interval: CheckInterval::Daily,
            reminder_interval: ReminderInterval::TwoHours,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) enum UpdateStage {
    #[default]
    Optional,
    Warning,
    Overdue,
}

/// Cached observations are never installation authority. Sparkle revalidates the signed feed
/// and archive. These bounded facts retain the pending version, elapsed time, and reminder cadence.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct UpdateHistory {
    version: Option<String>,
    latest_version: Option<String>,
    published_at: u64,
    high_water: u64,
    last_reminder: Option<u64>,
    reminder_stage: UpdateStage,
    pub(crate) last_check: Option<u64>,
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
            *self = Self {
                last_check: self.last_check,
                ..Self::default()
            };
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

    pub(crate) fn stage(&mut self, now: u64) -> UpdateStage {
        self.high_water = self.high_water.max(now);
        if self.version.is_none() {
            return UpdateStage::Optional;
        }
        match self.high_water.saturating_sub(self.published_at) {
            age if age >= 2 * DAY => UpdateStage::Overdue,
            age if age >= DAY => UpdateStage::Warning,
            _ => UpdateStage::Optional,
        }
    }

    pub(crate) fn reminder_due(&mut self, now: u64, interval: ReminderInterval) -> bool {
        let stage = self.stage(now);
        if stage == UpdateStage::Optional {
            return false;
        }
        if stage != self.reminder_stage {
            return true;
        }
        let interval = if stage == UpdateStage::Warning {
            DAY
        } else {
            interval.seconds()
        };
        self.last_reminder
            .is_none_or(|last| self.high_water.saturating_sub(last) >= interval)
    }

    pub(crate) fn reminded(&mut self, now: u64) {
        self.reminder_stage = self.stage(now);
        self.last_reminder = Some(self.high_water);
    }

    pub(crate) fn clear(&mut self) {
        *self = Self {
            last_check: self.last_check,
            ..Self::default()
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deadlines_survive_newer_releases_restarts_and_clock_rollback() {
        let mut history = UpdateHistory::default();
        history.observe("0.1.0", "0.1.1", 1_000, 1_000);
        assert_eq!(history.stage(87_399), UpdateStage::Optional);
        assert_eq!(history.stage(87_400), UpdateStage::Warning);
        assert_eq!(history.stage(173_800), UpdateStage::Overdue);
        history.observe("0.1.0", "0.1.2", 180_000, 180_000);
        let bytes = serde_json::to_vec(&history).unwrap();
        let mut restored: UpdateHistory = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(restored.stage(2_000), UpdateStage::Overdue);
        assert_eq!(restored.pending_version("0.1.0"), Some("0.1.2"));
        assert_eq!(restored.pending_version("0.1.2"), None);
        restored.observe("0.1.2", "0.1.3", 190_000, 190_000);
        assert_eq!(restored.stage(190_000), UpdateStage::Optional);
    }

    #[test]
    fn reminders_escalate_once_and_respect_the_retained_interval() {
        let mut history = UpdateHistory::default();
        history.observe("0.1.0", "0.1.1", 1_000, 1_000);
        assert!(!history.reminder_due(1_001, ReminderInterval::TwoHours));
        assert!(history.reminder_due(87_400, ReminderInterval::TwoHours));
        history.reminded(87_400);
        assert!(!history.reminder_due(100_000, ReminderInterval::TwoHours));
        assert!(history.reminder_due(173_800, ReminderInterval::TwoHours));
        history.reminded(173_800);
        assert!(!history.reminder_due(180_999, ReminderInterval::TwoHours));
        assert!(history.reminder_due(181_000, ReminderInterval::TwoHours));
    }
}
