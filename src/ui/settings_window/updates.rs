//! The Updates section: SpaceTerm's card, with its row and its own update preferences, then the
//! `SpaceTerm Core` card with one row per Machine. Each row is the one the title-bar list, the
//! Machine card and the menu bar extra show, from [`crate::updates::state`]: one status line and
//! the row's update action, plus a menu of that item's last check, a check of that item alone
//! and its release notes. Check Now beside the page title checks SpaceTerm and each connected
//! Machine. Listing opens no connection (ADR 0039). [`crate::updates`] owns update policy and
//! state.

use gpui::prelude::*;
use gpui::{AnyElement, AnyWindowHandle, App, Context, ElementId, Entity, SharedString, Window};
use spaceterm_ui::{
    AnchoredAlignment, AnchoredPlacement, AnchoredPlacementConfig, Button, ButtonSize,
    ButtonVariant, Icon, IconName, Menu, MenuEntry, SegmentedControl, SegmentedOption, Switch,
    ToggleSize,
};

use super::{SettingsRowId, SettingsWindow};
use crate::settings::SettingsDocument;
use crate::ui::appearance::ChromeAppearance;
use crate::ui::chrome_icons::IconRole;
use crate::ui::sidebar_window::form::{CaptionTone, FormRow};
use crate::updates::copy::settings::{self as copy, ItemEntry};
use crate::updates::machines::store::{MachineUpdates, Surface};
use crate::updates::machines::{Asked, MachineAction, MachineLink, MachineUpdate};
use crate::updates::policy::{CheckInterval, UpdatePreferences};
use crate::updates::state::{
    self, AskedCheck, ItemCommand, RowAction, RowTone, Target, UpdateView,
};
use crate::updates::{ApplicationUpdates, UpdateService, UpdateState};

/// The SpaceTerm row's label: the product, whose version leads its description.
pub(super) const SPACETERM_LABEL: &str = copy::SPACETERM;

/// Check Now, beside the page title.
pub(super) const CHECK_NOW_SELECTOR: &str = "settings-update-check-now";
/// The SpaceTerm row's update action, such as Relaunch to Update or Download.
pub(super) const RESTART_SELECTOR: &str = "settings-update-restart";
/// The SpaceTerm row's menu.
pub(super) const APP_MENU_SELECTOR: &str = "settings-update-status-menu";
/// Each Machine row under `SpaceTerm Core`, numbered in list order.
pub(super) const MACHINE_ROW_SELECTOR: &str = "settings-row-core-update";

/// How the SpaceTerm row reads and what it offers: the shared SpaceTerm row, with or without a
/// desktop update service.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct UpdateStatusPresentation {
    /// "{v}" or "{v} · {status}", as every update surface reads it.
    pub(super) summary: SharedString,
    /// The color the status line reads in.
    pub(super) tone: CaptionTone,
    /// The row's own action, if any.
    pub(super) action: Option<RowAction>,
}

/// The color a row's status line reads in.
fn caption_tone(tone: RowTone) -> CaptionTone {
    match tone {
        RowTone::Plain => CaptionTone::Guidance,
        RowTone::Warning => CaptionTone::Warning,
        RowTone::Failure => CaptionTone::Error,
    }
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
/// isn't connected to is left alone: its own menu offers Connect and Check.
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

/// Whether a check Check Now started still runs: SpaceTerm's own, or a connected Machine's.
fn checking(view: &UpdateView, cx: &App) -> bool {
    let app = view.app.asked_check == Some(AskedCheck::Running)
        || service(cx).is_some_and(|updates| {
            matches!(updates.read(cx).state(), UpdateState::Checking)
                && updates.read(cx).manual_check()
        });
    app || listed(cx)
        .iter()
        .any(|update| update.link == MachineLink::Connected && update.asked == Some(Asked::Check))
}

/// Performs an entry of an item's menu. None of them updates anything.
fn perform_item(command: ItemCommand, window: AnyWindowHandle, cx: &mut App) {
    match command {
        ItemCommand::Check(Target::App) => {
            if let Some(updates) = service(cx) {
                updates.update(cx, |updates, cx| updates.check(true, cx));
            }
        }
        ItemCommand::Check(Target::Machine(machine)) => {
            if let Some(store) = MachineUpdates::global(cx) {
                store.update(cx, |store, cx| {
                    store.perform(machine, MachineAction::CheckAgain, Surface::Settings, cx)
                });
            }
        }
        ItemCommand::WhatsNew => crate::ui::updates::whats_new::open_whats_new_in(window, cx),
        ItemCommand::ReleasePage(version) => {
            cx.open_url(&crate::updates::release_notes::release_url(&version));
        }
    }
}

/// The version this build carries release notes for, if any.
fn embedded_notes() -> Option<&'static str> {
    crate::updates::release_notes::embedded().map(|notes| notes.version())
}

impl SettingsWindow {
    pub(super) fn update_status(&self, cx: &App) -> UpdateStatusPresentation {
        let row = state::current(cx).app_row();
        UpdateStatusPresentation {
            summary: copy::line(row.version.as_deref(), row.status.as_deref()).into(),
            tone: caption_tone(row.tone),
            action: row.action,
        }
    }

    /// Check Now, beside the page title. While a check it started runs, it waits.
    pub(super) fn render_update_check_now(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let view = state::current(cx);
        Button::new(CHECK_NOW_SELECTOR, copy::CHECK_NOW)
            .accessibility_description(copy::CHECK_NOW_DESCRIPTION)
            .tab_stop(true)
            .variant(ButtonVariant::Outline)
            .size(ButtonSize::Regular)
            .disabled(checking(&view, cx))
            .debug_selector(CHECK_NOW_SELECTOR)
            .on_activate(|_, _, cx| check_now(cx))
            .into_any_element()
    }

    /// An item's menu: readable lines first, then its entries.
    fn render_item_menu(
        &self,
        selector: String,
        name: &str,
        entries: Vec<ItemEntry>,
        appearance: &ChromeAppearance,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let mut items = Vec::new();
        for entry in entries {
            if entry.separator_before {
                items.push(MenuEntry::separator());
            }
            let item = MenuEntry::action(entry.label, entry.command.clone())
                .disabled(!entry.enabled)
                .debug_selector(format!("{selector}-{}", entry.selector));
            items.push(if entry.command.is_some() {
                item
            } else {
                item.informational()
            });
        }
        let owner = cx.weak_entity();
        let icon_size = appearance.icons.metrics(IconRole::Control).glyph_size;
        Menu::new(
            ElementId::from(SharedString::from(selector.clone())),
            SharedString::from(copy::item_menu_label(name)),
            items,
        )
        .icon_trigger(move |foreground| {
            Icon::new(IconName::Ellipsis, icon_size, foreground).into_any_element()
        })
        .placement(AnchoredPlacementConfig::new(
            AnchoredPlacement::Bottom,
            AnchoredAlignment::End,
        ))
        .debug_selector(selector)
        .on_activate(move |activation, window, cx| {
            let Some(command) = activation.action().clone() else {
                return;
            };
            let handle = window.window_handle();
            // A sheet the entry opens appears after the menu closes.
            let owner = owner.clone();
            window.defer(cx, move |_, cx| {
                if owner.upgrade().is_some() {
                    perform_item(command, handle, cx);
                }
            });
        })
        .into_any_element()
    }

    /// A row's update action and its menu, side by side.
    fn render_item_controls(
        &self,
        selector: &str,
        action: Option<(RowAction, SharedString)>,
        menu: AnyElement,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let owner = cx.weak_entity();
        let button = action.map(|(action, description)| {
            let command = action.command;
            Button::new(
                ElementId::from(SharedString::from(selector.to_owned())),
                action.label,
            )
            .accessibility_description(description)
            .tab_stop(true)
            .variant(ButtonVariant::Outline)
            .size(ButtonSize::Regular)
            .debug_selector(selector.to_owned())
            .on_activate(move |_, window, cx| {
                let command = command.clone();
                let _ = owner.update(cx, |settings, cx| {
                    settings.perform_update_command(command, window.window_handle(), cx)
                });
            })
        });
        gpui::div()
            .flex()
            .items_center()
            .gap_1()
            .children(button)
            .child(menu)
            .into_any_element()
    }

    /// The SpaceTerm row's controls: its update action, if any, and its menu.
    pub(super) fn render_update_status(
        &mut self,
        appearance: &ChromeAppearance,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let view = state::current(cx);
        let row = view.app_row();
        let summary = SharedString::from(copy::line(row.version.as_deref(), row.status.as_deref()));
        let entries = copy::app_menu(&view.app, &row, embedded_notes(), service(cx).is_some());
        let menu = self.render_item_menu(
            APP_MENU_SELECTOR.to_owned(),
            SPACETERM_LABEL,
            entries,
            appearance,
            cx,
        );
        let controls = self.render_item_controls(
            RESTART_SELECTOR,
            row.action.map(|action| (action, summary)),
            menu,
            cx,
        );
        gpui::div()
            .debug_selector(|| "settings-row-update-status-control".to_owned())
            .child(controls)
            .into_any_element()
    }

    /// Each Machine's update row, local first, then store order.
    #[cfg(test)]
    pub(super) fn update_machine_rows(&self, cx: &App) -> Vec<state::RowView> {
        let view = state::current(cx);
        listed(cx)
            .into_iter()
            .filter_map(|update| view.machine_row(update.machine))
            .collect()
    }

    /// The rows under `SpaceTerm Core`: one per Machine, local first, then store order, each
    /// with one status line, its update action and its menu. Never empty, so the catalog row
    /// keeps a place: with no Machine known it says so.
    pub(super) fn render_update_machine_rows(
        &mut self,
        appearance: &ChromeAppearance,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let view = state::current(cx);
        let machines: Vec<_> = listed(cx)
            .into_iter()
            .filter_map(|update| {
                let row = view.machine_row(update.machine)?;
                Some((view.machine(update.machine)?.clone(), row))
            })
            .collect();
        if machines.is_empty() {
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
        machines
            .into_iter()
            .enumerate()
            .map(|(index, (machine, row))| {
                let selector = if index == 0 {
                    SettingsRowId::CoreUpdates.descriptor().selector.to_owned()
                } else {
                    format!("{MACHINE_ROW_SELECTOR}-{index}")
                };
                let entries = copy::machine_menu(&machine, &row, embedded_notes());
                let menu = self.render_item_menu(
                    format!("{selector}-menu"),
                    &row.name,
                    entries,
                    appearance,
                    cx,
                );
                let action = row
                    .action
                    .clone()
                    .map(|action| (action, row.accessibility.clone()));
                let controls =
                    self.render_item_controls(&format!("{selector}-action"), action, menu, cx);
                let line = copy::line(row.version.as_deref(), row.status.as_deref());
                FormRow::new(SharedString::from(selector), row.name.clone(), controls)
                    .caption(SharedString::from(line), caption_tone(row.tone))
                    .status_line(None)
                    .render(appearance, window, cx)
                    .into_any_element()
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
            copy::CHECK_INTERVAL,
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
