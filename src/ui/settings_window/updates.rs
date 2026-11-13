//! The Updates section: the SpaceTerm row, the `SpaceTerm Core` heading with one row per
//! Machine, and update preferences. Every row is the one the title-bar list, the Machine card
//! and the menu bar extra show, from [`crate::updates::state`]. Listing opens no connection
//! (ADR 0039). [`crate::updates`] owns update policy and state.

use gpui::prelude::*;
use gpui::{AnyElement, App, Context, ElementId, Entity, SharedString, Window};
use spaceterm_ui::{
    Button, ButtonSize, ButtonVariant, SegmentedControl, SegmentedOption, Switch, ToggleSize,
};

use super::{SettingsRowId, SettingsWindow};
use crate::settings::SettingsDocument;
use crate::ui::appearance::ChromeAppearance;
use crate::ui::sidebar_window::form::{CaptionTone, FormRow};
use crate::updates::copy::settings as copy;
use crate::updates::machines::store::{MachineUpdates, Surface};
use crate::updates::machines::{MachineAction, MachineLink, MachineUpdate};
use crate::updates::policy::{CheckInterval, UpdatePreferences};
use crate::updates::state::{self, RowAction, RowView};
use crate::updates::{ApplicationUpdates, UpdateService, UpdateState};

/// The SpaceTerm row's label: the product, whose version leads its description.
pub(super) const SPACETERM_LABEL: &str = copy::SPACETERM;

pub(super) const CHECK_NOW_SELECTOR: &str = "settings-update-check-now";
/// The Check Now row's content, which spans the row with the button at its trailing edge.
pub(super) const CHECK_NOW_CONTROL_SELECTOR: &str = "settings-row-update-check-now-control";
/// The SpaceTerm row's action, such as Relaunch to Update or What's New.
pub(super) const RESTART_SELECTOR: &str = "settings-update-restart";
/// Each Machine row under `SpaceTerm Core`, numbered in list order.
pub(super) const MACHINE_ROW_SELECTOR: &str = "settings-row-core-update";

/// How the SpaceTerm row reads and what it offers: the shared SpaceTerm row, with or without a
/// desktop update service.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct UpdateStatusPresentation {
    /// "{v}" or "{v} · {status}", as every update surface reads it.
    pub(super) summary: SharedString,
    /// The row's own action, if any.
    pub(super) action: Option<RowAction>,
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

/// The desktop update service, when this build updates itself.
fn service(cx: &App) -> Option<Entity<ApplicationUpdates>> {
    cx.try_global::<UpdateService>()
        .map(|service| service.0.clone())
        .filter(|service| !matches!(service.read(cx).state(), UpdateState::Unavailable))
}

/// The Machines in list order: local first, then store order.
fn listed(cx: &App) -> Vec<MachineUpdate> {
    let mut machines: Vec<MachineUpdate> = MachineUpdates::global(cx)
        .map(|store| store.read(cx).machines().to_vec())
        .unwrap_or_default();
    machines.sort_by_key(|update| !update.local);
    machines
}

/// Checks for a newer SpaceTerm, when this build updates itself, and asks each connected
/// Machine's SpaceTerm Core to check, over the connections SpaceTerm already holds. A Machine it
/// isn't connected to is left alone.
pub(super) fn check_now(cx: &mut App) {
    if let Some(updates) = service(cx) {
        updates.update(cx, |updates, cx| updates.check(true, cx));
    }
    let Some(store) = MachineUpdates::global(cx) else {
        return;
    };
    let connected: Vec<_> = store
        .read(cx)
        .machines()
        .iter()
        .filter(|update| update.link == MachineLink::Connected)
        .map(|update| update.machine)
        .collect();
    store.update(cx, |store, cx| {
        for machine in connected {
            store.perform(machine, MachineAction::CheckAgain, Surface::Settings, cx);
        }
    });
}

/// Whether SpaceTerm's own check runs now.
fn checking(cx: &App) -> bool {
    service(cx).is_some_and(|updates| matches!(updates.read(cx).state(), UpdateState::Checking))
}

impl SettingsWindow {
    pub(super) fn update_status(&self, cx: &App) -> UpdateStatusPresentation {
        let row = state::current(cx).app_row();
        UpdateStatusPresentation {
            summary: copy::line(row.version.as_deref(), row.status.as_deref()).into(),
            action: row.action,
        }
    }

    /// The SpaceTerm row's control: its action, if any.
    pub(super) fn render_update_status(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let owner = cx.weak_entity();
        let status = self.update_status(cx);
        let button = status.action.map(|action| {
            let command = action.command;
            Button::new(RESTART_SELECTOR, action.label)
                .accessibility_description(status.summary.clone())
                .tab_stop(true)
                .variant(ButtonVariant::Outline)
                .size(ButtonSize::Regular)
                .debug_selector(RESTART_SELECTOR)
                .on_activate(move |_, window, cx| {
                    let command = command.clone();
                    let _ = owner.update(cx, |settings, cx| {
                        settings.perform_update_command(command, window.window_handle(), cx)
                    });
                })
        });
        gpui::div()
            .debug_selector(|| "settings-row-update-status-control".to_owned())
            .flex()
            .gap_2()
            .children(button)
            .into_any_element()
    }

    /// Check Now, after the interval. It is offered with or without a desktop update service,
    /// because it also asks each connected Machine's SpaceTerm Core to check.
    pub(super) fn render_update_check_now(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let last = service(cx)
            .map(|updates| last_checked(updates.read(cx).last_check(), unix_now()))
            .unwrap_or_default();
        gpui::div()
            .debug_selector(|| CHECK_NOW_CONTROL_SELECTOR.to_owned())
            .flex()
            .w_full()
            .justify_end()
            .child(
                Button::new(CHECK_NOW_SELECTOR, copy::CHECK_NOW)
                    .accessibility_description(SharedString::from(format!(
                        "{} {last}",
                        copy::CHECK_NOW_DESCRIPTION
                    )))
                    .tab_stop(true)
                    .variant(ButtonVariant::Outline)
                    .size(ButtonSize::Regular)
                    .disabled(checking(cx))
                    .debug_selector(CHECK_NOW_SELECTOR)
                    .on_activate(|_, _, cx| check_now(cx)),
            )
            .into_any_element()
    }

    /// Each Machine's update row, local first, then store order.
    pub(super) fn update_machine_rows(&self, cx: &App) -> Vec<RowView> {
        let view = state::current(cx);
        listed(cx)
            .into_iter()
            .filter_map(|update| view.machine_row(update.machine))
            .collect()
    }

    /// The rows under `SpaceTerm Core`: one per Machine, local first, then store order, each
    /// with its version and status and its action. Never empty, so the catalog row keeps a
    /// place: with no Machine known it says so.
    pub(super) fn render_update_machine_rows(
        &mut self,
        appearance: &ChromeAppearance,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let rows = self.update_machine_rows(cx);
        if rows.is_empty() {
            return vec![
                FormRow::new(
                    SettingsRowId::CoreUpdates.descriptor().selector,
                    copy::NO_MACHINES,
                    gpui::div().into_any_element(),
                )
                .render(appearance, window, cx)
                .into_any_element(),
            ];
        }
        rows.into_iter()
            .enumerate()
            .map(|(index, row)| {
                let selector = if index == 0 {
                    SettingsRowId::CoreUpdates.descriptor().selector.to_owned()
                } else {
                    format!("{MACHINE_ROW_SELECTOR}-{index}")
                };
                let owner = cx.weak_entity();
                let control = row.action.clone().map(|action| {
                    let command = action.command;
                    Button::new(
                        ElementId::from(SharedString::from(format!("{selector}-action"))),
                        action.label,
                    )
                    .accessibility_description(row.accessibility.clone())
                    .tab_stop(true)
                    .variant(ButtonVariant::Outline)
                    .size(ButtonSize::Regular)
                    .debug_selector(format!("{selector}-action"))
                    .on_activate(move |_, window, cx| {
                        let command = command.clone();
                        let _ = owner.update(cx, |settings, cx| {
                            settings.perform_update_command(command, window.window_handle(), cx)
                        });
                    })
                    .into_any_element()
                });
                let line = copy::line(row.version.as_deref(), row.status.as_deref());
                let mut rendered = FormRow::new(
                    SharedString::from(selector),
                    row.name.clone(),
                    control.unwrap_or_else(|| gpui::div().into_any_element()),
                )
                .description(SharedString::from(line));
                if let Some(description) = row.description {
                    rendered = rendered.message(Some((description, CaptionTone::Guidance)));
                }
                rendered.render(appearance, window, cx).into_any_element()
            })
            .collect()
    }

    pub(super) fn render_automatic_update_downloads(
        &mut self,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let value = self.editor.document().updates.automatic_downloads;
        let owner = cx.weak_entity();
        Switch::new(
            "settings-automatic-update-downloads",
            copy::AUTOMATIC_DOWNLOADS,
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
        SettingsRowId::AutomaticUpdateDownloads => copy::AUTOMATIC_DOWNLOADS_DESCRIPTION,
        SettingsRowId::UpdateCheckInterval => copy::CHECK_INTERVAL_DESCRIPTION,
        SettingsRowId::UpdateCheckNow => copy::CHECK_NOW_DESCRIPTION,
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
