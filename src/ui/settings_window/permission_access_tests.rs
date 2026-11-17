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

use super::super::access_row::{AccessAction, AccessControl, CaptionKind};
use super::super::permission_access::{
    ACCESSIBILITY, PermissionAccessStatus, RecoveryNotice, SCREEN_RECORDING,
};
use super::super::{PermissionCapabilities, SettingsWindow};
use crate::settings::storage::testing::MemoryStorage;

use PermissionAuthorization::{Granted, NotGranted};
use SystemPermission::{Accessibility, ScreenRecording};

const SCREEN_RECORDING_ROW: &str = "settings-row-screen-recording-access";
const ACCESSIBILITY_ROW: &str = "settings-row-accessibility-access";
const SCREEN_RECORDING_RESET: &str = "settings-row-screen-recording-access-reset";
const ACCESSIBILITY_RESET: &str = "settings-row-accessibility-access-reset";
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
                capture: None,
                system_permissions: access,
                notifications: None,
                permission_setup,
            },
            crate::theme_registry::testing::offline(),
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
) -> Option<AccessAction> {
    window.read_with(cx, |window, _| {
        match window
            .permission_access
            .row(permission)
            .presentation()
            .control
        {
            AccessControl::Action(action) => Some(action),
            AccessControl::Status { .. } => None,
        }
    })
}

/// The state the row shows in place of a button, when it has no next step.
fn shown_state(
    window: &Entity<SettingsWindow>,
    permission: SystemPermission,
    cx: &mut VisualTestContext,
) -> Option<&'static str> {
    window.read_with(cx, |window, _| {
        match window
            .permission_access
            .row(permission)
            .presentation()
            .control
        {
            AccessControl::Status { label, .. } => Some(label),
            AccessControl::Action(_) => None,
        }
    })
}

fn caption_kind(
    window: &Entity<SettingsWindow>,
    permission: SystemPermission,
    cx: &mut VisualTestContext,
) -> CaptionKind {
    window.read_with(cx, |window, _| {
        window
            .permission_access
            .row(permission)
            .presentation()
            .caption_kind
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
            .caption
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
        SCREEN_RECORDING.row.control,
        SCREEN_RECORDING.row.allow,
        ACCESSIBILITY_ROW,
        ACCESSIBILITY.row.control,
        ACCESSIBILITY.row.status,
        ACCESSIBILITY_RESET,
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
    // Each row ends in one control: a missing grant offers Allow, and an allowed grant shows its
    // state with the reset beside the label instead of a second button.
    assert_eq!(
        action(&window, ScreenRecording, cx),
        Some(AccessAction::Allow)
    );
    assert_eq!(shown_state(&window, Accessibility, cx), Some("Allowed"));
    assert!(cx.debug_bounds(SCREEN_RECORDING.row.status).is_none());
    assert!(cx.debug_bounds(SCREEN_RECORDING_RESET).is_none());
    assert!(cx.debug_bounds(ACCESSIBILITY.row.allow).is_none());
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

/// Allow starts a Permission Setup, the row follows it, and only a read reports the grant.
#[gpui::test]
fn allow_starts_a_permission_setup_and_the_row_follows_it(cx: &mut TestAppContext) {
    let access = ScriptedPermissionAccess::new(Ok(NotGranted), Ok(NotGranted));
    let host = ScriptedSetupGuideHost::new();
    let (window, cx) = open_privacy_with(&access, host.clone(), cx);

    click(SCREEN_RECORDING.row.allow, cx);

    assert_eq!(*access.prepared.borrow(), [ScreenRecording]);
    assert_eq!(*access.opened.borrow(), [ScreenRecording]);
    assert_eq!(
        action(&window, ScreenRecording, cx),
        Some(AccessAction::Cancel)
    );
    assert_eq!(
        explanation(&window, ScreenRecording, cx),
        "Continue in System Settings."
    );
    assert_eq!(
        caption_kind(&window, ScreenRecording, cx),
        CaptionKind::Waiting
    );
    assert!(
        cx.debug_bounds(format!("{}-track", SCREEN_RECORDING.row.progress).leak())
            .is_some()
    );
    // The other permission is untouched by this row's setup.
    assert_eq!(
        action(&window, Accessibility, cx),
        Some(AccessAction::Allow)
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
    assert_eq!(shown_state(&window, ScreenRecording, cx), Some("Allowed"));
    assert!(cx.debug_bounds(SCREEN_RECORDING.row.status).is_some());
    assert!(cx.debug_bounds(SCREEN_RECORDING_RESET).is_some());
}

#[gpui::test]
fn cancel_ends_the_running_setup(cx: &mut TestAppContext) {
    let access = ScriptedPermissionAccess::new(Ok(NotGranted), Ok(NotGranted));
    let (window, cx) = open_privacy(&access, cx);
    click(ACCESSIBILITY.row.allow, cx);

    click(ACCESSIBILITY.row.cancel, cx);

    assert_eq!(
        action(&window, Accessibility, cx),
        Some(AccessAction::Allow)
    );
    assert_eq!(
        explanation(&window, Accessibility, cx),
        "Lets terminal programs click and type in other apps."
    );
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
        Some(AccessAction::OpenSettings)
    );
    click(SCREEN_RECORDING.row.open_settings, cx);

    assert_eq!(*access.opened.borrow(), [ScreenRecording]);
    assert!(access.prepared.borrow().is_empty());
}

/// Revocation and reauthorization both arrive through reads on return.
#[gpui::test]
fn a_revoked_grant_is_read_on_return_and_offers_allow(cx: &mut TestAppContext) {
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
        Some(AccessAction::Allow)
    );
    assert!(cx.debug_bounds(ACCESSIBILITY_RESET).is_none());

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
    assert!(cx.debug_bounds(ACCESSIBILITY.row.status).is_some());

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

    let options = cx.update(|_, cx| super::super::window_options(cx));
    assert!(options.kind == gpui::WindowKind::Normal);
}

/// Stale-grant recovery: an allowed grant can still fail, so the reset beside the label asks
/// once, clears one permission of the grant's holder, and starts a Permission Setup again.
#[gpui::test]
fn the_reset_clears_one_permission_and_sets_it_up_again(cx: &mut TestAppContext) {
    let access = ScriptedPermissionAccess::new(Ok(Granted), Ok(Granted));
    let (window, cx) = open_privacy(&access, cx);

    click(ACCESSIBILITY_RESET, cx);
    // The confirmation alone resets nothing.
    assert!(access.resets.borrow().is_empty());
    click_modal_action(ACCESSIBILITY.reset_confirm, cx);

    assert_eq!(*access.resets.borrow(), [Accessibility]);
    assert_eq!(
        notice(&window, Accessibility, cx),
        Some(RecoveryNotice::Resetting)
    );
    assert_eq!(shown_state(&window, Accessibility, cx), Some("Waiting"));
    assert_eq!(
        caption_kind(&window, Accessibility, cx),
        CaptionKind::Waiting
    );
    assert!(cx.debug_bounds(ACCESSIBILITY_RESET).is_none());
    // The grant can disappear before the reset reports, and a return to the window meanwhile
    // keeps reporting the reset as running and offers no Allow.
    access.set(Accessibility, Ok(NotGranted));
    return_to_settings(cx);
    assert_eq!(
        notice(&window, Accessibility, cx),
        Some(RecoveryNotice::Resetting)
    );
    assert_eq!(shown_state(&window, Accessibility, cx), Some("Waiting"));
    assert!(cx.debug_bounds(ACCESSIBILITY.row.allow).is_none());

    access.take_reset()(Ok(()));
    cx.run_until_parked();

    assert_eq!(*access.prepared.borrow(), [Accessibility]);
    assert_eq!(*access.opened.borrow(), [Accessibility]);
    assert_eq!(notice(&window, Accessibility, cx), None);
    assert_eq!(
        action(&window, Accessibility, cx),
        Some(AccessAction::Cancel)
    );
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

    click(SCREEN_RECORDING_RESET, cx);
    click_modal_action(SCREEN_RECORDING.reset_confirm, cx);
    access.set(ScreenRecording, Ok(NotGranted));
    access.take_reset()(Ok(()));
    cx.run_until_parked();

    assert_eq!(*access.opened.borrow(), [ScreenRecording]);
    assert!(access.prepared.borrow().is_empty());
    assert_eq!(
        action(&window, ScreenRecording, cx),
        Some(AccessAction::OpenSettings)
    );
}

#[gpui::test]
fn a_cancelled_reset_changes_nothing(cx: &mut TestAppContext) {
    let access = ScriptedPermissionAccess::new(Ok(Granted), Ok(Granted));
    let (window, cx) = open_privacy(&access, cx);
    let guidance = explanation(&window, ScreenRecording, cx);

    click(SCREEN_RECORDING_RESET, cx);
    click_modal_action(SCREEN_RECORDING.reset_cancel, cx);

    assert!(access.resets.borrow().is_empty());
    assert!(access.opened.borrow().is_empty());
    assert_eq!(explanation(&window, ScreenRecording, cx), guidance);
}

/// A failed reset leaves the grant as it was and says so, without sending the person to remove
/// the entry by hand: Permission Setup clears the entry itself.
#[gpui::test]
fn a_failed_reset_reports_failure_and_offers_the_reset_again(cx: &mut TestAppContext) {
    let access = ScriptedPermissionAccess::new(Ok(Granted), Ok(Granted));
    let (window, cx) = open_privacy(&access, cx);

    click(SCREEN_RECORDING_RESET, cx);
    click_modal_action(SCREEN_RECORDING.reset_confirm, cx);
    access.take_reset()(Err(PermissionAccessError::PlatformRejected));
    cx.run_until_parked();

    assert_eq!(
        notice(&window, ScreenRecording, cx),
        Some(RecoveryNotice::ResetFailed)
    );
    assert_eq!(
        explanation(&window, ScreenRecording, cx),
        "Couldn\u{2019}t reset Screen Recording. Try again."
    );
    assert_eq!(
        caption_kind(&window, ScreenRecording, cx),
        CaptionKind::Error
    );
    assert_eq!(shown_state(&window, ScreenRecording, cx), Some("Allowed"));
    assert!(cx.debug_bounds(SCREEN_RECORDING_RESET).is_some());
    // A failed reset starts no setup.
    assert!(access.prepared.borrow().is_empty());

    // A reset that cannot start reports the same failure.
    access
        .reset_failure
        .set(Some(PermissionAccessError::PlatformUnavailable));
    click(SCREEN_RECORDING_RESET, cx);
    click_modal_action(SCREEN_RECORDING.reset_confirm, cx);
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

    click(SCREEN_RECORDING_RESET, cx);
    click_modal_action(SCREEN_RECORDING.reset_confirm, cx);
    drop(access.take_reset());
    cx.run_until_parked();

    assert_eq!(
        notice(&window, ScreenRecording, cx),
        Some(RecoveryNotice::ResetFailed)
    );
    assert!(access.prepared.borrow().is_empty());
}

/// SpaceTerm resets only its own entry, so a grant held by the application that started SpaceTerm
/// Core offers no reset at all.
#[gpui::test]
fn a_grant_the_holder_cannot_reset_offers_no_reset(cx: &mut TestAppContext) {
    let access = ScriptedPermissionAccess::new(Ok(Granted), Ok(Granted));
    access.resettable.set(false);
    let (window, cx) = open_privacy(&access, cx);

    assert!(cx.debug_bounds(SCREEN_RECORDING_RESET).is_none());
    assert!(cx.debug_bounds(ACCESSIBILITY_RESET).is_none());
    assert_eq!(shown_state(&window, ScreenRecording, cx), Some("Allowed"));
    cx.update(|window_context, cx| {
        window.update(cx, |settings, cx| {
            settings.confirm_permission_reset(ScreenRecording, window_context, cx);
            settings.reset_permission(ScreenRecording, cx);
        });
    });
    cx.run_until_parked();
    assert!(
        cx.debug_bounds(modal_action(SCREEN_RECORDING.reset_confirm))
            .is_none()
    );
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

    click(ACCESSIBILITY.row.allow, cx);

    assert_eq!(
        status(&window, Accessibility, cx),
        PermissionAccessStatus::Authorization(NotGranted)
    );
    assert_eq!(
        action(&window, Accessibility, cx),
        Some(AccessAction::Allow)
    );
    let failure = explanation(&window, Accessibility, cx);
    assert_ne!(failure, guidance);
    assert_eq!(caption_kind(&window, Accessibility, cx), CaptionKind::Error);
    assert!(failure.contains("Open System Settings and allow"));
    assert!(failure.contains("Privacy & Security > Device Control and Data Access"));
    assert!(failure.contains(APPLICATION));

    access.open_failure.set(None);
    click(ACCESSIBILITY.row.allow, cx);
    assert_eq!(
        explanation(&window, Accessibility, cx),
        "Continue in System Settings."
    );
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

    click(ACCESSIBILITY.row.allow, cx);

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
    assert_eq!(
        action(&window, ScreenRecording, cx),
        Some(AccessAction::CheckAgain)
    );
    assert_eq!(
        caption_kind(&window, ScreenRecording, cx),
        CaptionKind::Error
    );
    // Neither success nor recovery is offered for a state the system did not report.
    assert!(cx.debug_bounds(SCREEN_RECORDING.row.status).is_none());
    assert!(cx.debug_bounds(SCREEN_RECORDING_RESET).is_none());

    access.set(ScreenRecording, Ok(NotGranted));
    click(SCREEN_RECORDING.row.check_again, cx);

    assert_eq!(
        status(&window, ScreenRecording, cx),
        PermissionAccessStatus::Authorization(NotGranted)
    );
    assert!(cx.debug_bounds(SCREEN_RECORDING.row.allow).is_some());
}

/// Every typed failure produces its independent fixed product copy.
#[gpui::test]
fn failure_explanations_are_distinct_fixed_copy(cx: &mut TestAppContext) {
    let access = ScriptedPermissionAccess::new(Ok(Granted), Ok(Granted));
    let (window, cx) = open_settings(Some(access.clone()), None, cx);
    for (error, expected) in [
        (
            PermissionAccessError::PlatformUnavailable,
            "Couldn\u{2019}t check Screen Recording access.",
        ),
        (
            PermissionAccessError::PlatformRejected,
            "Screen Recording access check failed.",
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

    for (permission, text, reset) in [
        (ScreenRecording, &SCREEN_RECORDING, SCREEN_RECORDING_RESET),
        (Accessibility, &ACCESSIBILITY, ACCESSIBILITY_RESET),
    ] {
        assert_eq!(
            status(&window, permission, cx),
            PermissionAccessStatus::Unsupported
        );
        assert_eq!(shown_state(&window, permission, cx), Some("Unavailable"));
        assert_eq!(
            explanation(&window, permission, cx),
            "Unavailable on this platform."
        );
        for selector in [
            text.row.allow,
            text.row.cancel,
            text.row.open_settings,
            text.row.check_again,
            reset,
        ] {
            assert!(
                cx.debug_bounds(selector).is_none(),
                "{selector} should not be offered"
            );
        }
    }
}

/// Every state ends its row in one control on the card's trailing edge, so a button and a state
/// line up, and the control stays centered on a row at its natural height.
#[gpui::test]
fn every_state_ends_the_row_in_one_control_on_the_trailing_edge(cx: &mut TestAppContext) {
    let access = ScriptedPermissionAccess::new(Ok(NotGranted), Ok(Granted));
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

            let shown = match action(&window, ScreenRecording, cx) {
                Some(AccessAction::Allow) => SCREEN_RECORDING.row.allow,
                Some(AccessAction::CheckAgain) => SCREEN_RECORDING.row.check_again,
                Some(other) => panic!("unexpected {other:?}"),
                None => SCREEN_RECORDING.row.status,
            };
            let mut bounds = |selector: &'static str| {
                cx.debug_bounds(selector)
                    .unwrap_or_else(|| panic!("{selector} should render"))
            };
            let row = bounds(SCREEN_RECORDING_ROW);
            let label = bounds("settings-row-screen-recording-access-label");
            let description = bounds("settings-row-screen-recording-access-description");
            let control = bounds(shown);
            // The other row shows its allowed state, so the two trailing edges compare a button
            // or a failure with a state.
            let reference = bounds(ACCESSIBILITY.row.status);
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
                (control.right() - reference.right()).abs() <= gpui::px(1.0),
                "{context}: every row's control should end on the same trailing edge"
            );
        }
    }
}

/// A read that fails while a reset runs cannot replace the reset with Check Again.
#[gpui::test]
fn a_failed_read_during_a_reset_keeps_the_reset_running(cx: &mut TestAppContext) {
    let access = ScriptedPermissionAccess::new(Ok(Granted), Ok(Granted));
    let (window, cx) = open_privacy(&access, cx);
    click(SCREEN_RECORDING_RESET, cx);
    click_modal_action(SCREEN_RECORDING.reset_confirm, cx);

    access.set(
        ScreenRecording,
        Err(PermissionAccessError::PlatformUnavailable),
    );
    return_to_settings(cx);

    assert_eq!(
        status(&window, ScreenRecording, cx),
        PermissionAccessStatus::Failed(PermissionAccessError::PlatformUnavailable)
    );
    assert_eq!(shown_state(&window, ScreenRecording, cx), Some("Waiting"));
    assert_eq!(action(&window, ScreenRecording, cx), None);
    assert_eq!(
        explanation(&window, ScreenRecording, cx),
        "Resetting Screen Recording\u{2026}"
    );
}
