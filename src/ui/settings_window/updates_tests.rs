//! The Updates section driven through its rendered controls.

use std::rc::Rc;

use gpui::{Entity, Modifiers, TestAppContext, VisualTestContext};

use crate::appearance::{Appearance, SettingsDocument};
use crate::platform::appearance::testing::RecordingAppearancePlatform;
use crate::platform::window_movement::RecordingOperatingSystemWindowDragPlatform;
use crate::ui::appearance_runtime;
use crate::updates::policy::{CheckInterval, ReminderInterval, UpdatePreferences};

use super::test_support::MemoryStorage;
use super::updates::{CHECK_NOW_SELECTOR, DOWNLOAD_SELECTOR, RESTART_SELECTOR};
use super::{SettingsSectionId, SettingsWindow};

fn open_updates(cx: &mut TestAppContext) -> (Entity<SettingsWindow>, &mut VisualTestContext) {
    let (settings, changed) = crate::settings::UserSettings::load(MemoryStorage::with_document(
        &SettingsDocument::default(),
    ));
    let platform = RecordingAppearancePlatform::default();
    platform.set_system_appearance(Some(Appearance::Dark));
    cx.update(|cx| {
        appearance_runtime::install(settings, changed, Rc::new(platform), cx)
            .expect("appearance runtime should install");
        crate::ui::init(cx).expect("UI initialization should succeed");
    });
    let (window, cx) = cx.add_window_view(|window, cx| {
        SettingsWindow::new_with_capabilities(
            Rc::new(RecordingOperatingSystemWindowDragPlatform::default()),
            None,
            None,
            window,
            cx,
        )
    });
    cx.update(|window, _| window.activate_window());
    cx.run_until_parked();
    click("settings-navigation-settings-section-updates", cx);
    (window, cx)
}

fn click(selector: &'static str, cx: &mut VisualTestContext) {
    let position = cx
        .debug_bounds(selector)
        .unwrap_or_else(|| panic!("{selector} was not rendered"))
        .center();
    cx.simulate_mouse_move(position, None, Modifiers::none());
    cx.simulate_click(position, Modifiers::none());
    cx.run_until_parked();
}

fn preferences(window: &Entity<SettingsWindow>, cx: &mut VisualTestContext) -> UpdatePreferences {
    window.read_with(cx, |settings, _| settings.editor.document().updates)
}

#[gpui::test]
fn updates_section_should_default_to_quiet_automatic_downloads(cx: &mut TestAppContext) {
    let (window, cx) = open_updates(cx);

    assert_eq!(
        window.read_with(cx, |settings, _| settings.active_section),
        SettingsSectionId::Updates
    );
    for selector in [
        "settings-row-update-status",
        "settings-row-automatic-update-downloads",
        "settings-row-update-check-interval",
        "settings-row-update-reminder-interval",
    ] {
        assert!(
            cx.debug_bounds(selector).is_some(),
            "{selector} should render"
        );
    }
    assert_eq!(preferences(&window, cx), UpdatePreferences::default());
    assert!(preferences(&window, cx).automatic_downloads);
    // Defaults offer nothing to reset.
    assert!(
        cx.debug_bounds("settings-row-automatic-update-downloads-reset")
            .is_none()
    );
}

#[gpui::test]
fn update_preferences_should_edit_the_draft_and_reset_one_row_at_a_time(cx: &mut TestAppContext) {
    let (window, cx) = open_updates(cx);

    click("settings-automatic-update-downloads", cx);
    click("settings-update-check-interval-hourly", cx);
    click("settings-update-reminder-interval-eight-hours", cx);
    assert_eq!(
        preferences(&window, cx),
        UpdatePreferences {
            automatic_downloads: false,
            check_interval: CheckInterval::Hourly,
            reminder_interval: ReminderInterval::EightHours,
        }
    );

    click("settings-row-update-check-interval-reset", cx);
    assert_eq!(
        preferences(&window, cx),
        UpdatePreferences {
            automatic_downloads: false,
            check_interval: CheckInterval::Daily,
            reminder_interval: ReminderInterval::EightHours,
        }
    );
    click("settings-row-automatic-update-downloads-reset", cx);
    click("settings-row-update-reminder-interval-reset", cx);
    assert_eq!(preferences(&window, cx), UpdatePreferences::default());
}

#[gpui::test]
fn settings_search_should_find_the_overdue_reminder_from_another_section(cx: &mut TestAppContext) {
    let (window, cx) = open_updates(cx);
    click("settings-navigation-settings-section-interface", cx);

    cx.update(|_, cx| {
        let search = window.read(cx).search.clone();
        search.update(cx, |search, cx| search.set_value("overdue".to_owned(), cx));
    });
    cx.run_until_parked();

    assert_eq!(
        window.read_with(cx, |settings, _| settings.active_section),
        SettingsSectionId::Updates
    );
    assert!(
        cx.debug_bounds("settings-row-update-reminder-interval")
            .is_some()
    );
}

#[gpui::test]
fn status_should_offer_no_update_action_without_the_update_service(cx: &mut TestAppContext) {
    let (window, cx) = open_updates(cx);

    let status = window.read_with(cx, |settings, cx| settings.update_status(cx));
    assert_eq!(status.action, None);
    for selector in [CHECK_NOW_SELECTOR, DOWNLOAD_SELECTOR, RESTART_SELECTOR] {
        assert!(
            cx.debug_bounds(selector).is_none(),
            "{selector} should not render"
        );
    }
    assert!(
        cx.debug_bounds("settings-row-update-status-description")
            .is_some()
    );
}
