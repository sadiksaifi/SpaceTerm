//! Screen Recording and Device Control access in the Privacy section, driven through a scripted
//! capability.

use std::rc::Rc;
use std::sync::Arc;

use gpui::{Entity, Modifiers, TestAppContext, VisualTestContext};

use crate::appearance::Appearance;
use crate::platform::appearance::testing::RecordingAppearancePlatform;
use crate::platform::permission_access::testing::ScriptedPermissionAccess;
use crate::platform::permission_access::{
    AccessibilityNaming, PermissionAccess, PermissionAccessError, PermissionAuthorization,
    SystemPermission,
};
use crate::platform::setup_guide_host::SystemSettingsWindow;
use crate::platform::setup_guide_host::testing::ScriptedSetupGuideHost;
use crate::platform::window_movement::RecordingOperatingSystemWindowDragPlatform;
use crate::settings::SettingsDocument;
use crate::ui::appearance_runtime;

use super::super::permission_access::{
    ACCESSIBILITY, PermissionAccessAction, PermissionAccessStatus, RecoveryNotice, SCREEN_RECORDING,
};
use super::super::test_support::MemoryStorage;
use super::super::{PermissionCapabilities, SettingsRowId, SettingsSectionId, SettingsWindow};

use PermissionAuthorization::{Granted, NotGranted};
use SystemPermission::{Accessibility, ScreenRecording};

const SCREEN_RECORDING_ROW: &str = "settings-row-screen-recording-access";
const ACCESSIBILITY_ROW: &str = "settings-row-accessibility-access";
const APPLICATION: &str =
    crate::application_identity::ApplicationIdentity::current().display_name();

/// Opens Settings beside `access`. A `host` also installs the Permission Setup the rows start.
fn open_settings(
    access: Option<Rc<ScriptedPermissionAccess>>,
    host: Option<Arc<ScriptedSetupGuideHost>>,
    cx: &mut TestAppContext,
) -> (Entity<SettingsWindow>, &mut VisualTestContext) {
    let settings =
        crate::settings::Settings::load(MemoryStorage::with_document(&SettingsDocument::default()));
    let platform = RecordingAppearancePlatform::default();
    platform.set_system_appearance(Some(Appearance::Dark));
    let access = access.map(|access| access as Rc<dyn PermissionAccess>);
    let permission_setup = cx.update(|cx| {
        appearance_runtime::install(settings, Rc::new(platform), cx)
            .expect("appearance runtime should install");
        crate::ui::init(cx).expect("UI initialization should succeed");
        match (&access, host) {
            (Some(access), Some(host)) => Some(
                crate::ui::permission_setup::PermissionSetup::create(access.clone(), host, cx),
            ),
            _ => None,
        }
    });
    let (window, cx) = cx.add_window_view(|window, cx| {
        SettingsWindow::new_with_capabilities(
            Rc::new(RecordingOperatingSystemWindowDragPlatform::default()),
            PermissionCapabilities {
                microphone: None,
                system_permissions: access,
                permission_setup,
            },
            None,
            window,
            cx,
        )
    });
    cx.update(|window, _| window.activate_window());
    cx.run_until_parked();
    (window, cx)
}

fn open_privacy_with<'a>(
    access: &Rc<ScriptedPermissionAccess>,
    host: Arc<ScriptedSetupGuideHost>,
    cx: &'a mut TestAppContext,
) -> (Entity<SettingsWindow>, &'a mut VisualTestContext) {
    let (window, cx) = open_settings(Some(access.clone()), Some(host), cx);
    click("settings-navigation-settings-section-privacy", cx);
    (window, cx)
}

fn open_privacy<'a>(
    access: &Rc<ScriptedPermissionAccess>,
    cx: &'a mut TestAppContext,
) -> (Entity<SettingsWindow>, &'a mut VisualTestContext) {
    open_privacy_with(access, ScriptedSetupGuideHost::new(), cx)
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

/// The selector the modal renderer gives one action.
fn modal_action(debug_identity: &str) -> &'static str {
    format!("modal-action-{debug_identity}").leak()
}

fn click_modal_action(debug_identity: &str, cx: &mut VisualTestContext) {
    click(modal_action(debug_identity), cx);
}

fn status(
    window: &Entity<SettingsWindow>,
    permission: SystemPermission,
    cx: &mut VisualTestContext,
) -> PermissionAccessStatus {
    window.read_with(cx, |window, _| {
        window.permission_access.row(permission).status
    })
}

fn action(
    window: &Entity<SettingsWindow>,
    permission: SystemPermission,
    cx: &mut VisualTestContext,
) -> Option<PermissionAccessAction> {
    window.read_with(cx, |window, _| {
        window
            .permission_access
            .row(permission)
            .presentation()
            .action
    })
}

fn action_enabled(
    window: &Entity<SettingsWindow>,
    permission: SystemPermission,
    cx: &mut VisualTestContext,
) -> bool {
    window.read_with(cx, |window, _| {
        window
            .permission_access
            .row(permission)
            .presentation()
            .action_enabled
    })
}

fn explanation(
    window: &Entity<SettingsWindow>,
    permission: SystemPermission,
    cx: &mut VisualTestContext,
) -> String {
    window.read_with(cx, |window, _| {
        window
            .permission_access
            .row(permission)
            .presentation()
            .explanation
            .to_string()
    })
}

fn notice(
    window: &Entity<SettingsWindow>,
    permission: SystemPermission,
    cx: &mut VisualTestContext,
) -> Option<RecoveryNotice> {
    window.read_with(cx, |window, _| {
        window.permission_access.row(permission).notice
    })
}

/// Leaving the window for System Settings and coming back.
fn return_to_settings(cx: &mut VisualTestContext) {
    cx.deactivate_window();
    cx.update(|window, _| window.activate_window());
    cx.run_until_parked();
}

#[gpui::test]
fn privacy_section_presents_both_permissions_beside_microphone_access(cx: &mut TestAppContext) {
    let access = ScriptedPermissionAccess::new(Ok(NotGranted), Ok(Granted));
    let (window, cx) = open_privacy(&access, cx);

    for selector in [
        "settings-section-privacy-group-permissions",
        "settings-row-microphone-access",
        SCREEN_RECORDING_ROW,
        SCREEN_RECORDING.control,
        SCREEN_RECORDING.state_not_allowed,
        SCREEN_RECORDING.set_up,
        ACCESSIBILITY_ROW,
        ACCESSIBILITY.control,
        ACCESSIBILITY.state_allowed,
        ACCESSIBILITY.troubleshoot,
    ] {
        assert!(
            cx.debug_bounds(selector).is_some(),
            "{selector} should render"
        );
    }
    let group = cx
        .debug_bounds("settings-section-privacy-group-permissions")
        .expect("group bounds");
    for row in [SCREEN_RECORDING_ROW, ACCESSIBILITY_ROW] {
        let bounds = cx.debug_bounds(row).expect("row bounds");
        assert!(
            group.contains(&bounds.origin),
            "{row} should share the group"
        );
    }
    // Each row offers one action: Set Up already recovers a missing grant, so only an allowed
    // grant offers Troubleshoot.
    assert!(cx.debug_bounds(SCREEN_RECORDING.troubleshoot).is_none());
    assert!(cx.debug_bounds(ACCESSIBILITY.set_up).is_none());
    // The description says what the permission lets terminal programs do, in any state.
    assert_eq!(
        explanation(&window, ScreenRecording, cx),
        "Lets terminal programs take screenshots."
    );
    assert_eq!(
        explanation(&window, Accessibility, cx),
        "Lets terminal programs click and type in other apps."
    );
    // Reading authorization starts nothing: a Permission Setup waits for the explicit action.
    assert!(access.prepared.borrow().is_empty());
    assert!(access.opened.borrow().is_empty());
}

#[gpui::test]
fn settings_search_reveals_a_permission_from_another_section(cx: &mut TestAppContext) {
    let access = ScriptedPermissionAccess::new(Ok(Granted), Ok(NotGranted));
    let (window, cx) = open_settings(Some(access), Some(ScriptedSetupGuideHost::new()), cx);

    cx.update(|_, cx| {
        let search = window.read(cx).search.clone();
        search.update(cx, |search, cx| {
            search.set_value("accessibility".to_owned(), cx);
        });
    });
    cx.run_until_parked();

    window.read_with(cx, |settings, _| {
        assert_eq!(settings.active_section, SettingsSectionId::Privacy);
        assert_eq!(settings.revealed, Some(SettingsRowId::AccessibilityAccess));
    });
    assert!(cx.debug_bounds(ACCESSIBILITY_ROW).is_some());
    assert!(cx.debug_bounds(ACCESSIBILITY.set_up).is_some());
}

/// Set Up starts a Permission Setup, the row follows it, and only a read reports the grant.
#[gpui::test]
fn set_up_starts_a_permission_setup_and_the_row_follows_it(cx: &mut TestAppContext) {
    let access = ScriptedPermissionAccess::new(Ok(NotGranted), Ok(NotGranted));
    let host = ScriptedSetupGuideHost::new();
    let (window, cx) = open_privacy_with(&access, host.clone(), cx);

    click(SCREEN_RECORDING.set_up, cx);

    assert_eq!(*access.prepared.borrow(), [ScreenRecording]);
    assert_eq!(*access.opened.borrow(), [ScreenRecording]);
    assert_eq!(
        action(&window, ScreenRecording, cx),
        Some(PermissionAccessAction::CancelSetup)
    );
    assert!(explanation(&window, ScreenRecording, cx).contains("Continue in System Settings"));
    // The other permission is untouched by this row's setup.
    assert_eq!(
        action(&window, Accessibility, cx),
        Some(PermissionAccessAction::SetUp)
    );
    // Opening System Settings alone never reports access as allowed.
    return_to_settings(cx);
    assert_eq!(
        status(&window, ScreenRecording, cx),
        PermissionAccessStatus::Authorization(NotGranted)
    );

    host.set_window(SystemSettingsWindow::Frontmost {
        display: cx.update(|_, cx| cx.primary_display().expect("a display").id()),
        content: gpui::bounds(
            gpui::point(gpui::px(100.0), gpui::px(100.0)),
            gpui::size(gpui::px(715.0), gpui::px(560.0)),
        ),
    });
    cx.executor()
        .advance_clock(std::time::Duration::from_millis(33));
    cx.run_until_parked();
    access.set(ScreenRecording, Ok(Granted));
    access.report_change();
    cx.run_until_parked();

    assert_eq!(
        status(&window, ScreenRecording, cx),
        PermissionAccessStatus::Authorization(Granted)
    );
    assert_eq!(
        action(&window, ScreenRecording, cx),
        Some(PermissionAccessAction::Troubleshoot)
    );
    assert!(cx.debug_bounds(SCREEN_RECORDING.state_allowed).is_some());
}

#[gpui::test]
fn cancel_setup_ends_the_running_setup(cx: &mut TestAppContext) {
    let access = ScriptedPermissionAccess::new(Ok(NotGranted), Ok(NotGranted));
    let (window, cx) = open_privacy(&access, cx);
    click(ACCESSIBILITY.set_up, cx);

    click(ACCESSIBILITY.cancel_setup, cx);

    assert_eq!(
        action(&window, Accessibility, cx),
        Some(PermissionAccessAction::SetUp)
    );
    assert!(!explanation(&window, Accessibility, cx).contains("Continue in System Settings"));
    assert_eq!(
        access.observers(),
        1,
        "only the window still observes changes"
    );
}

/// A host without a Permission Setup still leads to the permission's list in System Settings.
#[gpui::test]
fn a_host_without_a_permission_setup_opens_system_settings(cx: &mut TestAppContext) {
    let access = ScriptedPermissionAccess::new(Ok(NotGranted), Ok(NotGranted));
    let (window, cx) = open_settings(Some(access.clone()), None, cx);
    click("settings-navigation-settings-section-privacy", cx);

    assert_eq!(
        action(&window, ScreenRecording, cx),
        Some(PermissionAccessAction::OpenSettings)
    );
    click(SCREEN_RECORDING.open_settings, cx);

    assert_eq!(*access.opened.borrow(), [ScreenRecording]);
    assert!(access.prepared.borrow().is_empty());
}

/// Revocation and reauthorization both arrive through reads on return.
#[gpui::test]
fn a_revoked_grant_is_read_on_return_and_offers_a_setup(cx: &mut TestAppContext) {
    let access = ScriptedPermissionAccess::new(Ok(Granted), Ok(Granted));
    let (window, cx) = open_privacy(&access, cx);

    access.set(Accessibility, Ok(NotGranted));
    return_to_settings(cx);
    assert_eq!(
        status(&window, Accessibility, cx),
        PermissionAccessStatus::Authorization(NotGranted)
    );
    assert_eq!(
        action(&window, Accessibility, cx),
        Some(PermissionAccessAction::SetUp)
    );

    access.set(Accessibility, Ok(Granted));
    return_to_settings(cx);
    assert_eq!(
        status(&window, Accessibility, cx),
        PermissionAccessStatus::Authorization(Granted)
    );
}

/// The system caches a read until it reports the change, which can arrive after the window became
/// active again, so the reported change refreshes the rows without another activation.
#[gpui::test]
fn a_reported_change_refreshes_a_grant_the_activation_read_missed(cx: &mut TestAppContext) {
    let access = ScriptedPermissionAccess::new(Ok(NotGranted), Ok(NotGranted));
    let (window, cx) = open_privacy(&access, cx);
    assert_eq!(access.observers(), 1);

    access.set(Accessibility, Ok(Granted));
    cx.run_until_parked();
    assert_eq!(
        status(&window, Accessibility, cx),
        PermissionAccessStatus::Authorization(NotGranted)
    );

    access.report_change();
    cx.run_until_parked();
    assert_eq!(
        status(&window, Accessibility, cx),
        PermissionAccessStatus::Authorization(Granted)
    );
    assert!(cx.debug_bounds(ACCESSIBILITY.state_allowed).is_some());

    // Closing the window ends the observation with it.
    drop(window);
    cx.update(|window, _| window.remove_window());
    cx.run_until_parked();
    assert_eq!(access.observers(), 0);
}

/// System Settings is an ordinary window of another application. Settings stays at the normal
/// window level, so System Settings and its prompts appear above Settings instead of behind it.
#[gpui::test]
fn settings_opens_at_the_normal_window_level_so_system_settings_stays_visible(
    cx: &mut TestAppContext,
) {
    let access = ScriptedPermissionAccess::new(Ok(NotGranted), Ok(NotGranted));
    let (_window, cx) = open_settings(Some(access), None, cx);

    let options = cx.update(|_, cx| {
        crate::ui::sidebar_window::window_options(
            "Settings",
            gpui::size(gpui::px(1.), gpui::px(1.)),
            cx,
        )
    });
    assert!(options.kind == gpui::WindowKind::Normal);
}

/// Stale-grant recovery: an allowed grant can still fail, so Troubleshoot offers one alert whose
/// Reset clears one permission of this application and starts a Permission Setup again.
#[gpui::test]
fn troubleshoot_resets_one_permission_and_sets_it_up_again(cx: &mut TestAppContext) {
    let access = ScriptedPermissionAccess::new(Ok(Granted), Ok(Granted));
    let (window, cx) = open_privacy(&access, cx);

    click(ACCESSIBILITY.troubleshoot, cx);
    assert!(
        cx.debug_bounds(modal_action(ACCESSIBILITY.troubleshoot_open_settings))
            .is_none(),
        "a resettable identity is offered Reset, which leads to System Settings itself"
    );
    // The alert alone resets nothing.
    assert!(access.resets.borrow().is_empty());
    click_modal_action(ACCESSIBILITY.troubleshoot_reset, cx);

    assert_eq!(*access.resets.borrow(), [Accessibility]);
    assert_eq!(
        notice(&window, Accessibility, cx),
        Some(RecoveryNotice::Resetting)
    );
    assert!(!action_enabled(&window, Accessibility, cx));
    // The grant can disappear before the reset reports, and a return to the window meanwhile
    // keeps reporting the reset as running and withholds Set Up.
    access.set(Accessibility, Ok(NotGranted));
    return_to_settings(cx);
    assert_eq!(
        notice(&window, Accessibility, cx),
        Some(RecoveryNotice::Resetting)
    );
    assert!(!action_enabled(&window, Accessibility, cx));
    click(ACCESSIBILITY.set_up, cx);
    assert!(access.prepared.borrow().is_empty());

    access.take_reset()(Ok(()));
    cx.run_until_parked();

    assert_eq!(*access.prepared.borrow(), [Accessibility]);
    assert_eq!(*access.opened.borrow(), [Accessibility]);
    assert_eq!(notice(&window, Accessibility, cx), None);
    assert_eq!(
        action(&window, Accessibility, cx),
        Some(PermissionAccessAction::CancelSetup)
    );
    assert!(action_enabled(&window, Accessibility, cx));
    // The other permission keeps its grant and its presentation.
    assert_eq!(
        status(&window, ScreenRecording, cx),
        PermissionAccessStatus::Authorization(Granted)
    );
    assert_eq!(notice(&window, ScreenRecording, cx), None);
}

/// A host without a Permission Setup goes straight to System Settings after a reset.
#[gpui::test]
fn a_reset_without_a_permission_setup_opens_system_settings(cx: &mut TestAppContext) {
    let access = ScriptedPermissionAccess::new(Ok(Granted), Ok(Granted));
    let (window, cx) = open_settings(Some(access.clone()), None, cx);
    click("settings-navigation-settings-section-privacy", cx);

    click(SCREEN_RECORDING.troubleshoot, cx);
    click_modal_action(SCREEN_RECORDING.troubleshoot_reset, cx);
    access.set(ScreenRecording, Ok(NotGranted));
    access.take_reset()(Ok(()));
    cx.run_until_parked();

    assert_eq!(*access.opened.borrow(), [ScreenRecording]);
    assert!(access.prepared.borrow().is_empty());
    assert_eq!(
        action(&window, ScreenRecording, cx),
        Some(PermissionAccessAction::OpenSettings)
    );
}

#[gpui::test]
fn a_cancelled_troubleshoot_changes_nothing(cx: &mut TestAppContext) {
    let access = ScriptedPermissionAccess::new(Ok(Granted), Ok(Granted));
    let (window, cx) = open_privacy(&access, cx);
    let guidance = explanation(&window, ScreenRecording, cx);

    click(SCREEN_RECORDING.troubleshoot, cx);
    click_modal_action(SCREEN_RECORDING.troubleshoot_cancel, cx);

    assert!(access.resets.borrow().is_empty());
    assert!(access.opened.borrow().is_empty());
    assert_eq!(explanation(&window, ScreenRecording, cx), guidance);
}

#[gpui::test]
fn a_failed_reset_reports_failure_and_explains_the_manual_recovery(cx: &mut TestAppContext) {
    let access = ScriptedPermissionAccess::new(Ok(Granted), Ok(Granted));
    let (window, cx) = open_privacy(&access, cx);

    click(SCREEN_RECORDING.troubleshoot, cx);
    click_modal_action(SCREEN_RECORDING.troubleshoot_reset, cx);
    access.take_reset()(Err(PermissionAccessError::PlatformRejected));
    cx.run_until_parked();

    assert_eq!(
        notice(&window, ScreenRecording, cx),
        Some(RecoveryNotice::ResetFailed)
    );
    let failure = explanation(&window, ScreenRecording, cx);
    assert!(failure.contains("could not be reset"));
    assert!(failure.contains("Privacy & Security > Screen & System Audio Recording"));
    assert!(!failure.contains("PlatformRejected"));
    // A failed reset starts no setup.
    assert!(access.prepared.borrow().is_empty());

    // A reset that cannot start reports the same failure.
    access
        .reset_failure
        .set(Some(PermissionAccessError::PlatformUnavailable));
    click(SCREEN_RECORDING.troubleshoot, cx);
    click_modal_action(SCREEN_RECORDING.troubleshoot_reset, cx);
    assert_eq!(
        notice(&window, ScreenRecording, cx),
        Some(RecoveryNotice::ResetFailed)
    );
    assert_eq!(access.resets.borrow().len(), 2);
}

/// A reset dropped without a result never reports success.
#[gpui::test]
fn a_reset_dropped_without_a_result_reports_failure(cx: &mut TestAppContext) {
    let access = ScriptedPermissionAccess::new(Ok(Granted), Ok(Granted));
    let (window, cx) = open_privacy(&access, cx);

    click(SCREEN_RECORDING.troubleshoot, cx);
    click_modal_action(SCREEN_RECORDING.troubleshoot_reset, cx);
    drop(access.take_reset());
    cx.run_until_parked();

    assert_eq!(
        notice(&window, ScreenRecording, cx),
        Some(RecoveryNotice::ResetFailed)
    );
    assert!(access.prepared.borrow().is_empty());
}

/// A running bundle that is not this build's identity is offered the manual steps only.
#[gpui::test]
fn troubleshoot_offers_system_settings_when_the_identity_cannot_be_reset(cx: &mut TestAppContext) {
    let access = ScriptedPermissionAccess::new(Ok(Granted), Ok(Granted));
    access.resettable.set(false);
    let (window, cx) = open_privacy(&access, cx);

    click(SCREEN_RECORDING.troubleshoot, cx);

    assert!(
        cx.debug_bounds(modal_action(SCREEN_RECORDING.troubleshoot_reset))
            .is_none()
    );
    click_modal_action(SCREEN_RECORDING.troubleshoot_open_settings, cx);
    assert_eq!(*access.opened.borrow(), [ScreenRecording]);
    cx.update(|_, cx| {
        window.update(cx, |settings, cx| {
            settings.reset_permission(ScreenRecording, cx);
        });
    });
    assert!(access.resets.borrow().is_empty());
}

#[gpui::test]
fn a_setup_that_cannot_open_system_settings_explains_where_to_go(cx: &mut TestAppContext) {
    let access = ScriptedPermissionAccess::new(Ok(NotGranted), Ok(NotGranted));
    access
        .open_failure
        .set(Some(PermissionAccessError::PlatformRejected));
    let (window, cx) = open_privacy(&access, cx);
    let guidance = explanation(&window, Accessibility, cx);

    click(ACCESSIBILITY.set_up, cx);

    assert_eq!(
        status(&window, Accessibility, cx),
        PermissionAccessStatus::Authorization(NotGranted)
    );
    assert_eq!(
        action(&window, Accessibility, cx),
        Some(PermissionAccessAction::SetUp)
    );
    let failure = explanation(&window, Accessibility, cx);
    assert_ne!(failure, guidance);
    assert!(failure.contains("could not be opened"));
    assert!(failure.contains("Privacy & Security > Device Control and Data Access"));
    assert!(failure.contains(APPLICATION));

    access.open_failure.set(None);
    click(ACCESSIBILITY.set_up, cx);
    assert!(explanation(&window, Accessibility, cx).contains("Continue in System Settings"));
}

/// A system whose System Settings calls the list Accessibility sends the person there.
#[gpui::test]
fn an_earlier_system_names_the_accessibility_list(cx: &mut TestAppContext) {
    let access = ScriptedPermissionAccess::new(Ok(NotGranted), Ok(NotGranted));
    access.naming.set(AccessibilityNaming::Accessibility);
    access
        .open_failure
        .set(Some(PermissionAccessError::PlatformRejected));
    let (window, cx) = open_privacy(&access, cx);

    click(ACCESSIBILITY.set_up, cx);

    let failure = explanation(&window, Accessibility, cx);
    assert!(failure.contains("Privacy & Security > Accessibility"));
    assert!(!failure.contains("Device Control"));
    assert_eq!(
        window.read_with(cx, |window, _| window.permission_access.naming()),
        AccessibilityNaming::Accessibility
    );
}

#[gpui::test]
fn an_unreadable_authorization_reports_failure_and_checks_again(cx: &mut TestAppContext) {
    let access =
        ScriptedPermissionAccess::new(Err(PermissionAccessError::PlatformUnavailable), Ok(Granted));
    let (window, cx) = open_privacy(&access, cx);

    assert_eq!(
        status(&window, ScreenRecording, cx),
        PermissionAccessStatus::Failed(PermissionAccessError::PlatformUnavailable)
    );
    assert!(
        cx.debug_bounds(SCREEN_RECORDING.state_unavailable)
            .is_some()
    );
    assert_eq!(
        action(&window, ScreenRecording, cx),
        Some(PermissionAccessAction::CheckAgain)
    );
    // Neither success nor recovery is offered for a state the system did not report.
    assert!(cx.debug_bounds(SCREEN_RECORDING.troubleshoot).is_none());

    access.set(ScreenRecording, Ok(NotGranted));
    click(SCREEN_RECORDING.check_again, cx);

    assert_eq!(
        status(&window, ScreenRecording, cx),
        PermissionAccessStatus::Authorization(NotGranted)
    );
    assert!(cx.debug_bounds(SCREEN_RECORDING.set_up).is_some());
}

/// Every typed failure produces its independent fixed product copy.
#[gpui::test]
fn failure_explanations_are_distinct_fixed_copy(cx: &mut TestAppContext) {
    let access = ScriptedPermissionAccess::new(Ok(Granted), Ok(Granted));
    let (window, cx) = open_settings(Some(access.clone()), None, cx);
    let check_failure = match (cfg!(spaceterm_packaged), cfg!(spaceterm_release)) {
        (false, _) => "SpaceTerm Development could not check Screen Recording access.",
        (true, false) => "SpaceTerm Preflight could not check Screen Recording access.",
        (true, true) => "SpaceTerm could not check Screen Recording access.",
    };
    for (error, expected) in [
        (PermissionAccessError::OffMainThread, check_failure),
        (
            PermissionAccessError::PlatformUnavailable,
            "The system did not report Screen Recording access.",
        ),
        (
            PermissionAccessError::PlatformRejected,
            "The system rejected the Screen Recording access check.",
        ),
    ] {
        access.set(ScreenRecording, Err(error));
        cx.update(|_, cx| {
            window.update(cx, |settings, cx| settings.refresh_permission_access(cx));
        });
        assert_eq!(explanation(&window, ScreenRecording, cx), expected);
    }
}

#[gpui::test]
fn a_host_without_the_capability_presents_both_permissions_as_unavailable(cx: &mut TestAppContext) {
    let (window, cx) = open_settings(None, Some(ScriptedSetupGuideHost::new()), cx);
    click("settings-navigation-settings-section-privacy", cx);

    for (permission, text) in [
        (ScreenRecording, &SCREEN_RECORDING),
        (Accessibility, &ACCESSIBILITY),
    ] {
        assert_eq!(
            status(&window, permission, cx),
            PermissionAccessStatus::Unsupported
        );
        assert!(cx.debug_bounds(text.state_unavailable).is_some());
        assert_eq!(action(&window, permission, cx), None);
        for selector in [
            text.set_up,
            text.cancel_setup,
            text.open_settings,
            text.check_again,
            text.troubleshoot,
        ] {
            assert!(
                cx.debug_bounds(selector).is_none(),
                "{selector} should not be offered"
            );
        }
    }
}

/// The badge and its button share the control column with wrapped guidance, so every state keeps
/// each row at its natural height with the control centered on it.
#[gpui::test]
fn permission_rows_keep_their_natural_height_beside_their_action(cx: &mut TestAppContext) {
    let access = ScriptedPermissionAccess::new(Ok(NotGranted), Ok(NotGranted));
    let (window, cx) = open_privacy(&access, cx);
    let states = [
        Ok(NotGranted),
        Ok(Granted),
        Err(PermissionAccessError::PlatformRejected),
    ];
    for width in [super::super::WINDOW_WIDTH, 1100.0, 1400.0] {
        cx.simulate_resize(gpui::size(
            gpui::px(width),
            gpui::px(super::super::WINDOW_HEIGHT),
        ));
        cx.run_until_parked();
        for state in states {
            access.set(ScreenRecording, state);
            cx.update(|_, cx| {
                window.update(cx, |settings, cx| settings.refresh_permission_access(cx));
            });
            cx.run_until_parked();

            let mut bounds = |selector: &'static str| {
                cx.debug_bounds(selector)
                    .unwrap_or_else(|| panic!("{selector} should render"))
            };
            let row = bounds(SCREEN_RECORDING_ROW);
            let label = bounds("settings-row-screen-recording-access-label");
            let description = bounds("settings-row-screen-recording-access-description");
            let control = bounds(SCREEN_RECORDING.control);
            let context = format!("{state:?} at {width}px");

            let padding_above = label.top() - row.top();
            let padding_below = row.bottom() - description.bottom();
            assert!(
                (padding_above - padding_below).abs() <= gpui::px(1.0),
                "{context}: the row should end just below its guidance"
            );
            assert!(
                (control.center().y - row.center().y).abs() <= gpui::px(1.0),
                "{context}: the control should be centered on the row"
            );
            assert!(
                control.right() <= row.right(),
                "{context}: the control should stay inside the row"
            );
        }
    }
}
