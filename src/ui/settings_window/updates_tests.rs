//! The Updates section driven through its rendered controls.

use std::rc::Rc;

use gpui::{Entity, Modifiers, TestAppContext, VisualTestContext};

use crate::appearance::Appearance;
use crate::platform::appearance::testing::RecordingAppearancePlatform;
use crate::platform::window_movement::RecordingOperatingSystemWindowDragPlatform;
use crate::settings::SettingsDocument;
use crate::ui::appearance_runtime;
use crate::updates::policy::{CheckInterval, UpdatePreferences};

use super::updates::{CHECK_NOW_SELECTOR, RESTART_SELECTOR};
use super::{SettingsSectionId, SettingsWindow};
use crate::settings::storage::testing::MemoryStorage;

fn open_updates(cx: &mut TestAppContext) -> (Entity<SettingsWindow>, &mut VisualTestContext) {
    let settings =
        crate::settings::Settings::load(MemoryStorage::with_document(&SettingsDocument::default()));
    let platform = RecordingAppearancePlatform::default();
    platform.set_system_appearance(Some(Appearance::Dark));
    cx.update(|cx| {
        appearance_runtime::install(settings, Rc::new(platform), cx)
            .expect("appearance runtime should install");
        crate::ui::init(cx).expect("UI initialization should succeed");
    });
    let (window, cx) = cx.add_window_view(|window, cx| {
        SettingsWindow::new_with_capabilities(
            Rc::new(RecordingOperatingSystemWindowDragPlatform::default()),
            Default::default(),
            crate::theme_registry::testing::offline(),
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
    ] {
        assert!(
            cx.debug_bounds(selector).is_some(),
            "{selector} should render"
        );
    }
    assert!(
        cx.debug_bounds("settings-row-update-reminder-interval")
            .is_none(),
        "reminders repeat daily and have no setting"
    );
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
    assert_eq!(
        preferences(&window, cx),
        UpdatePreferences {
            automatic_downloads: false,
            check_interval: CheckInterval::Hourly,
        }
    );

    click("settings-row-update-check-interval-reset", cx);
    assert_eq!(
        preferences(&window, cx),
        UpdatePreferences {
            automatic_downloads: false,
            check_interval: CheckInterval::Daily,
        }
    );
    click("settings-row-automatic-update-downloads-reset", cx);
    assert_eq!(preferences(&window, cx), UpdatePreferences::default());
}

#[gpui::test]
fn status_should_offer_no_update_action_without_the_update_service(cx: &mut TestAppContext) {
    let (window, cx) = open_updates(cx);

    let status = window.read_with(cx, |settings, cx| settings.update_status(cx));
    assert_eq!(status.action, None);
    for selector in [CHECK_NOW_SELECTOR, RESTART_SELECTOR] {
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

#[gpui::test]
fn ready_settings_and_title_bar_share_copy_and_relaunch_without_confirmation(
    cx: &mut TestAppContext,
) {
    use crate::updates::{ApplicationUpdates, UpdateEvent, UpdateService};
    let adapter = Rc::new(crate::updates::testing::RecordingAdapter::available());
    cx.update(|cx| ApplicationUpdates::install(adapter.clone(), cx));
    let (window, cx) = open_updates(cx);
    let updates = cx.update(|_, cx| cx.global::<UpdateService>().0.clone());
    updates.update(cx, |updates, cx| updates.check(false, cx));
    adapter.emit(UpdateEvent::Available("0.11.1".into()));
    adapter.emit(UpdateEvent::Ready);
    cx.run_until_parked();
    let status = window.read_with(cx, |settings, cx| settings.update_status(cx));
    assert_eq!(
        status.summary.as_ref(),
        "SpaceTerm 0.11.1 is ready. Relaunching keeps every Terminal Session running."
    );
    assert_eq!(status.button.as_ref(), "Relaunch");
    let shared = cx.update(|_, cx| crate::updates::presentation::current(cx).desktop.sentence());
    assert_eq!(status.summary, shared);
    click(RESTART_SELECTOR, cx);
    assert_eq!(
        adapter.installations.get(),
        1,
        "one click requests relaunch"
    );
    assert!(
        cx.debug_bounds(RESTART_SELECTOR).is_none(),
        "relaunch is in progress"
    );
}

/// A relaunch that ran out of time leaves the updater holding its installer, and no new check
/// can replace it. After the person acknowledges the title bar's explanation, every surface
/// still offers to relaunch, and the Settings button quits again into the held installer.
#[gpui::test]
fn an_acknowledged_relaunch_failure_still_relaunches_from_settings(cx: &mut TestAppContext) {
    use crate::updates::presentation::{self, Command, DesktopAction, ExtraItem, ItemKey};
    use crate::updates::{ApplicationUpdates, UpdateEvent, UpdateService};
    let adapter = Rc::new(crate::updates::testing::RecordingAdapter::available());
    cx.update(|cx| ApplicationUpdates::install(adapter.clone(), cx));
    let (window, cx) = open_updates(cx);
    let quits = Rc::new(std::cell::Cell::new(0));
    let counted = quits.clone();
    cx.update(|_, cx| {
        cx.on_action(move |_: &crate::app::QuitApplication, _| counted.set(counted.get() + 1));
    });
    let updates = cx.update(|_, cx| cx.global::<UpdateService>().0.clone());
    updates.update(cx, |updates, cx| updates.check(false, cx));
    adapter.emit(UpdateEvent::Available("0.11.1".into()));
    adapter.emit(UpdateEvent::Ready);
    cx.run_until_parked();
    click(RESTART_SELECTOR, cx);
    cx.executor()
        .advance_clock(std::time::Duration::from_secs(30));
    cx.run_until_parked();

    // The person acknowledges the title bar's explanation.
    let failed = cx.update(|_, cx| presentation::current(cx).items());
    let Some(Command::Explain(explanation)) = failed
        .iter()
        .find(|item| item.key == ItemKey::Desktop)
        .and_then(|item| item.command.clone())
    else {
        panic!("the failed relaunch explains");
    };
    let then = *explanation.then.expect("OK acknowledges the failure");
    cx.update(|_, cx| {
        crate::ui::updates::commands::perform(
            then,
            crate::ui::updates::commands::Presenter::Front,
            cx,
        );
    });
    cx.run_until_parked();

    let shared = cx.update(|_, cx| presentation::current(cx));
    let relaunch = Command::Desktop(DesktopAction::Update);
    let item = shared
        .items()
        .into_iter()
        .find(|item| item.key == ItemKey::Desktop)
        .expect("the title bar still offers to relaunch");
    assert_eq!(item.label.as_ref(), "Relaunch");
    assert_eq!(item.command.as_ref(), Some(&relaunch));
    assert!(
        shared.extra_actions().iter().any(
            |action| matches!(action, ExtraItem::Action { command, .. } if *command == relaunch)
        ),
        "the menu bar extra still offers to relaunch"
    );
    let status = window.read_with(cx, |settings, cx| settings.update_status(cx));
    assert_eq!(
        status.summary.as_ref(),
        "SpaceTerm 0.11.1 is downloaded, but SpaceTerm couldn't quit to install it. Your \
         Terminal Sessions are still running."
    );
    assert_eq!(status.button.as_ref(), "Try Again");

    let before = quits.get();
    click(RESTART_SELECTOR, cx);
    assert_eq!(
        quits.get(),
        before + 1,
        "Try Again quits into the held installer"
    );
    assert!(
        cx.update(|_, cx| crate::updates::relaunch_approval(cx))
            .is_some()
    );
    assert_eq!(
        adapter.checks.get(),
        1,
        "the held installer needs no new check"
    );
}
