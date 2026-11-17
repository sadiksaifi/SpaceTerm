//! Microphone and camera access in the Privacy section, driven through a scripted capability.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gpui::{Entity, Modifiers, TestAppContext, VisualTestContext};

use crate::appearance::Appearance;
use crate::platform::appearance::testing::RecordingAppearancePlatform;
use crate::platform::capture_access::{
    CaptureAccess, CaptureAccessError, CaptureAuthorization, CaptureAuthorizationCompletion,
    CaptureDevice, CaptureResetCompletion,
};
use crate::platform::window_movement::RecordingOperatingSystemWindowDragPlatform;
use crate::settings::SettingsDocument;
use crate::ui::appearance_runtime;

use super::super::access_row::{AccessAction, AccessControl, CaptionKind};
use super::super::capture::{CAMERA, CaptureAccessStatus, CaptureReset, MICROPHONE};
use super::super::{SettingsSectionId, SettingsWindow};
use crate::settings::storage::testing::MemoryStorage;

const ROW_SELECTOR: &str = "settings-row-microphone-access";
const CONTROL_SELECTOR: &str = MICROPHONE.row.control;
const STATUS_SELECTOR: &str = MICROPHONE.row.status;
const ALLOW_SELECTOR: &str = MICROPHONE.row.allow;
const OPEN_SETTINGS_SELECTOR: &str = MICROPHONE.row.open_settings;
const CHECK_AGAIN_SELECTOR: &str = MICROPHONE.row.check_again;
const RESET_SELECTOR: &str = "settings-row-microphone-access-reset";

/// A capability whose authorization, failures, and pending decisions the test controls. It
/// answers every device the same way and records which device each action named.
struct ScriptedCaptureAccess {
    authorization: Cell<Result<CaptureAuthorization, CaptureAccessError>>,
    camera_authorization: Cell<Option<Result<CaptureAuthorization, CaptureAccessError>>>,
    request_failure: Cell<Option<CaptureAccessError>>,
    open_failure: Cell<Option<CaptureAccessError>>,
    pending: RefCell<Vec<CaptureAuthorizationCompletion>>,
    requests: Cell<usize>,
    opened: Cell<usize>,
    devices: RefCell<Vec<CaptureDevice>>,
    resettable: Cell<bool>,
    reset_failure: Cell<Option<CaptureAccessError>>,
    resets: RefCell<Vec<CaptureDevice>>,
    pending_resets: RefCell<Vec<CaptureResetCompletion>>,
}

impl ScriptedCaptureAccess {
    fn new(authorization: Result<CaptureAuthorization, CaptureAccessError>) -> Rc<Self> {
        Rc::new(Self {
            authorization: Cell::new(authorization),
            camera_authorization: Cell::new(None),
            request_failure: Cell::new(None),
            open_failure: Cell::new(None),
            pending: RefCell::default(),
            requests: Cell::new(0),
            opened: Cell::new(0),
            devices: RefCell::default(),
            resettable: Cell::new(true),
            reset_failure: Cell::new(None),
            resets: RefCell::default(),
            pending_resets: RefCell::default(),
        })
    }

    fn take_reset(&self) -> CaptureResetCompletion {
        self.pending_resets
            .borrow_mut()
            .pop()
            .expect("a reset should be awaiting its result")
    }

    fn take_completion(&self) -> CaptureAuthorizationCompletion {
        self.pending
            .borrow_mut()
            .pop()
            .expect("a request should be awaiting its decision")
    }
}

impl CaptureAccess for ScriptedCaptureAccess {
    fn authorization(
        &self,
        device: CaptureDevice,
    ) -> Result<CaptureAuthorization, CaptureAccessError> {
        if device == CaptureDevice::Camera {
            self.camera_authorization
                .get()
                .unwrap_or_else(|| self.authorization.get())
        } else {
            self.authorization.get()
        }
    }

    fn request_authorization(
        &self,
        device: CaptureDevice,
        completion: CaptureAuthorizationCompletion,
    ) -> Result<(), CaptureAccessError> {
        self.requests.set(self.requests.get() + 1);
        self.devices.borrow_mut().push(device);
        if let Some(error) = self.request_failure.get() {
            return Err(error);
        }
        self.pending.borrow_mut().push(completion);
        Ok(())
    }

    fn open_settings(&self, device: CaptureDevice) -> Result<(), CaptureAccessError> {
        self.opened.set(self.opened.get() + 1);
        self.devices.borrow_mut().push(device);
        self.open_failure.get().map_or(Ok(()), Err)
    }

    fn can_reset(&self) -> bool {
        self.resettable.get()
    }

    fn reset(
        &self,
        device: CaptureDevice,
        completion: CaptureResetCompletion,
    ) -> Result<(), CaptureAccessError> {
        self.resets.borrow_mut().push(device);
        if let Some(error) = self.reset_failure.get() {
            return Err(error);
        }
        self.pending_resets.borrow_mut().push(completion);
        Ok(())
    }
}

fn open_settings(
    access: Option<Rc<dyn CaptureAccess>>,
    cx: &mut TestAppContext,
) -> (Entity<SettingsWindow>, &mut VisualTestContext) {
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
            super::super::PermissionCapabilities {
                capture: access,
                ..super::super::PermissionCapabilities::default()
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

/// Opens Settings on the Privacy section with a scripted capability.
fn open_privacy<'a>(
    access: &Rc<ScriptedCaptureAccess>,
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

fn status(window: &Entity<SettingsWindow>, cx: &mut VisualTestContext) -> CaptureAccessStatus {
    window.read_with(cx, |window, _| window.microphone_access.status)
}

fn action(window: &Entity<SettingsWindow>, cx: &mut VisualTestContext) -> Option<AccessAction> {
    window.read_with(cx, |window, _| {
        match window.microphone_access.presentation().control {
            AccessControl::Action(action) => Some(action),
            AccessControl::Status { .. } => None,
        }
    })
}

/// The state the row shows in place of a button, when it has no next step.
fn shown_state(
    window: &Entity<SettingsWindow>,
    cx: &mut VisualTestContext,
) -> Option<&'static str> {
    window.read_with(cx, |window, _| {
        match window.microphone_access.presentation().control {
            AccessControl::Status { label, .. } => Some(label),
            AccessControl::Action(_) => None,
        }
    })
}

fn explanation(window: &Entity<SettingsWindow>, cx: &mut VisualTestContext) -> String {
    window.read_with(cx, |window, _| {
        window.microphone_access.presentation().caption.to_string()
    })
}

fn caption_kind(window: &Entity<SettingsWindow>, cx: &mut VisualTestContext) -> CaptionKind {
    window.read_with(cx, |window, _| {
        window.microphone_access.presentation().caption_kind
    })
}

fn click_modal_action(debug_identity: &str, cx: &mut VisualTestContext) {
    click(format!("modal-action-{debug_identity}").leak(), cx);
}

/// GPUI retains a selector once drawn, so absence is only provable for one never rendered.
fn assert_no_action_rendered(cx: &mut VisualTestContext) {
    for selector in [ALLOW_SELECTOR, OPEN_SETTINGS_SELECTOR, CHECK_AGAIN_SELECTOR] {
        assert!(
            cx.debug_bounds(selector).is_none(),
            "{selector} should not be offered"
        );
    }
}

#[gpui::test]
fn privacy_section_presents_microphone_access_as_a_row_in_its_group(cx: &mut TestAppContext) {
    let access = ScriptedCaptureAccess::new(Ok(CaptureAuthorization::NotDetermined));
    let (window, cx) = open_privacy(&access, cx);

    assert_eq!(
        window.read_with(cx, |window, _| window.active_section),
        SettingsSectionId::Privacy
    );
    for selector in [
        "settings-section-privacy",
        "settings-section-privacy-group-permissions",
        ROW_SELECTOR,
        "settings-row-microphone-access-label",
        CONTROL_SELECTOR,
        ALLOW_SELECTOR,
    ] {
        assert!(
            cx.debug_bounds(selector).is_some(),
            "{selector} should render"
        );
    }
    let row = cx.debug_bounds(ROW_SELECTOR).expect("row bounds");
    let control = cx.debug_bounds(CONTROL_SELECTOR).expect("control bounds");
    assert!(row.contains(&control.origin) && control.right() <= row.right());
    // Reading authorization is not a request: the system prompt waits for the explicit action.
    assert_eq!(access.requests.get(), 0);
}

#[gpui::test]
fn requesting_access_waits_for_the_native_decision_on_the_foreground(cx: &mut TestAppContext) {
    let access = ScriptedCaptureAccess::new(Ok(CaptureAuthorization::NotDetermined));
    let (window, cx) = open_privacy(&access, cx);

    click(ALLOW_SELECTOR, cx);

    assert_eq!(access.requests.get(), 1);
    assert_eq!(status(&window, cx), CaptureAccessStatus::Requesting);
    assert_eq!(action(&window, cx), None);
    assert_eq!(shown_state(&window, cx), Some("Waiting"));
    assert_eq!(explanation(&window, cx), "Respond to the system prompt.");
    assert_eq!(caption_kind(&window, cx), CaptionKind::Waiting);
    assert!(
        cx.debug_bounds(format!("{}-track", MICROPHONE.row.progress).leak())
            .is_some()
    );

    // No button remains to stack a second prompt.
    cx.update(|_, cx| {
        window.update(cx, |settings, cx| {
            settings.request_capture_access(CaptureDevice::Microphone, cx);
        });
    });
    assert_eq!(access.requests.get(), 1);

    // The system prompt activating and returning the window does not overwrite the pending request.
    access
        .authorization
        .set(Ok(CaptureAuthorization::Authorized));
    cx.deactivate_window();
    cx.update(|window, _| window.activate_window());
    cx.run_until_parked();
    assert_eq!(status(&window, cx), CaptureAccessStatus::Requesting);

    // The decision waits for the foreground task to resume.
    let completion = access.take_completion();
    completion(CaptureAuthorization::Authorized);
    assert_eq!(status(&window, cx), CaptureAccessStatus::Requesting);

    cx.run_until_parked();

    assert_eq!(
        status(&window, cx),
        CaptureAccessStatus::Authorization(CaptureAuthorization::Authorized)
    );
    assert_eq!(action(&window, cx), None);
    assert_eq!(shown_state(&window, cx), Some("Allowed"));
    assert!(cx.debug_bounds(STATUS_SELECTOR).is_some());
}

#[gpui::test]
fn a_denial_from_the_prompt_offers_system_settings_rather_than_prompting_again(
    cx: &mut TestAppContext,
) {
    let access = ScriptedCaptureAccess::new(Ok(CaptureAuthorization::NotDetermined));
    let (window, cx) = open_privacy(&access, cx);

    click(ALLOW_SELECTOR, cx);
    let completion = access.take_completion();
    completion(CaptureAuthorization::Denied);
    cx.run_until_parked();

    assert_eq!(
        status(&window, cx),
        CaptureAccessStatus::Authorization(CaptureAuthorization::Denied)
    );
    assert_eq!(action(&window, cx), Some(AccessAction::OpenSettings));
    click(OPEN_SETTINGS_SELECTOR, cx);
    assert_eq!(access.opened.get(), 1);
    assert_eq!(access.requests.get(), 1);
}

#[gpui::test]
fn a_request_dropped_without_a_decision_reads_authorization_again(cx: &mut TestAppContext) {
    let access = ScriptedCaptureAccess::new(Ok(CaptureAuthorization::NotDetermined));
    let (window, cx) = open_privacy(&access, cx);

    click(ALLOW_SELECTOR, cx);
    access
        .authorization
        .set(Ok(CaptureAuthorization::Restricted));
    drop(access.take_completion());
    cx.run_until_parked();

    assert_eq!(
        status(&window, cx),
        CaptureAccessStatus::Authorization(CaptureAuthorization::Restricted)
    );
}

#[gpui::test]
fn denied_access_recovers_through_system_settings(cx: &mut TestAppContext) {
    let access = ScriptedCaptureAccess::new(Ok(CaptureAuthorization::Denied));
    let (window, cx) = open_privacy(&access, cx);

    assert_eq!(action(&window, cx), Some(AccessAction::OpenSettings));
    assert!(cx.debug_bounds(ALLOW_SELECTOR).is_none());
    let guidance = explanation(&window, cx);

    click(OPEN_SETTINGS_SELECTOR, cx);

    assert_eq!(access.opened.get(), 1);
    assert_eq!(access.requests.get(), 0);
    assert_eq!(explanation(&window, cx), guidance);

    // Returning from System Settings reads the decision the person made there.
    access
        .authorization
        .set(Ok(CaptureAuthorization::Authorized));
    cx.deactivate_window();
    cx.update(|window, _| window.activate_window());
    cx.run_until_parked();

    assert_eq!(
        status(&window, cx),
        CaptureAccessStatus::Authorization(CaptureAuthorization::Authorized)
    );
    assert_eq!(action(&window, cx), None);
}

#[gpui::test]
fn a_failed_recovery_keeps_the_denial_and_explains_where_to_go(cx: &mut TestAppContext) {
    let access = ScriptedCaptureAccess::new(Ok(CaptureAuthorization::Denied));
    access
        .open_failure
        .set(Some(CaptureAccessError::PlatformRejected));
    let (window, cx) = open_privacy(&access, cx);
    let guidance = explanation(&window, cx);

    click(OPEN_SETTINGS_SELECTOR, cx);

    assert_eq!(
        status(&window, cx),
        CaptureAccessStatus::Authorization(CaptureAuthorization::Denied)
    );
    let failure = explanation(&window, cx);
    assert_ne!(failure, guidance);
    assert!(failure.contains("Privacy & Security > Microphone"));
    assert_eq!(action(&window, cx), Some(AccessAction::OpenSettings));

    access.open_failure.set(None);
    click(OPEN_SETTINGS_SELECTOR, cx);
    assert_eq!(explanation(&window, cx), guidance);
}

#[gpui::test]
fn authorized_access_is_confirmed_without_a_fix(cx: &mut TestAppContext) {
    let access = ScriptedCaptureAccess::new(Ok(CaptureAuthorization::Authorized));
    access.resettable.set(false);
    let (window, cx) = open_privacy(&access, cx);

    assert_eq!(shown_state(&window, cx), Some("Allowed"));
    assert!(cx.debug_bounds(STATUS_SELECTOR).is_some());
    assert_eq!(action(&window, cx), None);
    assert_no_action_rendered(cx);
    // SpaceTerm resets only its own entry, so a holder it cannot reset offers no reset.
    assert!(cx.debug_bounds(RESET_SELECTOR).is_none());
}

#[gpui::test]
fn restricted_access_explains_the_policy_without_prompting(cx: &mut TestAppContext) {
    let access = ScriptedCaptureAccess::new(Ok(CaptureAuthorization::Restricted));
    let (window, cx) = open_privacy(&access, cx);

    assert_eq!(shown_state(&window, cx), Some("Restricted"));
    assert!(cx.debug_bounds(RESET_SELECTOR).is_none());
    assert_eq!(action(&window, cx), None);
    assert_no_action_rendered(cx);
    assert_eq!(
        explanation(&window, cx),
        "Your device\u{2019}s administrator has blocked microphone access."
    );

    // Nothing outside the row can prompt from a restricted state either.
    cx.update(|_, cx| {
        window.update(cx, |settings, cx| {
            settings.request_capture_access(CaptureDevice::Microphone, cx);
            settings.open_capture_settings(CaptureDevice::Microphone, cx);
        });
    });
    cx.run_until_parked();
    assert_eq!((access.requests.get(), access.opened.get()), (0, 0));
}

#[gpui::test]
fn an_unreadable_authorization_reports_a_content_free_failure_and_checks_again(
    cx: &mut TestAppContext,
) {
    let access = ScriptedCaptureAccess::new(Err(CaptureAccessError::PlatformUnavailable));
    let (window, cx) = open_privacy(&access, cx);

    assert_eq!(
        status(&window, cx),
        CaptureAccessStatus::Failed(CaptureAccessError::PlatformUnavailable)
    );
    assert_eq!(caption_kind(&window, cx), CaptionKind::Error);
    assert_eq!(action(&window, cx), Some(AccessAction::CheckAgain));
    assert!(cx.debug_bounds(ALLOW_SELECTOR).is_none());

    access
        .authorization
        .set(Ok(CaptureAuthorization::NotDetermined));
    click(CHECK_AGAIN_SELECTOR, cx);

    assert_eq!(
        status(&window, cx),
        CaptureAccessStatus::Authorization(CaptureAuthorization::NotDetermined)
    );
    assert!(cx.debug_bounds(ALLOW_SELECTOR).is_some());
}

#[gpui::test]
fn a_rejected_request_reports_failure_instead_of_waiting(cx: &mut TestAppContext) {
    let access = ScriptedCaptureAccess::new(Ok(CaptureAuthorization::NotDetermined));
    access
        .request_failure
        .set(Some(CaptureAccessError::PlatformRejected));
    let (window, cx) = open_privacy(&access, cx);

    click(ALLOW_SELECTOR, cx);

    assert_eq!(
        status(&window, cx),
        CaptureAccessStatus::Failed(CaptureAccessError::PlatformRejected)
    );
    assert_eq!(action(&window, cx), Some(AccessAction::CheckAgain));
    assert!(cx.debug_bounds(CHECK_AGAIN_SELECTOR).is_some());
}

/// Every typed failure produces its independent fixed product copy.
#[gpui::test]
fn failure_explanations_are_distinct_fixed_copy(cx: &mut TestAppContext) {
    let access = ScriptedCaptureAccess::new(Ok(CaptureAuthorization::Authorized));
    let (window, cx) = open_settings(Some(access.clone()), cx);
    for (error, expected) in [
        (
            CaptureAccessError::PlatformUnavailable,
            "Couldn\u{2019}t check microphone access.",
        ),
        (
            CaptureAccessError::PlatformRejected,
            "Microphone access request failed.",
        ),
    ] {
        access.authorization.set(Err(error));
        cx.update(|_, cx| window.update(cx, |settings, cx| settings.refresh_capture_access(cx)));
        assert_eq!(explanation(&window, cx), expected);
    }
}

#[gpui::test]
fn a_host_without_the_capability_presents_microphone_access_as_unavailable(
    cx: &mut TestAppContext,
) {
    let (window, cx) = open_settings(None, cx);
    click("settings-navigation-settings-section-privacy", cx);

    assert_eq!(status(&window, cx), CaptureAccessStatus::Unsupported);
    assert!(cx.debug_bounds(ROW_SELECTOR).is_some());
    assert_eq!(shown_state(&window, cx), Some("Unavailable"));
    assert_eq!(explanation(&window, cx), "Unavailable on this platform.");
    assert_eq!(action(&window, cx), None);
    assert_no_action_rendered(cx);

    cx.deactivate_window();
    cx.update(|window, _| window.activate_window());
    cx.run_until_parked();
    assert_eq!(status(&window, cx), CaptureAccessStatus::Unsupported);
}

#[gpui::test]
fn microphone_access_row_keeps_its_natural_height_with_wrapped_guidance(cx: &mut TestAppContext) {
    const GROUP_SELECTOR: &str = "settings-section-privacy-group-permissions-card";
    const LABEL_SELECTOR: &str = "settings-row-microphone-access-label";
    const DESCRIPTION_SELECTOR: &str = "settings-row-microphone-access-description";
    const LAST_ROW_SELECTOR: &str = "settings-row-accessibility-access";

    let access = ScriptedCaptureAccess::new(Ok(CaptureAuthorization::NotDetermined));
    let (window, cx) = open_privacy(&access, cx);
    let states = [
        Ok(CaptureAuthorization::NotDetermined),
        Ok(CaptureAuthorization::Denied),
        Ok(CaptureAuthorization::Restricted),
        Err(CaptureAccessError::PlatformRejected),
    ];
    for width in [super::super::WINDOW_WIDTH, 1100.0, 1400.0] {
        cx.simulate_resize(gpui::size(
            gpui::px(width),
            gpui::px(super::super::WINDOW_HEIGHT),
        ));
        cx.run_until_parked();
        for state in states {
            access.authorization.set(state);
            cx.update(|_, cx| {
                window.update(cx, |settings, cx| settings.refresh_capture_access(cx));
            });
            cx.run_until_parked();

            let mut bounds = |selector: &'static str| {
                cx.debug_bounds(selector)
                    .unwrap_or_else(|| panic!("{selector} should render"))
            };
            let card = bounds(GROUP_SELECTOR);
            let row = bounds(ROW_SELECTOR);
            let label = bounds(LABEL_SELECTOR);
            let description = bounds(DESCRIPTION_SELECTOR);
            let control = bounds(CONTROL_SELECTOR);
            let context = format!("{state:?} at {width}px");

            let padding_above = label.top() - row.top();
            let padding_below = row.bottom() - description.bottom();
            assert!(
                (padding_above - padding_below).abs() <= gpui::px(1.0),
                "{context}: the row should end just below its guidance, not \
                 {padding_below:?} below it"
            );
            assert!(
                (control.center().y - row.center().y).abs() <= gpui::px(1.0),
                "{context}: the control should be centered on the row"
            );
            let last_row = bounds(LAST_ROW_SELECTOR);
            let card_inset_above = row.top() - card.top();
            let card_inset_below = card.bottom() - last_row.bottom();
            assert!(
                (card_inset_above - card_inset_below).abs() <= gpui::px(1.0),
                "{context}: the card should end with its last row"
            );
        }
    }
    // The recovery guidance still wraps at the narrowest window.
    cx.simulate_resize(gpui::size(
        gpui::px(super::super::WINDOW_WIDTH),
        gpui::px(super::super::WINDOW_HEIGHT),
    ));
    access.authorization.set(Ok(CaptureAuthorization::Denied));
    access
        .open_failure
        .set(Some(CaptureAccessError::PlatformRejected));
    cx.update(|_, cx| window.update(cx, |settings, cx| settings.refresh_capture_access(cx)));
    cx.run_until_parked();
    click(OPEN_SETTINGS_SELECTOR, cx);
    assert_eq!(
        explanation(&window, cx),
        "Open System Settings and allow SpaceTerm Development in Privacy & Security > Microphone."
    );
    let label = cx.debug_bounds(LABEL_SELECTOR).expect("label bounds");
    let description = cx
        .debug_bounds(DESCRIPTION_SELECTOR)
        .expect("description bounds");
    assert!(description.size.height > label.size.height * 1.5);
}

#[gpui::test]
fn microphone_access_publishes_its_state(cx: &mut TestAppContext) {
    use spaceterm_ui::a11y_testing::A11yTree;

    let published = |cx: &mut VisualTestContext| {
        let tree = A11yTree::read(cx);
        tree.in_order()
            .into_iter()
            .filter(|node| {
                node["aria"]["role"] == "Status"
                    && node["element_id"]
                        .as_str()
                        .is_some_and(|id| id.contains(STATUS_SELECTOR))
            })
            .map(|node| {
                node["aria"]["label"]
                    .as_str()
                    .expect("the microphone state publishes its label")
                    .to_owned()
            })
            .collect::<Vec<_>>()
    };
    let access = ScriptedCaptureAccess::new(Ok(CaptureAuthorization::Authorized));
    let (window, cx) = open_privacy(&access, cx);
    for (authorization, expected) in [
        (Ok(CaptureAuthorization::Authorized), "Allowed"),
        (Ok(CaptureAuthorization::Restricted), "Restricted"),
    ] {
        access.authorization.set(authorization);
        access
            .camera_authorization
            .set(Some(Ok(CaptureAuthorization::Denied)));
        cx.update(|_, cx| window.update(cx, |settings, cx| settings.refresh_capture_access(cx)));
        cx.run_until_parked();
        assert_eq!(
            published(cx),
            [expected],
            "the microphone's own published state"
        );
    }
    access
        .authorization
        .set(Ok(CaptureAuthorization::NotDetermined));
    cx.update(|_, cx| window.update(cx, |settings, cx| settings.refresh_capture_access(cx)));
    cx.run_until_parked();
    click(ALLOW_SELECTOR, cx);
    assert_eq!(published(cx), ["Waiting"]);
}

/// One server report holds every System Permission, so the server's change signal refreshes the
/// microphone row too, without the window becoming active.
#[gpui::test]
fn a_reported_permission_change_refreshes_the_microphone_row(cx: &mut TestAppContext) {
    use crate::platform::permission_access::PermissionAuthorization::NotGranted;
    use crate::platform::permission_access::testing::ScriptedPermissionAccess;

    let microphone = ScriptedCaptureAccess::new(Ok(CaptureAuthorization::NotDetermined));
    let system = ScriptedPermissionAccess::new(Ok(NotGranted), Ok(NotGranted));
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
            super::super::PermissionCapabilities {
                capture: Some(microphone.clone()),
                system_permissions: Some(system.clone()),
                notifications: None,
                permission_setup: None,
            },
            crate::theme_registry::testing::offline(),
            None,
            window,
            cx,
        )
    });
    cx.run_until_parked();

    microphone
        .authorization
        .set(Ok(CaptureAuthorization::Authorized));
    system.report_change();
    cx.run_until_parked();

    assert_eq!(
        status(&window, cx),
        CaptureAccessStatus::Authorization(CaptureAuthorization::Authorized)
    );
}

/// The camera row asks for the camera alone and reads its own state, beside the microphone row.
#[gpui::test]
fn the_camera_row_requests_and_recovers_the_camera(cx: &mut TestAppContext) {
    let access = ScriptedCaptureAccess::new(Ok(CaptureAuthorization::NotDetermined));
    let (window, cx) = open_privacy(&access, cx);
    let camera = |window: &Entity<SettingsWindow>, cx: &mut VisualTestContext| {
        window.read_with(cx, |settings, _| settings.camera_access.presentation())
    };
    assert!(cx.debug_bounds(CAMERA.row.allow).is_some());

    click(CAMERA.row.allow, cx);
    assert_eq!(*access.devices.borrow(), [CaptureDevice::Camera]);
    assert!(cx.debug_bounds(CAMERA.row.status).is_some());
    assert_eq!(
        action(&window, cx),
        Some(AccessAction::Allow),
        "the microphone row keeps its own state"
    );

    access.authorization.set(Ok(CaptureAuthorization::Denied));
    access.take_completion()(CaptureAuthorization::Denied);
    cx.run_until_parked();
    assert_eq!(
        camera(&window, cx).control,
        AccessControl::Action(AccessAction::OpenSettings)
    );

    click(CAMERA.row.open_settings, cx);
    assert_eq!(
        *access.devices.borrow(),
        [CaptureDevice::Camera, CaptureDevice::Camera]
    );
    assert_eq!(
        camera(&window, cx).caption.as_ref(),
        "Lets terminal programs use the camera."
    );
}

/// An allowed device's reset asks first, then clears the decision so the row offers Allow and the
/// system prompts again.
#[gpui::test]
fn the_reset_clears_an_allowed_decision_so_the_system_asks_again(cx: &mut TestAppContext) {
    let access = ScriptedCaptureAccess::new(Ok(CaptureAuthorization::Authorized));
    let (window, cx) = open_privacy(&access, cx);

    click(RESET_SELECTOR, cx);
    assert!(
        access.resets.borrow().is_empty(),
        "the alert alone resets nothing"
    );
    click_modal_action(MICROPHONE.reset_confirm, cx);

    assert_eq!(*access.resets.borrow(), [CaptureDevice::Microphone]);
    assert_eq!(shown_state(&window, cx), Some("Waiting"));
    assert_eq!(explanation(&window, cx), "Resetting Microphone\u{2026}");
    assert_eq!(caption_kind(&window, cx), CaptionKind::Waiting);

    access
        .authorization
        .set(Ok(CaptureAuthorization::NotDetermined));
    access.take_reset()(Ok(()));
    cx.run_until_parked();

    assert_eq!(
        status(&window, cx),
        CaptureAccessStatus::Authorization(CaptureAuthorization::NotDetermined)
    );
    assert_eq!(action(&window, cx), Some(AccessAction::Allow));
    assert_eq!(
        explanation(&window, cx),
        "Lets terminal programs use the microphone."
    );
    click(ALLOW_SELECTOR, cx);
    assert_eq!(access.requests.get(), 1);
    // The camera keeps its decision.
    assert_eq!(
        window.read_with(cx, |settings, _| settings.camera_access.status),
        CaptureAccessStatus::Authorization(CaptureAuthorization::Authorized)
    );
}

/// macOS prompts only while undecided, so a denied device's reset is how the prompt comes back.
#[gpui::test]
fn a_denied_device_offers_the_reset_beside_system_settings(cx: &mut TestAppContext) {
    let access = ScriptedCaptureAccess::new(Ok(CaptureAuthorization::Denied));
    let (window, cx) = open_privacy(&access, cx);

    assert_eq!(action(&window, cx), Some(AccessAction::OpenSettings));
    click(RESET_SELECTOR, cx);
    click_modal_action(MICROPHONE.reset_cancel, cx);
    assert!(access.resets.borrow().is_empty());

    click(RESET_SELECTOR, cx);
    click_modal_action(MICROPHONE.reset_confirm, cx);
    access
        .authorization
        .set(Ok(CaptureAuthorization::NotDetermined));
    access.take_reset()(Ok(()));
    cx.run_until_parked();
    assert_eq!(action(&window, cx), Some(AccessAction::Allow));
}

/// A failed reset keeps the decision and says so in the row, which still offers the reset.
#[gpui::test]
fn a_failed_reset_keeps_the_decision_and_offers_the_reset_again(cx: &mut TestAppContext) {
    let access = ScriptedCaptureAccess::new(Ok(CaptureAuthorization::Authorized));
    let (window, cx) = open_privacy(&access, cx);

    click(RESET_SELECTOR, cx);
    click_modal_action(MICROPHONE.reset_confirm, cx);
    access.take_reset()(Err(CaptureAccessError::PlatformRejected));
    cx.run_until_parked();

    assert_eq!(
        window.read_with(cx, |settings, _| settings.microphone_access.reset),
        Some(CaptureReset::Failed)
    );
    assert_eq!(shown_state(&window, cx), Some("Allowed"));
    assert_eq!(
        explanation(&window, cx),
        "Couldn\u{2019}t reset Microphone. Try again."
    );
    assert_eq!(caption_kind(&window, cx), CaptionKind::Error);

    // A reset that cannot start, or ends without a result, reports the same failure.
    access
        .reset_failure
        .set(Some(CaptureAccessError::PlatformUnavailable));
    click(RESET_SELECTOR, cx);
    click_modal_action(MICROPHONE.reset_confirm, cx);
    assert_eq!(caption_kind(&window, cx), CaptionKind::Error);
    access.reset_failure.set(None);
    click(RESET_SELECTOR, cx);
    click_modal_action(MICROPHONE.reset_confirm, cx);
    drop(access.take_reset());
    cx.run_until_parked();
    assert_eq!(
        window.read_with(cx, |settings, _| settings.microphone_access.reset),
        Some(CaptureReset::Failed)
    );
    assert_eq!(access.resets.borrow().len(), 3);
}
