//! Screen Recording and Device Control access in the Privacy section, driven through a scripted
//! capability.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gpui::{Entity, Modifiers, TestAppContext, VisualTestContext};

use crate::appearance::{Appearance, SettingsDocument};
use crate::platform::appearance::testing::RecordingAppearancePlatform;
use crate::platform::computer_use_access::{
    ComputerUseAccess, ComputerUseAccessError, ComputerUseAccessObservation,
    ComputerUseAccessSubscription, ComputerUseAuthorization, ComputerUsePermission,
    ComputerUseResetCompletion,
};
use crate::platform::window_movement::RecordingOperatingSystemWindowDragPlatform;
use crate::ui::appearance_runtime;

use super::computer_use::{
    ComputerUseAccessAction, ComputerUseAccessStatus, DEVICE_CONTROL, RecoveryNotice,
    SCREEN_RECORDING, TroubleshootAvailability,
};
use super::test_support::MemoryStorage;
use super::{PermissionCapabilities, SettingsRowId, SettingsSectionId, SettingsWindow};

use ComputerUseAuthorization::{Granted, NotGranted};
use ComputerUsePermission::{Accessibility, ScreenRecording};

const SCREEN_RECORDING_ROW: &str = "settings-row-screen-recording-access";
const DEVICE_CONTROL_ROW: &str = "settings-row-device-control-access";
const APPLICATION: &str = crate::application_identity::ApplicationIdentity::current().display_name();

type Authorization = Result<ComputerUseAuthorization, ComputerUseAccessError>;

/// A capability whose authorization, failures, and pending resets the test controls.
struct ScriptedComputerUseAccess {
    screen_recording: Cell<Authorization>,
    accessibility: Cell<Authorization>,
    request_failure: Cell<Option<ComputerUseAccessError>>,
    open_failure: Cell<Option<ComputerUseAccessError>>,
    resettable: Cell<bool>,
    reset_failure: Cell<Option<ComputerUseAccessError>>,
    requests: RefCell<Vec<ComputerUsePermission>>,
    opened: RefCell<Vec<ComputerUsePermission>>,
    resets: RefCell<Vec<ComputerUsePermission>>,
    pending_resets: RefCell<Vec<ComputerUseResetCompletion>>,
    /// Reports a change to the observing window, as the system does after a grant changes.
    changes: RefCell<Option<async_channel::Sender<()>>>,
    observing: Rc<Cell<bool>>,
}

/// Records that the observing window still holds its observation.
struct ScriptedSubscription(Rc<Cell<bool>>);

impl ComputerUseAccessSubscription for ScriptedSubscription {}

impl Drop for ScriptedSubscription {
    fn drop(&mut self) {
        self.0.set(false);
    }
}

impl ScriptedComputerUseAccess {
    fn new(screen_recording: Authorization, accessibility: Authorization) -> Rc<Self> {
        Rc::new(Self {
            screen_recording: Cell::new(screen_recording),
            accessibility: Cell::new(accessibility),
            request_failure: Cell::new(None),
            open_failure: Cell::new(None),
            resettable: Cell::new(true),
            reset_failure: Cell::new(None),
            requests: RefCell::default(),
            opened: RefCell::default(),
            resets: RefCell::default(),
            pending_resets: RefCell::default(),
            changes: RefCell::default(),
            observing: Rc::default(),
        })
    }

    /// Reports a change the way the system does: without the window becoming active.
    fn report_change(&self) {
        self.changes
            .borrow()
            .as_ref()
            .expect("the window should observe changes")
            .try_send(())
            .expect("the window should receive the change");
    }

    fn set(&self, permission: ComputerUsePermission, authorization: Authorization) {
        match permission {
            ScreenRecording => self.screen_recording.set(authorization),
            Accessibility => self.accessibility.set(authorization),
        }
    }

    fn take_reset(&self) -> ComputerUseResetCompletion {
        self.pending_resets
            .borrow_mut()
            .pop()
            .expect("a reset should be awaiting its result")
    }
}

impl ComputerUseAccess for ScriptedComputerUseAccess {
    fn authorization(&self, permission: ComputerUsePermission) -> Authorization {
        match permission {
            ScreenRecording => self.screen_recording.get(),
            Accessibility => self.accessibility.get(),
        }
    }

    fn observe(&self) -> Option<ComputerUseAccessObservation> {
        let (sender, changed) = async_channel::bounded(1);
        *self.changes.borrow_mut() = Some(sender);
        self.observing.set(true);
        Some(ComputerUseAccessObservation {
            changed,
            subscription: Box::new(ScriptedSubscription(self.observing.clone())),
        })
    }

    fn request_authorization(
        &self,
        permission: ComputerUsePermission,
    ) -> Result<(), ComputerUseAccessError> {
        self.requests.borrow_mut().push(permission);
        self.request_failure.get().map_or(Ok(()), Err)
    }

    fn open_settings(
        &self,
        permission: ComputerUsePermission,
    ) -> Result<(), ComputerUseAccessError> {
        self.opened.borrow_mut().push(permission);
        self.open_failure.get().map_or(Ok(()), Err)
    }

    fn can_reset(&self) -> bool {
        self.resettable.get()
    }

    fn reset(
        &self,
        permission: ComputerUsePermission,
        completion: ComputerUseResetCompletion,
    ) -> Result<(), ComputerUseAccessError> {
        self.resets.borrow_mut().push(permission);
        if let Some(error) = self.reset_failure.get() {
            return Err(error);
        }
        self.pending_resets.borrow_mut().push(completion);
        Ok(())
    }
}

fn open_settings(
    access: Option<Rc<dyn ComputerUseAccess>>,
    cx: &mut TestAppContext,
) -> (Entity<SettingsWindow>, &mut VisualTestContext) {
    let settings = crate::settings::UserSettings::load(MemoryStorage::with_document(
        &SettingsDocument::default(),
    ));
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
            PermissionCapabilities {
                microphone: None,
                computer_use: access,
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

fn open_privacy<'a>(
    access: &Rc<ScriptedComputerUseAccess>,
    cx: &'a mut TestAppContext,
) -> (Entity<SettingsWindow>, &'a mut VisualTestContext) {
    let (window, cx) = open_settings(Some(access.clone()), cx);
    click("settings-navigation-settings-section-privacy", cx);
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

/// The selector the modal renderer gives one action.
fn modal_action(debug_identity: &str) -> &'static str {
    format!("modal-action-{debug_identity}").leak()
}

fn click_modal_action(debug_identity: &str, cx: &mut VisualTestContext) {
    click(modal_action(debug_identity), cx);
}

fn status(
    window: &Entity<SettingsWindow>,
    permission: ComputerUsePermission,
    cx: &mut VisualTestContext,
) -> ComputerUseAccessStatus {
    window.read_with(cx, |window, _| {
        window.computer_use_access.row(permission).status()
    })
}

fn action(
    window: &Entity<SettingsWindow>,
    permission: ComputerUsePermission,
    cx: &mut VisualTestContext,
) -> Option<ComputerUseAccessAction> {
    window.read_with(cx, |window, _| {
        window
            .computer_use_access
            .row(permission)
            .presentation()
            .action
    })
}

fn troubleshoot(
    window: &Entity<SettingsWindow>,
    permission: ComputerUsePermission,
    cx: &mut VisualTestContext,
) -> Option<TroubleshootAvailability> {
    window.read_with(cx, |window, _| {
        window
            .computer_use_access
            .row(permission)
            .presentation()
            .troubleshoot
    })
}

fn explanation(
    window: &Entity<SettingsWindow>,
    permission: ComputerUsePermission,
    cx: &mut VisualTestContext,
) -> String {
    window.read_with(cx, |window, _| {
        window
            .computer_use_access
            .row(permission)
            .presentation()
            .explanation
            .to_string()
    })
}

fn notice(
    window: &Entity<SettingsWindow>,
    permission: ComputerUsePermission,
    cx: &mut VisualTestContext,
) -> Option<RecoveryNotice> {
    window.read_with(cx, |window, _| {
        window.computer_use_access.row(permission).notice()
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
    let access = ScriptedComputerUseAccess::new(Ok(NotGranted), Ok(Granted));
    let (window, cx) = open_privacy(&access, cx);

    for selector in [
        "settings-section-privacy-group-permissions",
        "settings-row-microphone-access",
        SCREEN_RECORDING_ROW,
        SCREEN_RECORDING.control,
        SCREEN_RECORDING.state_not_allowed,
        SCREEN_RECORDING.request,
        SCREEN_RECORDING.troubleshoot,
        DEVICE_CONTROL_ROW,
        DEVICE_CONTROL.control,
        DEVICE_CONTROL.state_allowed,
        DEVICE_CONTROL.troubleshoot,
    ] {
        assert!(
            cx.debug_bounds(selector).is_some(),
            "{selector} should render"
        );
    }
    let group = cx
        .debug_bounds("settings-section-privacy-group-permissions")
        .expect("group bounds");
    for row in [SCREEN_RECORDING_ROW, DEVICE_CONTROL_ROW] {
        let bounds = cx.debug_bounds(row).expect("row bounds");
        assert!(group.contains(&bounds.origin), "{row} should share the group");
    }
    // The guidance names the application the person turns on in System Settings.
    assert!(explanation(&window, ScreenRecording, cx).contains(APPLICATION));
    // Reading authorization is not a request: the system prompt waits for the explicit action.
    assert!(access.requests.borrow().is_empty());
    assert!(access.opened.borrow().is_empty());
}

#[gpui::test]
fn settings_search_reveals_a_permission_from_another_section(cx: &mut TestAppContext) {
    let access = ScriptedComputerUseAccess::new(Ok(Granted), Ok(NotGranted));
    let (window, cx) = open_settings(Some(access), cx);

    cx.update(|_, cx| {
        let search = window.read(cx).search.clone();
        search.update(cx, |search, cx| {
            search.set_value("accessibility".to_owned(), cx);
        });
    });
    cx.run_until_parked();

    window.read_with(cx, |settings, _| {
        assert_eq!(settings.active_section, SettingsSectionId::Privacy);
        assert_eq!(settings.revealed, Some(SettingsRowId::DeviceControlAccess));
    });
    assert!(cx.debug_bounds(DEVICE_CONTROL_ROW).is_some());
    assert!(cx.debug_bounds(DEVICE_CONTROL.request).is_some());
}

/// First authorization: the request prompts once, System Settings is where the grant is made, and
/// only a later read reports it.
#[gpui::test]
fn a_request_leads_to_system_settings_and_only_a_read_reports_the_grant(cx: &mut TestAppContext) {
    let access = ScriptedComputerUseAccess::new(Ok(NotGranted), Ok(NotGranted));
    let (window, cx) = open_privacy(&access, cx);

    click(SCREEN_RECORDING.request, cx);

    assert_eq!(*access.requests.borrow(), [ScreenRecording]);
    assert_eq!(
        action(&window, ScreenRecording, cx),
        Some(ComputerUseAccessAction::OpenSettings)
    );
    // The other permission is untouched by this row's request.
    assert_eq!(
        action(&window, Accessibility, cx),
        Some(ComputerUseAccessAction::Request)
    );
    let guidance = explanation(&window, ScreenRecording, cx);
    assert!(guidance.contains("Privacy & Security > Screen & System Audio Recording"));
    assert!(guidance.contains(&format!("quit and reopen {APPLICATION}")));

    click(SCREEN_RECORDING.open_settings, cx);

    assert_eq!(*access.opened.borrow(), [ScreenRecording]);
    // Opening the pane alone never reports access as allowed.
    assert_eq!(
        status(&window, ScreenRecording, cx),
        ComputerUseAccessStatus::Authorization(NotGranted)
    );
    return_to_settings(cx);
    assert_eq!(
        status(&window, ScreenRecording, cx),
        ComputerUseAccessStatus::Authorization(NotGranted)
    );

    access.set(ScreenRecording, Ok(Granted));
    return_to_settings(cx);

    assert_eq!(
        status(&window, ScreenRecording, cx),
        ComputerUseAccessStatus::Authorization(Granted)
    );
    assert_eq!(action(&window, ScreenRecording, cx), None);
    assert_eq!(
        troubleshoot(&window, ScreenRecording, cx),
        Some(TroubleshootAvailability::Enabled)
    );
    assert!(cx.debug_bounds(SCREEN_RECORDING.state_allowed).is_some());
    assert_eq!(access.requests.borrow().len(), 1);
}

/// Revocation and reauthorization both arrive through reads on return.
#[gpui::test]
fn a_revoked_grant_is_read_on_return_and_reauthorized_through_system_settings(
    cx: &mut TestAppContext,
) {
    let access = ScriptedComputerUseAccess::new(Ok(Granted), Ok(Granted));
    let (window, cx) = open_privacy(&access, cx);

    access.set(Accessibility, Ok(NotGranted));
    return_to_settings(cx);
    assert_eq!(
        status(&window, Accessibility, cx),
        ComputerUseAccessStatus::Authorization(NotGranted)
    );
    assert_eq!(
        action(&window, Accessibility, cx),
        Some(ComputerUseAccessAction::Request)
    );

    access.set(Accessibility, Ok(Granted));
    return_to_settings(cx);
    assert_eq!(
        status(&window, Accessibility, cx),
        ComputerUseAccessStatus::Authorization(Granted)
    );
    // Device Control applies without reopening, so its guidance never asks for a relaunch first.
    access.set(Accessibility, Ok(NotGranted));
    return_to_settings(cx);
    click(DEVICE_CONTROL.request, cx);
    assert!(!explanation(&window, Accessibility, cx).contains("quit and reopen"));
}

/// The system caches a read until it reports the change, which can arrive after the window became
/// active again, so the reported change refreshes the rows without another activation.
#[gpui::test]
fn a_reported_change_refreshes_a_grant_the_activation_read_missed(cx: &mut TestAppContext) {
    let access = ScriptedComputerUseAccess::new(Ok(NotGranted), Ok(NotGranted));
    let (window, cx) = open_privacy(&access, cx);
    assert!(access.observing.get());

    access.set(Accessibility, Ok(Granted));
    cx.run_until_parked();
    assert_eq!(
        status(&window, Accessibility, cx),
        ComputerUseAccessStatus::Authorization(NotGranted)
    );

    access.report_change();
    cx.run_until_parked();
    assert_eq!(
        status(&window, Accessibility, cx),
        ComputerUseAccessStatus::Authorization(Granted)
    );
    assert!(cx.debug_bounds(DEVICE_CONTROL.state_allowed).is_some());

    // Closing the window ends the observation with it.
    drop(window);
    cx.update(|window, _| window.remove_window());
    cx.run_until_parked();
    assert!(!access.observing.get());
}

/// An apparently allowed grant can still fail, so Troubleshoot reaches System Settings from it.
#[gpui::test]
fn troubleshooting_an_allowed_grant_opens_system_settings(cx: &mut TestAppContext) {
    let access = ScriptedComputerUseAccess::new(Ok(Granted), Ok(Granted));
    let (window, cx) = open_privacy(&access, cx);

    click(SCREEN_RECORDING.troubleshoot, cx);
    assert!(cx.debug_bounds(SCREEN_RECORDING.troubleshoot_reset).is_none());
    assert!(
        cx.debug_bounds(modal_action(SCREEN_RECORDING.troubleshoot_reset))
        .is_some(),
        "the guide should offer a reset when the capability supports one"
    );
    click_modal_action(SCREEN_RECORDING.troubleshoot_open_settings, cx);

    assert_eq!(*access.opened.borrow(), [ScreenRecording]);
    assert!(access.resets.borrow().is_empty());
    assert_eq!(
        status(&window, ScreenRecording, cx),
        ComputerUseAccessStatus::Authorization(Granted)
    );
}

/// Stale-grant recovery: a confirmed reset clears one permission of this application, then the
/// row offers a fresh request.
#[gpui::test]
fn a_confirmed_reset_clears_one_permission_and_offers_a_new_request(cx: &mut TestAppContext) {
    let access = ScriptedComputerUseAccess::new(Ok(Granted), Ok(Granted));
    let (window, cx) = open_privacy(&access, cx);

    click(DEVICE_CONTROL.troubleshoot, cx);
    click_modal_action(DEVICE_CONTROL.troubleshoot_reset, cx);
    // The guide leads to a confirmation; nothing is reset before the person confirms.
    assert!(access.resets.borrow().is_empty());
    click_modal_action(DEVICE_CONTROL.reset_confirm, cx);

    assert_eq!(*access.resets.borrow(), [Accessibility]);
    assert_eq!(
        notice(&window, Accessibility, cx),
        Some(RecoveryNotice::Resetting)
    );
    assert_eq!(
        troubleshoot(&window, Accessibility, cx),
        Some(TroubleshootAvailability::Busy)
    );
    // The grant can disappear before the reset reports, and a return to the window meanwhile
    // keeps reporting the reset as running.
    access.set(Accessibility, Ok(NotGranted));
    return_to_settings(cx);
    assert_eq!(
        notice(&window, Accessibility, cx),
        Some(RecoveryNotice::Resetting)
    );

    let completion = access.take_reset();
    completion(Ok(()));
    cx.run_until_parked();

    assert_eq!(
        status(&window, Accessibility, cx),
        ComputerUseAccessStatus::Authorization(NotGranted)
    );
    assert_eq!(
        action(&window, Accessibility, cx),
        Some(ComputerUseAccessAction::Request)
    );
    assert_eq!(
        notice(&window, Accessibility, cx),
        Some(RecoveryNotice::ResetCompleted)
    );
    assert_eq!(
        troubleshoot(&window, Accessibility, cx),
        Some(TroubleshootAvailability::Enabled)
    );
    // The other permission keeps its grant and its presentation.
    assert_eq!(
        status(&window, ScreenRecording, cx),
        ComputerUseAccessStatus::Authorization(Granted)
    );
    assert_eq!(notice(&window, ScreenRecording, cx), None);
}

#[gpui::test]
fn a_cancelled_reset_changes_nothing(cx: &mut TestAppContext) {
    let access = ScriptedComputerUseAccess::new(Ok(Granted), Ok(Granted));
    let (window, cx) = open_privacy(&access, cx);
    let guidance = explanation(&window, ScreenRecording, cx);

    click(SCREEN_RECORDING.troubleshoot, cx);
    click_modal_action(SCREEN_RECORDING.troubleshoot_reset, cx);
    click_modal_action(SCREEN_RECORDING.reset_cancel, cx);

    assert!(access.resets.borrow().is_empty());
    assert_eq!(explanation(&window, ScreenRecording, cx), guidance);
}

#[gpui::test]
fn a_failed_reset_reports_failure_and_explains_the_manual_recovery(cx: &mut TestAppContext) {
    let access = ScriptedComputerUseAccess::new(Ok(Granted), Ok(Granted));
    let (window, cx) = open_privacy(&access, cx);

    click(SCREEN_RECORDING.troubleshoot, cx);
    click_modal_action(SCREEN_RECORDING.troubleshoot_reset, cx);
    click_modal_action(SCREEN_RECORDING.reset_confirm, cx);
    access.take_reset()(Err(ComputerUseAccessError::PlatformRejected));
    cx.run_until_parked();

    assert_eq!(
        notice(&window, ScreenRecording, cx),
        Some(RecoveryNotice::ResetFailed)
    );
    let failure = explanation(&window, ScreenRecording, cx);
    assert!(failure.contains("could not reset"));
    assert!(failure.contains("Privacy & Security > Screen & System Audio Recording"));
    assert!(!failure.contains("PlatformRejected"));

    // A reset that cannot start reports the same failure.
    access
        .reset_failure
        .set(Some(ComputerUseAccessError::PlatformUnavailable));
    click(SCREEN_RECORDING.troubleshoot, cx);
    click_modal_action(SCREEN_RECORDING.troubleshoot_reset, cx);
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
    let access = ScriptedComputerUseAccess::new(Ok(Granted), Ok(Granted));
    let (window, cx) = open_privacy(&access, cx);

    click(SCREEN_RECORDING.troubleshoot, cx);
    click_modal_action(SCREEN_RECORDING.troubleshoot_reset, cx);
    click_modal_action(SCREEN_RECORDING.reset_confirm, cx);
    drop(access.take_reset());
    cx.run_until_parked();

    assert_eq!(
        notice(&window, ScreenRecording, cx),
        Some(RecoveryNotice::ResetFailed)
    );
}

/// A running bundle that is not this build's identity offers the manual steps only.
#[gpui::test]
fn the_guide_omits_reset_when_the_identity_cannot_be_reset(cx: &mut TestAppContext) {
    let access = ScriptedComputerUseAccess::new(Ok(NotGranted), Ok(Granted));
    access.resettable.set(false);
    let (window, cx) = open_privacy(&access, cx);

    click(SCREEN_RECORDING.troubleshoot, cx);

    assert!(
        cx.debug_bounds(modal_action(SCREEN_RECORDING.troubleshoot_done))
        .is_some()
    );
    assert!(
        cx.debug_bounds(modal_action(SCREEN_RECORDING.troubleshoot_reset))
        .is_none()
    );
    click_modal_action(SCREEN_RECORDING.troubleshoot_done, cx);
    cx.update(|_, cx| {
        window.update(cx, |settings, cx| {
            settings.reset_computer_use_access(ScreenRecording, cx);
        });
    });
    assert!(access.resets.borrow().is_empty());
    assert!(access.opened.borrow().is_empty());
}

#[gpui::test]
fn a_failed_open_keeps_the_status_and_explains_where_to_go(cx: &mut TestAppContext) {
    let access = ScriptedComputerUseAccess::new(Ok(NotGranted), Ok(NotGranted));
    access
        .open_failure
        .set(Some(ComputerUseAccessError::PlatformRejected));
    let (window, cx) = open_privacy(&access, cx);
    click(DEVICE_CONTROL.request, cx);
    let guidance = explanation(&window, Accessibility, cx);

    click(DEVICE_CONTROL.open_settings, cx);

    assert_eq!(
        status(&window, Accessibility, cx),
        ComputerUseAccessStatus::Authorization(NotGranted)
    );
    assert_eq!(
        notice(&window, Accessibility, cx),
        Some(RecoveryNotice::OpenFailed)
    );
    let failure = explanation(&window, Accessibility, cx);
    assert_ne!(failure, guidance);
    assert!(failure.contains("Privacy & Security > Device Control and Data Access"));
    assert!(failure.contains(APPLICATION));

    access.open_failure.set(None);
    click(DEVICE_CONTROL.open_settings, cx);
    assert_eq!(explanation(&window, Accessibility, cx), guidance);
}

#[gpui::test]
fn an_unreadable_authorization_reports_failure_and_checks_again(cx: &mut TestAppContext) {
    let access = ScriptedComputerUseAccess::new(
        Err(ComputerUseAccessError::PlatformUnavailable),
        Ok(Granted),
    );
    let (window, cx) = open_privacy(&access, cx);

    assert_eq!(
        status(&window, ScreenRecording, cx),
        ComputerUseAccessStatus::Failed(ComputerUseAccessError::PlatformUnavailable)
    );
    assert!(cx.debug_bounds(SCREEN_RECORDING.state_unavailable).is_some());
    assert_eq!(
        action(&window, ScreenRecording, cx),
        Some(ComputerUseAccessAction::CheckAgain)
    );
    // Neither success nor recovery is offered for a state the system did not report.
    assert_eq!(troubleshoot(&window, ScreenRecording, cx), None);
    assert!(cx.debug_bounds(SCREEN_RECORDING.troubleshoot).is_none());

    access.set(ScreenRecording, Ok(NotGranted));
    click(SCREEN_RECORDING.check_again, cx);

    assert_eq!(
        status(&window, ScreenRecording, cx),
        ComputerUseAccessStatus::Authorization(NotGranted)
    );
    assert!(cx.debug_bounds(SCREEN_RECORDING.request).is_some());
}

#[gpui::test]
fn a_rejected_request_reports_failure(cx: &mut TestAppContext) {
    let access = ScriptedComputerUseAccess::new(Ok(NotGranted), Ok(NotGranted));
    access
        .request_failure
        .set(Some(ComputerUseAccessError::OffMainThread));
    let (window, cx) = open_privacy(&access, cx);

    click(SCREEN_RECORDING.request, cx);

    assert_eq!(
        status(&window, ScreenRecording, cx),
        ComputerUseAccessStatus::Failed(ComputerUseAccessError::OffMainThread)
    );
    assert_eq!(
        action(&window, ScreenRecording, cx),
        Some(ComputerUseAccessAction::CheckAgain)
    );
}

/// Every failure explanation is fixed product copy, never a native error's text.
#[gpui::test]
fn failure_explanations_are_distinct_fixed_copy(cx: &mut TestAppContext) {
    let access = ScriptedComputerUseAccess::new(Ok(Granted), Ok(Granted));
    let (window, cx) = open_settings(Some(access.clone()), cx);
    let mut explanations = Vec::new();
    for error in [
        ComputerUseAccessError::OffMainThread,
        ComputerUseAccessError::PlatformUnavailable,
        ComputerUseAccessError::PlatformRejected,
    ] {
        access.set(ScreenRecording, Err(error));
        cx.update(|_, cx| {
            window.update(cx, |settings, cx| settings.refresh_computer_use_access(cx));
        });
        let presented = explanation(&window, ScreenRecording, cx);
        assert_ne!(presented, error.to_string());
        explanations.push(presented);
    }
    explanations.dedup();
    assert_eq!(explanations.len(), 3);
}

#[gpui::test]
fn a_host_without_the_capability_presents_both_permissions_as_unavailable(
    cx: &mut TestAppContext,
) {
    let (window, cx) = open_settings(None, cx);
    click("settings-navigation-settings-section-privacy", cx);

    for (permission, text) in [
        (ScreenRecording, &SCREEN_RECORDING),
        (Accessibility, &DEVICE_CONTROL),
    ] {
        assert_eq!(
            status(&window, permission, cx),
            ComputerUseAccessStatus::Unsupported
        );
        assert!(cx.debug_bounds(text.state_unavailable).is_some());
        assert_eq!(action(&window, permission, cx), None);
        assert_eq!(troubleshoot(&window, permission, cx), None);
        for selector in [
            text.request,
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

/// The badge and two buttons share the control column with wrapped guidance, so every state keeps
/// each row at its natural height with the control centered on it.
#[gpui::test]
fn permission_rows_keep_their_natural_height_with_two_actions(cx: &mut TestAppContext) {
    let access = ScriptedComputerUseAccess::new(Ok(NotGranted), Ok(NotGranted));
    let (window, cx) = open_privacy(&access, cx);
    let states = [
        Ok(NotGranted),
        Ok(Granted),
        Err(ComputerUseAccessError::PlatformRejected),
    ];
    for width in [super::WINDOW_WIDTH, 1100.0, 1400.0] {
        cx.simulate_resize(gpui::size(gpui::px(width), gpui::px(super::WINDOW_HEIGHT)));
        cx.run_until_parked();
        for state in states {
            access.set(ScreenRecording, state);
            cx.update(|_, cx| {
                window.update(cx, |settings, cx| settings.refresh_computer_use_access(cx));
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
