//! Microphone access in the Privacy section, driven through a scripted capability.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gpui::{Entity, Modifiers, TestAppContext, VisualTestContext};

use crate::appearance::{Appearance, SettingsDocument};
use crate::platform::appearance::testing::RecordingAppearancePlatform;
use crate::platform::microphone_access::{
    MicrophoneAccess, MicrophoneAccessError, MicrophoneAuthorization,
    MicrophoneAuthorizationCompletion,
};
use crate::platform::window_movement::RecordingOperatingSystemWindowDragPlatform;
use crate::ui::appearance_runtime;

use super::microphone::{
    CHECK_AGAIN_SELECTOR, CONTROL_SELECTOR, MicrophoneAccessAction, MicrophoneAccessStatus,
    OPEN_SETTINGS_SELECTOR, REQUEST_SELECTOR,
};
use super::test_support::MemoryStorage;
use super::{SettingsRowId, SettingsSectionId, SettingsWindow};

const ROW_SELECTOR: &str = "settings-row-microphone-access";

/// A capability whose authorization, failures, and pending decisions the test controls.
struct ScriptedMicrophoneAccess {
    authorization: Cell<Result<MicrophoneAuthorization, MicrophoneAccessError>>,
    request_failure: Cell<Option<MicrophoneAccessError>>,
    open_failure: Cell<Option<MicrophoneAccessError>>,
    pending: RefCell<Vec<MicrophoneAuthorizationCompletion>>,
    requests: Cell<usize>,
    opened: Cell<usize>,
}

impl ScriptedMicrophoneAccess {
    fn new(authorization: Result<MicrophoneAuthorization, MicrophoneAccessError>) -> Rc<Self> {
        Rc::new(Self {
            authorization: Cell::new(authorization),
            request_failure: Cell::new(None),
            open_failure: Cell::new(None),
            pending: RefCell::default(),
            requests: Cell::new(0),
            opened: Cell::new(0),
        })
    }

    fn take_completion(&self) -> MicrophoneAuthorizationCompletion {
        self.pending
            .borrow_mut()
            .pop()
            .expect("a request should be awaiting its decision")
    }
}

impl MicrophoneAccess for ScriptedMicrophoneAccess {
    fn authorization(&self) -> Result<MicrophoneAuthorization, MicrophoneAccessError> {
        self.authorization.get()
    }

    fn request_authorization(
        &self,
        completion: MicrophoneAuthorizationCompletion,
    ) -> Result<(), MicrophoneAccessError> {
        self.requests.set(self.requests.get() + 1);
        if let Some(error) = self.request_failure.get() {
            return Err(error);
        }
        self.pending.borrow_mut().push(completion);
        Ok(())
    }

    fn open_settings(&self) -> Result<(), MicrophoneAccessError> {
        self.opened.set(self.opened.get() + 1);
        self.open_failure.get().map_or(Ok(()), Err)
    }
}

fn open_settings(
    access: Option<Rc<dyn MicrophoneAccess>>,
    cx: &mut TestAppContext,
) -> (Entity<SettingsWindow>, &mut VisualTestContext) {
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
            access,
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
    access: &Rc<ScriptedMicrophoneAccess>,
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

fn status(window: &Entity<SettingsWindow>, cx: &mut VisualTestContext) -> MicrophoneAccessStatus {
    window.read_with(cx, |window, _| window.microphone_access.status())
}

fn action(
    window: &Entity<SettingsWindow>,
    cx: &mut VisualTestContext,
) -> Option<MicrophoneAccessAction> {
    window.read_with(cx, |window, _| {
        window.microphone_access.presentation().action
    })
}

fn explanation(window: &Entity<SettingsWindow>, cx: &mut VisualTestContext) -> &'static str {
    window.read_with(cx, |window, _| {
        window.microphone_access.presentation().explanation
    })
}

/// GPUI retains a selector once drawn, so absence is only provable for one never rendered.
fn assert_no_action_rendered(cx: &mut VisualTestContext) {
    for selector in [
        REQUEST_SELECTOR,
        OPEN_SETTINGS_SELECTOR,
        CHECK_AGAIN_SELECTOR,
    ] {
        assert!(
            cx.debug_bounds(selector).is_none(),
            "{selector} should not be offered"
        );
    }
}

#[gpui::test]
fn privacy_section_presents_microphone_access_as_a_row_in_its_group(cx: &mut TestAppContext) {
    let access = ScriptedMicrophoneAccess::new(Ok(MicrophoneAuthorization::NotDetermined));
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
        "settings-microphone-access-state-not-requested",
        REQUEST_SELECTOR,
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
fn settings_search_reveals_microphone_access_from_another_section(cx: &mut TestAppContext) {
    let access = ScriptedMicrophoneAccess::new(Ok(MicrophoneAuthorization::Denied));
    let (window, cx) = open_settings(Some(access), cx);
    assert_eq!(
        window.read_with(cx, |window, _| window.active_section),
        SettingsSectionId::Appearance
    );

    cx.update(|_, cx| {
        let search = window.read(cx).search.clone();
        search.update(cx, |search, cx| search.set_value("voice".to_owned(), cx));
    });
    cx.run_until_parked();

    window.read_with(cx, |settings, _| {
        assert_eq!(settings.active_section, SettingsSectionId::Privacy);
        assert_eq!(settings.revealed, Some(SettingsRowId::MicrophoneAccess));
    });
    assert!(cx.debug_bounds(ROW_SELECTOR).is_some());
    assert!(cx.debug_bounds(OPEN_SETTINGS_SELECTOR).is_some());
}

#[gpui::test]
fn requesting_access_waits_for_the_native_decision_on_the_foreground(cx: &mut TestAppContext) {
    let access = ScriptedMicrophoneAccess::new(Ok(MicrophoneAuthorization::NotDetermined));
    let (window, cx) = open_privacy(&access, cx);

    click(REQUEST_SELECTOR, cx);

    assert_eq!(access.requests.get(), 1);
    assert_eq!(status(&window, cx), MicrophoneAccessStatus::Requesting);
    assert_eq!(
        action(&window, cx),
        Some(MicrophoneAccessAction::Requesting)
    );
    assert!(
        cx.debug_bounds("settings-microphone-access-state-requesting")
            .is_some()
    );

    // The withheld button cannot stack a second prompt.
    click(REQUEST_SELECTOR, cx);
    assert_eq!(access.requests.get(), 1);

    // The system prompt activating and returning the window does not overwrite the pending request.
    access
        .authorization
        .set(Ok(MicrophoneAuthorization::Authorized));
    cx.deactivate_window();
    cx.update(|window, _| window.activate_window());
    cx.run_until_parked();
    assert_eq!(status(&window, cx), MicrophoneAccessStatus::Requesting);

    // AVFoundation answers on an arbitrary queue, never on the GPUI foreground.
    let completion = access.take_completion();
    std::thread::spawn(move || completion(MicrophoneAuthorization::Authorized))
        .join()
        .expect("the native completion should run");
    assert_eq!(status(&window, cx), MicrophoneAccessStatus::Requesting);

    cx.run_until_parked();

    assert_eq!(
        status(&window, cx),
        MicrophoneAccessStatus::Authorization(MicrophoneAuthorization::Authorized)
    );
    assert_eq!(action(&window, cx), None);
    assert!(
        cx.debug_bounds("settings-microphone-access-state-allowed")
            .is_some()
    );
}

#[gpui::test]
fn a_denial_from_the_prompt_offers_system_settings_rather_than_prompting_again(
    cx: &mut TestAppContext,
) {
    let access = ScriptedMicrophoneAccess::new(Ok(MicrophoneAuthorization::NotDetermined));
    let (window, cx) = open_privacy(&access, cx);

    click(REQUEST_SELECTOR, cx);
    let completion = access.take_completion();
    std::thread::spawn(move || completion(MicrophoneAuthorization::Denied))
        .join()
        .expect("the native completion should run");
    cx.run_until_parked();

    assert_eq!(
        status(&window, cx),
        MicrophoneAccessStatus::Authorization(MicrophoneAuthorization::Denied)
    );
    assert_eq!(
        action(&window, cx),
        Some(MicrophoneAccessAction::OpenSettings)
    );
    click(OPEN_SETTINGS_SELECTOR, cx);
    assert_eq!(access.opened.get(), 1);
    assert_eq!(access.requests.get(), 1);
}

#[gpui::test]
fn a_request_dropped_without_a_decision_reads_authorization_again(cx: &mut TestAppContext) {
    let access = ScriptedMicrophoneAccess::new(Ok(MicrophoneAuthorization::NotDetermined));
    let (window, cx) = open_privacy(&access, cx);

    click(REQUEST_SELECTOR, cx);
    access
        .authorization
        .set(Ok(MicrophoneAuthorization::Restricted));
    drop(access.take_completion());
    cx.run_until_parked();

    assert_eq!(
        status(&window, cx),
        MicrophoneAccessStatus::Authorization(MicrophoneAuthorization::Restricted)
    );
}

#[gpui::test]
fn denied_access_recovers_through_system_settings(cx: &mut TestAppContext) {
    let access = ScriptedMicrophoneAccess::new(Ok(MicrophoneAuthorization::Denied));
    let (window, cx) = open_privacy(&access, cx);

    assert!(
        cx.debug_bounds("settings-microphone-access-state-denied")
            .is_some()
    );
    assert!(cx.debug_bounds(REQUEST_SELECTOR).is_none());
    let guidance = explanation(&window, cx);

    click(OPEN_SETTINGS_SELECTOR, cx);

    assert_eq!(access.opened.get(), 1);
    assert_eq!(access.requests.get(), 0);
    assert_eq!(explanation(&window, cx), guidance);

    // Returning from System Settings reads the decision the person made there.
    access
        .authorization
        .set(Ok(MicrophoneAuthorization::Authorized));
    cx.deactivate_window();
    cx.update(|window, _| window.activate_window());
    cx.run_until_parked();

    assert_eq!(
        status(&window, cx),
        MicrophoneAccessStatus::Authorization(MicrophoneAuthorization::Authorized)
    );
    assert_eq!(action(&window, cx), None);
}

#[gpui::test]
fn a_failed_recovery_keeps_the_denial_and_explains_where_to_go(cx: &mut TestAppContext) {
    let access = ScriptedMicrophoneAccess::new(Ok(MicrophoneAuthorization::Denied));
    access
        .open_failure
        .set(Some(MicrophoneAccessError::PlatformRejected));
    let (window, cx) = open_privacy(&access, cx);
    let guidance = explanation(&window, cx);

    click(OPEN_SETTINGS_SELECTOR, cx);

    assert_eq!(
        status(&window, cx),
        MicrophoneAccessStatus::Authorization(MicrophoneAuthorization::Denied)
    );
    let failure = explanation(&window, cx);
    assert_ne!(failure, guidance);
    assert!(failure.contains("Privacy & Security > Microphone"));
    assert_eq!(
        action(&window, cx),
        Some(MicrophoneAccessAction::OpenSettings)
    );

    access.open_failure.set(None);
    click(OPEN_SETTINGS_SELECTOR, cx);
    assert_eq!(explanation(&window, cx), guidance);
}

#[gpui::test]
fn authorized_access_is_confirmed_without_a_fix(cx: &mut TestAppContext) {
    let access = ScriptedMicrophoneAccess::new(Ok(MicrophoneAuthorization::Authorized));
    let (window, cx) = open_privacy(&access, cx);

    assert!(
        cx.debug_bounds("settings-microphone-access-state-allowed")
            .is_some()
    );
    assert_eq!(action(&window, cx), None);
    assert_no_action_rendered(cx);
    assert!(
        cx.debug_bounds("settings-row-microphone-access-reset")
            .is_none()
    );
}

#[gpui::test]
fn restricted_access_explains_the_policy_without_prompting(cx: &mut TestAppContext) {
    let access = ScriptedMicrophoneAccess::new(Ok(MicrophoneAuthorization::Restricted));
    let (window, cx) = open_privacy(&access, cx);

    assert!(
        cx.debug_bounds("settings-microphone-access-state-restricted")
            .is_some()
    );
    assert_eq!(action(&window, cx), None);
    assert_no_action_rendered(cx);
    assert!(explanation(&window, cx).contains("cannot change"));

    // Nothing outside the row can prompt from a restricted state either.
    cx.update(|_, cx| {
        window.update(cx, |settings, cx| {
            settings.request_microphone_access(cx);
            settings.open_microphone_settings(cx);
        });
    });
    cx.run_until_parked();
    assert_eq!((access.requests.get(), access.opened.get()), (0, 0));
}

#[gpui::test]
fn an_unreadable_authorization_reports_a_content_free_failure_and_checks_again(
    cx: &mut TestAppContext,
) {
    let access = ScriptedMicrophoneAccess::new(Err(MicrophoneAccessError::PlatformUnavailable));
    let (window, cx) = open_privacy(&access, cx);

    assert_eq!(
        status(&window, cx),
        MicrophoneAccessStatus::Failed(MicrophoneAccessError::PlatformUnavailable)
    );
    assert!(
        cx.debug_bounds("settings-microphone-access-state-unavailable")
            .is_some()
    );
    assert_eq!(
        action(&window, cx),
        Some(MicrophoneAccessAction::CheckAgain)
    );
    assert!(cx.debug_bounds(REQUEST_SELECTOR).is_none());

    access
        .authorization
        .set(Ok(MicrophoneAuthorization::NotDetermined));
    click(CHECK_AGAIN_SELECTOR, cx);

    assert_eq!(
        status(&window, cx),
        MicrophoneAccessStatus::Authorization(MicrophoneAuthorization::NotDetermined)
    );
    assert!(cx.debug_bounds(REQUEST_SELECTOR).is_some());
}

#[gpui::test]
fn a_rejected_request_reports_failure_instead_of_waiting(cx: &mut TestAppContext) {
    let access = ScriptedMicrophoneAccess::new(Ok(MicrophoneAuthorization::NotDetermined));
    access
        .request_failure
        .set(Some(MicrophoneAccessError::OffMainThread));
    let (window, cx) = open_privacy(&access, cx);

    click(REQUEST_SELECTOR, cx);

    assert_eq!(
        status(&window, cx),
        MicrophoneAccessStatus::Failed(MicrophoneAccessError::OffMainThread)
    );
    assert_eq!(
        action(&window, cx),
        Some(MicrophoneAccessAction::CheckAgain)
    );
    assert!(cx.debug_bounds(CHECK_AGAIN_SELECTOR).is_some());
}

/// Every failure explanation is fixed product copy, never a native error's text.
#[gpui::test]
fn failure_explanations_are_distinct_fixed_copy(cx: &mut TestAppContext) {
    let access = ScriptedMicrophoneAccess::new(Ok(MicrophoneAuthorization::Authorized));
    let (window, cx) = open_settings(Some(access.clone()), cx);
    let mut explanations = Vec::new();
    for error in [
        MicrophoneAccessError::OffMainThread,
        MicrophoneAccessError::PlatformUnavailable,
        MicrophoneAccessError::PlatformRejected,
    ] {
        access.authorization.set(Err(error));
        cx.update(|_, cx| window.update(cx, |settings, cx| settings.refresh_microphone_access(cx)));
        let presented = explanation(&window, cx);
        assert_ne!(presented, error.to_string());
        explanations.push(presented);
    }
    explanations.dedup();
    assert_eq!(explanations.len(), 3);
}

#[gpui::test]
fn a_host_without_the_capability_presents_microphone_access_as_unavailable(
    cx: &mut TestAppContext,
) {
    let (window, cx) = open_settings(None, cx);
    click("settings-navigation-settings-section-privacy", cx);

    assert_eq!(status(&window, cx), MicrophoneAccessStatus::Unsupported);
    assert!(cx.debug_bounds(ROW_SELECTOR).is_some());
    assert!(
        cx.debug_bounds("settings-microphone-access-state-unavailable")
            .is_some()
    );
    assert_eq!(action(&window, cx), None);
    assert_no_action_rendered(cx);

    cx.deactivate_window();
    cx.update(|window, _| window.activate_window());
    cx.run_until_parked();
    assert_eq!(status(&window, cx), MicrophoneAccessStatus::Unsupported);
}

/// Wrapped guidance beside the badge and action once stretched the row, and its card, hundreds of
/// pixels below the content. Every state keeps the row at its natural height: equal padding above
/// the label and below the guidance, with the control centered on the same row.
#[gpui::test]
fn microphone_access_row_keeps_its_natural_height_with_wrapped_guidance(cx: &mut TestAppContext) {
    const GROUP_SELECTOR: &str = "settings-section-privacy-group-permissions-card";
    const LABEL_SELECTOR: &str = "settings-row-microphone-access-label";
    const DESCRIPTION_SELECTOR: &str = "settings-row-microphone-access-description";

    let access = ScriptedMicrophoneAccess::new(Ok(MicrophoneAuthorization::NotDetermined));
    let (window, cx) = open_privacy(&access, cx);
    let states = [
        Ok(MicrophoneAuthorization::NotDetermined),
        Ok(MicrophoneAuthorization::Denied),
        Ok(MicrophoneAuthorization::Restricted),
        Err(MicrophoneAccessError::PlatformRejected),
    ];
    for width in [super::WINDOW_WIDTH, 1100.0, 1400.0] {
        cx.simulate_resize(gpui::size(gpui::px(width), gpui::px(super::WINDOW_HEIGHT)));
        cx.run_until_parked();
        for state in states {
            access.authorization.set(state);
            cx.update(|_, cx| {
                window.update(cx, |settings, cx| settings.refresh_microphone_access(cx));
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
            let card_inset_above = row.top() - card.top();
            let card_inset_below = card.bottom() - row.bottom();
            assert!(
                (card_inset_above - card_inset_below).abs() <= gpui::px(1.0),
                "{context}: the card should end with its only row"
            );
        }
    }
    // The narrowest window wraps the guidance, which is the layout that once stretched.
    cx.simulate_resize(gpui::size(
        gpui::px(super::WINDOW_WIDTH),
        gpui::px(super::WINDOW_HEIGHT),
    ));
    access
        .authorization
        .set(Ok(MicrophoneAuthorization::NotDetermined));
    cx.update(|_, cx| window.update(cx, |settings, cx| settings.refresh_microphone_access(cx)));
    cx.run_until_parked();
    let label = cx.debug_bounds(LABEL_SELECTOR).expect("label bounds");
    let description = cx
        .debug_bounds(DESCRIPTION_SELECTOR)
        .expect("description bounds");
    assert!(description.size.height > label.size.height * 1.5);
}
