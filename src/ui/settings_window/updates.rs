//! The Updates section: the installed version, the latest check, and update preferences.
//! [`crate::updates`] owns update policy and state.

use gpui::prelude::*;
use gpui::{AnyElement, App, Context, Entity, SharedString};
use spaceterm_ui::{
    Button, ButtonSize, ButtonVariant, SegmentedControl, SegmentedOption, Switch, ToggleSize,
};

use super::{SettingsRowId, SettingsWindow};
use crate::application_identity::ApplicationIdentity;
use crate::settings::SettingsDocument;
use crate::updates::policy::{CheckInterval, UpdatePreferences};
use crate::updates::presentation::{self, Command, DesktopState};
use crate::updates::{ApplicationUpdates, UpdateError, UpdateService, UpdateState};

/// The status row's label names the running build, so the page answers "which version" first.
pub(super) const CURRENT_VERSION_LABEL: &str = ApplicationIdentity::current().version_label();

pub(super) const CHECK_NOW_SELECTOR: &str = "settings-update-check-now";
pub(super) const RESTART_SELECTOR: &str = "settings-update-restart";

/// The shared row's next command, or a passive check when nothing needs action.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum UpdateStatusAction {
    CheckNow { enabled: bool },
    Command(Command),
}

#[derive(Clone, Debug, PartialEq)]
pub(super) struct UpdateStatusPresentation {
    pub(super) summary: SharedString,
    pub(super) action: Option<UpdateStatusAction>,
    pub(super) button: SharedString,
    /// Check Now beside the row's own command, after an update and after a failure.
    pub(super) check_now: bool,
}

/// How long ago the service last completed a check. A stamp after `now` reads as just now.
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
        let shared = presentation::current(cx);
        if let Some(row) = shared.desktop_row() {
            let (button, action) = row.button.map_or_else(
                || (SharedString::default(), None),
                |button| {
                    (
                        button.label,
                        Some(UpdateStatusAction::Command(button.command)),
                    )
                },
            );
            // A failure the updater still holds offers only Relaunch, which no check can replace.
            let check_now = matches!(
                shared.desktop,
                DesktopState::WhatsNew { .. } | DesktopState::InstallFailed { held: false, .. }
            );
            return UpdateStatusPresentation {
                summary: row.sentence,
                action,
                button,
                check_now,
            };
        }
        let Some(updates) = service(cx) else {
            return UpdateStatusPresentation {
                summary: UpdateError::Unavailable.to_string().into(),
                action: None,
                button: SharedString::default(),
                check_now: false,
            };
        };
        let updates = updates.read(cx);
        if matches!(updates.state(), UpdateState::Unavailable) {
            return UpdateStatusPresentation {
                summary: UpdateError::Unavailable.to_string().into(),
                action: None,
                button: SharedString::default(),
                check_now: false,
            };
        }
        let checking = matches!(updates.state(), UpdateState::Checking);
        UpdateStatusPresentation {
            summary: if checking {
                "Checking for SpaceTerm updates…".into()
            } else {
                format!(
                    "{} {}.",
                    shared.desktop.sentence(),
                    last_checked(updates.last_check(), unix_now())
                )
                .into()
            },
            action: Some(UpdateStatusAction::CheckNow { enabled: !checking }),
            button: "Check Now".into(),
            check_now: false,
        }
    }

    pub(super) fn render_update_status(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let owner = cx.weak_entity();
        let status = self.update_status(cx);
        let checking = service(cx)
            .is_some_and(|updates| matches!(updates.read(cx).state(), UpdateState::Checking));
        let check_now = status.check_now.then(|| {
            Button::new(CHECK_NOW_SELECTOR, "Check Now")
                .tab_stop(true)
                .variant(ButtonVariant::Outline)
                .size(ButtonSize::Regular)
                .disabled(checking)
                .debug_selector(CHECK_NOW_SELECTOR)
                .on_activate(|_, _, cx| {
                    if let Some(updates) = service(cx) {
                        updates.update(cx, |updates, cx| updates.check(true, cx));
                    }
                })
        });
        let button = status.action.map(|action| {
            let (selector, enabled) = match &action {
                UpdateStatusAction::CheckNow { enabled } => (CHECK_NOW_SELECTOR, *enabled),
                UpdateStatusAction::Command(_) => (RESTART_SELECTOR, true),
            };
            Button::new(selector, status.button)
                .accessibility_description(status.summary.clone())
                .tab_stop(true)
                .variant(ButtonVariant::Outline)
                .size(ButtonSize::Regular)
                .disabled(!enabled)
                .debug_selector(selector)
                .on_activate(move |_, window, cx| {
                    let action = action.clone();
                    let _ = owner.update(cx, |settings, cx| match action {
                        UpdateStatusAction::CheckNow { .. } => {
                            if let Some(updates) = service(cx) {
                                updates.update(cx, |updates, cx| updates.check(true, cx));
                            }
                        }
                        UpdateStatusAction::Command(command) => {
                            settings.perform_update_command(command, window.window_handle(), cx)
                        }
                    });
                })
        });
        gpui::div()
            .debug_selector(|| "settings-row-update-status-control".to_owned())
            .flex()
            .gap_2()
            .children(check_now)
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
        _ => return None,
    }
    Some(())
}

/// One line of guidance for the preference rows.
pub(super) fn update_row_description(row: SettingsRowId) -> Option<&'static str> {
    Some(match row {
        SettingsRowId::AutomaticUpdateDownloads => {
            "Relaunch SpaceTerm to install a downloaded update. Terminal Sessions keep running."
        }
        SettingsRowId::UpdateCheckInterval => "SpaceTerm checks when it opens.",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: u64 = 1_000_000;

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
