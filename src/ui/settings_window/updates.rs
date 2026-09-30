//! The Updates section: the installed version, the latest check, and update preferences.
//!
//! [`crate::updates`] owns update policy and state. These rows render that state, forward the one
//! next step it offers, and edit the retained [`UpdatePreferences`] through the Settings draft.

use gpui::prelude::*;
use gpui::{AnyElement, App, Context, Entity, SharedString};
use spaceterm_ui::{SegmentedControl, SegmentedOption, Switch, ToggleSize};

use crate::ui::sidebar_window::form::action_button;
use super::{SettingsRowId, SettingsWindow};
use crate::appearance::SettingsDocument;
use crate::updates::policy::{CheckInterval, ReminderInterval, UpdatePreferences};
use crate::updates::{ApplicationUpdates, UpdateError, UpdateService, UpdateState};

/// The status row's label names the running build, so the page answers "which version" first.
pub(super) const CURRENT_VERSION_LABEL: &str = concat!("SpaceTerm ", env!("SPACETERM_VERSION"));

pub(super) const CHECK_NOW_SELECTOR: &str = "settings-update-check-now";
pub(super) const DOWNLOAD_SELECTOR: &str = "settings-update-download";
pub(super) const RESTART_SELECTOR: &str = "settings-update-restart";

/// The one next step the status row offers.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum UpdateStatusAction {
    /// Checks now. Disabled while a check is already running.
    CheckNow {
        enabled: bool,
    },
    Download,
    /// Asks for the install confirmation. Installing happens only after the person confirms.
    RequestInstall,
    /// The install was confirmed and its restart was interrupted.
    FinishInstall,
}

/// The status row's summary and its action for one service state.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct UpdateStatusPresentation {
    pub(super) summary: SharedString,
    pub(super) action: Option<UpdateStatusAction>,
}

impl UpdateStatusPresentation {
    /// `last_check` and `now` are Unix seconds.
    pub(super) fn resolve(state: &UpdateState, last_check: Option<u64>, now: u64) -> Self {
        let check_now = Some(UpdateStatusAction::CheckNow { enabled: true });
        let (summary, action) = match state {
            UpdateState::Unavailable => (UpdateError::Unavailable.to_string(), None),
            UpdateState::Idle => (format!("{}.", last_checked(last_check, now)), check_now),
            UpdateState::Checking => (
                "Checking for updates…".to_owned(),
                Some(UpdateStatusAction::CheckNow { enabled: false }),
            ),
            UpdateState::UpToDate => (
                format!("Up to date. {}.", last_checked(last_check, now)),
                check_now,
            ),
            UpdateState::Available { version } => (
                format!("Version {version} is available."),
                Some(UpdateStatusAction::Download),
            ),
            UpdateState::Downloading {
                version,
                received,
                total,
            } if *total > 0 => {
                let percent = (*received as f64 / *total as f64).clamp(0.0, 1.0) * 100.0;
                (
                    format!("Downloading version {version}… {}%", percent.floor()),
                    None,
                )
            }
            UpdateState::Downloading { version, .. } => {
                (format!("Downloading version {version}…"), None)
            }
            UpdateState::Verifying { version } => (format!("Preparing version {version}…"), None),
            UpdateState::Ready { version } => (
                format!("Version {version} installs when you quit SpaceTerm."),
                Some(UpdateStatusAction::RequestInstall),
            ),
            UpdateState::Installing { version } => (
                format!("Version {version} installs when SpaceTerm restarts."),
                Some(UpdateStatusAction::FinishInstall),
            ),
            UpdateState::Failed { error } => (error.to_string(), check_now),
        };
        Self {
            summary: summary.into(),
            action,
        }
    }
}

/// How long ago the service last completed a check, in the words a status line uses.
///
/// A check stamped after `now` belongs to a clock that moved backwards and reads as just now.
pub(super) fn last_checked(last_check: Option<u64>, now: u64) -> String {
    const MINUTE: u64 = 60;
    const HOUR: u64 = 60 * MINUTE;
    const DAY: u64 = 24 * HOUR;
    let Some(last_check) = last_check else {
        return "Not checked yet".to_owned();
    };
    let plural = |count: u64, unit: &str| {
        if count == 1 {
            format!("1 {unit}")
        } else {
            format!("{count} {unit}s")
        }
    };
    match now.saturating_sub(last_check) {
        age if age < MINUTE => "Last checked just now".to_owned(),
        age if age < HOUR => format!("Last checked {} ago", plural(age / MINUTE, "minute")),
        age if age < DAY => format!("Last checked {} ago", plural(age / HOUR, "hour")),
        age if age < 2 * DAY => "Last checked yesterday".to_owned(),
        age => format!("Last checked {} ago", plural(age / DAY, "day")),
    }
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs())
}

fn service(cx: &App) -> Option<Entity<ApplicationUpdates>> {
    cx.try_global::<UpdateService>()
        .map(|service| service.0.clone())
}

impl SettingsWindow {
    pub(super) fn update_status(&self, cx: &App) -> UpdateStatusPresentation {
        let Some(updates) = service(cx) else {
            return UpdateStatusPresentation::resolve(&UpdateState::Unavailable, None, 0);
        };
        let updates = updates.read(cx);
        UpdateStatusPresentation::resolve(updates.state(), updates.last_check(), unix_now())
    }

    pub(super) fn render_update_status(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let owner = cx.weak_entity();
        let button = self.update_status(cx).action.map(|action| {
            let (selector, label, enabled) = match action {
                UpdateStatusAction::CheckNow { enabled } => {
                    (CHECK_NOW_SELECTOR, "Check Now", enabled)
                }
                UpdateStatusAction::Download => (DOWNLOAD_SELECTOR, "Download", true),
                UpdateStatusAction::RequestInstall => {
                    (RESTART_SELECTOR, "Restart to Install…", true)
                }
                UpdateStatusAction::FinishInstall => (RESTART_SELECTOR, "Restart", true),
            };
            action_button(selector, label, enabled, move |window, cx| {
                let _ = owner.update(cx, |_, cx| perform(action, window, cx));
            })
        });
        gpui::div()
            .debug_selector(|| "settings-row-update-status-control".to_owned())
            .children(button)
            .into_any_element()
    }

    pub(super) fn render_automatic_update_downloads(
        &mut self,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let value = self.editor.document().updates.automatic_downloads;
        let owner = cx.weak_entity();
        Switch::new(
            "settings-automatic-update-downloads",
            "Download updates automatically",
            value,
        )
        .size(ToggleSize::Regular)
        .label_hidden(true)
        .disabled(!self.editor.editable())
        .debug_selector("settings-automatic-update-downloads")
        .on_change(move |change, _, cx| {
            let enabled = change.requested();
            let _ = owner.update(cx, |settings, cx| {
                settings.edit(move |draft| draft.updates.automatic_downloads = enabled, cx);
            });
        })
        .into_any_element()
    }

    pub(super) fn render_update_check_interval(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let current = self.editor.document().updates.check_interval;
        let owner = cx.weak_entity();
        SegmentedControl::new(
            "settings-update-check-interval",
            "Check for updates",
            &current,
            [
                (CheckInterval::Hourly, "Hourly", "hourly"),
                (
                    CheckInterval::EverySixHours,
                    "Every 6 hours",
                    "every-six-hours",
                ),
                (CheckInterval::Daily, "Daily", "daily"),
            ]
            .into_iter()
            .map(|(interval, label, slug)| {
                SegmentedOption::new(interval, label)
                    .debug_selector(format!("settings-update-check-interval-{slug}"))
            })
            .collect(),
        )
        .expect("three check intervals are within the bounded option set")
        .disabled(!self.editor.editable())
        .debug_selector("settings-update-check-interval")
        .on_change(move |change, _, cx| {
            let interval = *change.requested();
            let _ = owner.update(cx, |settings, cx| {
                settings.edit(move |draft| draft.updates.check_interval = interval, cx);
            });
        })
        .into_any_element()
    }

    pub(super) fn render_update_reminder_interval(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let current = self.editor.document().updates.reminder_interval;
        let owner = cx.weak_entity();
        SegmentedControl::new(
            "settings-update-reminder-interval",
            "Remind me about overdue updates",
            &current,
            [
                (ReminderInterval::TwoHours, "2 hours", "two-hours"),
                (ReminderInterval::FourHours, "4 hours", "four-hours"),
                (ReminderInterval::EightHours, "8 hours", "eight-hours"),
                (ReminderInterval::Daily, "Daily", "daily"),
            ]
            .into_iter()
            .map(|(interval, label, slug)| {
                SegmentedOption::new(interval, label)
                    .debug_selector(format!("settings-update-reminder-interval-{slug}"))
            })
            .collect(),
        )
        .expect("four reminder intervals are within the bounded option set")
        .disabled(!self.editor.editable())
        .debug_selector("settings-update-reminder-interval")
        .on_change(move |change, _, cx| {
            let interval = *change.requested();
            let _ = owner.update(cx, |settings, cx| {
                settings.edit(move |draft| draft.updates.reminder_interval = interval, cx);
            });
        })
        .into_any_element()
    }

    /// Whether an update preference row differs from its default. `None` for any other row.
    pub(super) fn update_preference_differs(&self, row: SettingsRowId) -> Option<bool> {
        let mut reset = self.editor.document().updates;
        reset_update_preference(row, &mut reset)?;
        Some(reset != self.editor.document().updates)
    }

    pub(super) fn reset_update_preference(&mut self, row: SettingsRowId, cx: &mut Context<Self>) {
        self.edit(
            move |draft: &mut SettingsDocument| {
                reset_update_preference(row, &mut draft.updates);
            },
            cx,
        );
    }
}

/// Restores one update preference row to its default. `None` for any other row.
fn reset_update_preference(row: SettingsRowId, preferences: &mut UpdatePreferences) -> Option<()> {
    let defaults = UpdatePreferences::default();
    match row {
        SettingsRowId::AutomaticUpdateDownloads => {
            preferences.automatic_downloads = defaults.automatic_downloads;
        }
        SettingsRowId::UpdateCheckInterval => preferences.check_interval = defaults.check_interval,
        SettingsRowId::UpdateReminderInterval => {
            preferences.reminder_interval = defaults.reminder_interval;
        }
        _ => return None,
    }
    Some(())
}

/// One line of guidance for the preference rows.
pub(super) fn update_row_description(row: SettingsRowId) -> Option<&'static str> {
    Some(match row {
        SettingsRowId::AutomaticUpdateDownloads => {
            "Updates download in the background and install when you quit SpaceTerm."
        }
        SettingsRowId::UpdateCheckInterval => "SpaceTerm also checks each time it opens.",
        SettingsRowId::UpdateReminderInterval => {
            "An update becomes overdue two days after its release. A dismissed reminder returns after this interval."
        }
        _ => return None,
    })
}

fn perform(
    action: UpdateStatusAction,
    window: &mut gpui::Window,
    cx: &mut Context<SettingsWindow>,
) {
    let Some(updates) = service(cx) else { return };
    match action {
        // The row reports the result itself, so this check claims no alert.
        UpdateStatusAction::CheckNow { .. } => {
            updates.update(cx, |updates, cx| updates.check(true, cx));
        }
        UpdateStatusAction::Download => updates.update(cx, |updates, cx| updates.download(cx)),
        UpdateStatusAction::RequestInstall => {
            crate::ui::updates::request_install_from(window.window_handle(), cx);
        }
        UpdateStatusAction::FinishInstall => {
            updates.update(cx, |updates, cx| updates.retry_install(cx));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: u64 = 1_000_000;

    fn resolve(state: UpdateState) -> UpdateStatusPresentation {
        UpdateStatusPresentation::resolve(&state, Some(NOW - 5 * 60), NOW)
    }

    #[test]
    fn status_should_offer_the_services_next_step_and_nothing_while_it_works() {
        let version = || "0.4.2".to_owned();
        let cases = [
            (UpdateState::Unavailable, None),
            (
                UpdateState::Idle,
                Some(UpdateStatusAction::CheckNow { enabled: true }),
            ),
            (
                UpdateState::Checking,
                Some(UpdateStatusAction::CheckNow { enabled: false }),
            ),
            (
                UpdateState::UpToDate,
                Some(UpdateStatusAction::CheckNow { enabled: true }),
            ),
            (
                UpdateState::Available { version: version() },
                Some(UpdateStatusAction::Download),
            ),
            (
                UpdateState::Downloading {
                    version: version(),
                    received: 1,
                    total: 2,
                },
                None,
            ),
            (UpdateState::Verifying { version: version() }, None),
            (
                UpdateState::Ready { version: version() },
                Some(UpdateStatusAction::RequestInstall),
            ),
            (
                UpdateState::Installing { version: version() },
                Some(UpdateStatusAction::FinishInstall),
            ),
            (
                UpdateState::Failed {
                    error: UpdateError::Check,
                },
                Some(UpdateStatusAction::CheckNow { enabled: true }),
            ),
        ];
        for (state, action) in cases {
            assert_eq!(resolve(state.clone()).action, action, "{state:?}");
        }
    }

    #[test]
    fn status_should_name_the_last_check_and_the_quit_install() {
        assert_eq!(
            resolve(UpdateState::UpToDate).summary.as_ref(),
            "Up to date. Last checked 5 minutes ago."
        );
        assert_eq!(
            resolve(UpdateState::Ready {
                version: "0.4.2".to_owned()
            })
            .summary
            .as_ref(),
            "Version 0.4.2 installs when you quit SpaceTerm."
        );
        assert_eq!(
            resolve(UpdateState::Downloading {
                version: "0.4.2".to_owned(),
                received: 2,
                total: 3,
            })
            .summary
            .as_ref(),
            "Downloading version 0.4.2… 66%"
        );
    }

    #[test]
    fn last_check_should_read_as_elapsed_time_and_survive_clock_rollback() {
        let cases = [
            (None, "Not checked yet"),
            (Some(NOW), "Last checked just now"),
            (Some(NOW + 3_600), "Last checked just now"),
            (Some(NOW - 60), "Last checked 1 minute ago"),
            (Some(NOW - 2 * 3_600), "Last checked 2 hours ago"),
            (Some(NOW - 30 * 3_600), "Last checked yesterday"),
            (Some(NOW - 3 * 86_400), "Last checked 3 days ago"),
        ];
        for (last_check, expected) in cases {
            assert_eq!(last_checked(last_check, NOW), expected, "{last_check:?}");
        }
    }
}
