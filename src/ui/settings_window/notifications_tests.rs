//! The Notifications rows in the Privacy section, driven through a scripted capability.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gpui::{Entity, Modifiers, TestAppContext, VisualTestContext};

use crate::appearance::Appearance;
use crate::platform::appearance::testing::RecordingAppearancePlatform;
use crate::platform::notification_access::{
    NotificationAccess, NotificationAccessError, NotificationAccessState,
    NotificationRequestCompletion, NotificationStateCompletion,
};
use crate::platform::window_movement::RecordingOperatingSystemWindowDragPlatform;
use crate::settings::SettingsDocument;
use crate::settings::storage::testing::MemoryStorage;
use crate::ui::appearance_runtime;

use super::super::{SettingsSectionId, SettingsWindow};
use super::{
    CHECK_AGAIN, CONTROL, NotificationAccessAction, NotificationAccessStatus, OPEN_SETTINGS,
    REQUEST,
};

type ReadResult = Result<NotificationAccessState, NotificationAccessError>;

/// A capability whose answers the test controls. Reads answer at once; a request waits for the
/// test to answer it.
struct ScriptedNotificationAccess {
    state: Cell<ReadResult>,
    open_failure: Cell<Option<NotificationAccessError>>,
    pending: RefCell<Vec<NotificationRequestCompletion>>,
    reads: Cell<usize>,
    requests: Cell<usize>,
    opened: Cell<usize>,
}

impl ScriptedNotificationAccess {
    fn new(state: ReadResult) -> Rc<Self> {
        Rc::new(Self {
            state: Cell::new(state),
            open_failure: Cell::new(None),
            pending: RefCell::default(),
            reads: Cell::new(0),
            requests: Cell::new(0),
            opened: Cell::new(0),
        })
    }

    fn take_request(&self) -> NotificationRequestCompletion {
        self.pending
            .borrow_mut()
            .pop()
            .expect("a request should be awaiting its answer")
    }
}

impl NotificationAccess for ScriptedNotificationAccess {
    fn read(&self, completion: NotificationStateCompletion) {
        self.reads.set(self.reads.get() + 1);
        completion(self.state.get());
    }

    fn request(&self, completion: NotificationRequestCompletion) {
        self.requests.set(self.requests.get() + 1);
        self.pending.borrow_mut().push(completion);
    }

    fn open_settings(&self) -> Result<(), NotificationAccessError> {
        self.opened.set(self.opened.get() + 1);
        self.open_failure.get().map_or(Ok(()), Err)
    }
}

fn open_privacy(
    access: Option<Rc<ScriptedNotificationAccess>>,
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
                notifications: access.map(|access| access as Rc<dyn NotificationAccess>),
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

fn status(window: &Entity<SettingsWindow>, cx: &mut VisualTestContext) -> NotificationAccessStatus {
    window.read_with(cx, |window, _| window.notification_access.status)
}

fn action(
    window: &Entity<SettingsWindow>,
    cx: &mut VisualTestContext,
) -> Option<NotificationAccessAction> {
    window.read_with(cx, |window, _| {
        window.notification_access.presentation().action
    })
}

fn explanation(window: &Entity<SettingsWindow>, cx: &mut VisualTestContext) -> &'static str {
    window.read_with(cx, |window, _| {
        window.notification_access.presentation().explanation
    })
}

#[gpui::test]
fn privacy_section_presents_notifications_as_their_own_group(cx: &mut TestAppContext) {
    let access = ScriptedNotificationAccess::new(Ok(NotificationAccessState::NotDetermined));
    let (window, cx) = open_privacy(Some(access), cx);

    assert_eq!(
        window.read_with(cx, |window, _| window.active_section),
        SettingsSectionId::Privacy
    );
    for selector in [
        "settings-section-privacy-group-notifications",
        "settings-row-notifications-access",
        CONTROL,
        "settings-notifications-access-state-not-requested",
        REQUEST,
        "settings-row-terminal-attention-notifications",
        super::TERMINAL_ATTENTION,
    ] {
        assert!(cx.debug_bounds(selector).is_some(), "{selector} missing");
    }
}

#[gpui::test]
fn request_access_asks_once_and_presents_the_decision_read_afterwards(cx: &mut TestAppContext) {
    let access = ScriptedNotificationAccess::new(Ok(NotificationAccessState::NotDetermined));
    let (window, cx) = open_privacy(Some(access.clone()), cx);

    click(REQUEST, cx);
    assert_eq!(status(&window, cx), NotificationAccessStatus::Requesting);
    assert_eq!(
        action(&window, cx),
        Some(NotificationAccessAction::Requesting)
    );
    // The prompt activates and deactivates the window; that read cannot replace the request.
    let reads = access.reads.get();
    cx.deactivate_window();
    cx.update(|window, _| window.activate_window());
    cx.run_until_parked();
    assert_eq!(access.reads.get(), reads);
    click(REQUEST, cx);
    assert_eq!(access.requests.get(), 1);

    access.state.set(Ok(NotificationAccessState::Allowed));
    access.take_request()(Ok(()));
    cx.run_until_parked();

    assert_eq!(
        status(&window, cx),
        NotificationAccessStatus::State(NotificationAccessState::Allowed)
    );
    assert_eq!(action(&window, cx), None);
}

#[gpui::test]
fn a_failed_request_offers_check_again(cx: &mut TestAppContext) {
    let access = ScriptedNotificationAccess::new(Ok(NotificationAccessState::NotDetermined));
    let (window, cx) = open_privacy(Some(access.clone()), cx);

    click(REQUEST, cx);
    access.take_request()(Err(NotificationAccessError::Rejected));
    cx.run_until_parked();

    assert_eq!(
        status(&window, cx),
        NotificationAccessStatus::Failed(NotificationAccessError::Rejected)
    );
    assert_eq!(explanation(&window, cx), "Notification request failed.");
    assert_eq!(
        action(&window, cx),
        Some(NotificationAccessAction::CheckAgain)
    );
}

#[gpui::test]
fn each_state_that_hides_notifications_opens_system_settings(cx: &mut TestAppContext) {
    let access = ScriptedNotificationAccess::new(Ok(NotificationAccessState::Denied));
    let (window, cx) = open_privacy(Some(access.clone()), cx);

    for state in [
        NotificationAccessState::Denied,
        NotificationAccessState::Quiet,
        NotificationAccessState::Hidden,
    ] {
        access.state.set(Ok(state));
        window.update(cx, |settings, cx| settings.refresh_notification_access(cx));
        cx.run_until_parked();
        assert_eq!(
            action(&window, cx),
            Some(NotificationAccessAction::OpenSettings),
            "{state:?}"
        );
    }
    click(OPEN_SETTINGS, cx);
    assert_eq!(access.opened.get(), 1);
    assert_eq!(access.requests.get(), 0);
}

#[gpui::test]
fn a_system_settings_failure_names_where_to_allow_notifications(cx: &mut TestAppContext) {
    let access = ScriptedNotificationAccess::new(Ok(NotificationAccessState::Denied));
    access
        .open_failure
        .set(Some(NotificationAccessError::Rejected));
    let (window, cx) = open_privacy(Some(access.clone()), cx);

    click(OPEN_SETTINGS, cx);
    let open_failed = format!(
        "Open System Settings and allow {} in Notifications.",
        crate::application_identity::ApplicationIdentity::current().display_name()
    );
    assert_eq!(explanation(&window, cx), open_failed);

    // A state the failure did not try to fix retires it.
    for state in [
        NotificationAccessState::Allowed,
        NotificationAccessState::Denied,
    ] {
        access.state.set(Ok(state));
        window.update(cx, |settings, cx| settings.refresh_notification_access(cx));
        cx.run_until_parked();
    }
    assert_ne!(explanation(&window, cx), open_failed);
}

#[gpui::test]
fn returning_to_settings_reads_authorization_changed_in_system_settings(cx: &mut TestAppContext) {
    let access = ScriptedNotificationAccess::new(Ok(NotificationAccessState::Denied));
    let (window, cx) = open_privacy(Some(access.clone()), cx);

    access.state.set(Ok(NotificationAccessState::Allowed));
    cx.deactivate_window();
    cx.update(|window, _| window.activate_window());
    cx.run_until_parked();

    assert_eq!(
        status(&window, cx),
        NotificationAccessStatus::State(NotificationAccessState::Allowed)
    );
}

#[gpui::test]
fn a_failed_read_offers_check_again_which_reads_again(cx: &mut TestAppContext) {
    let access = ScriptedNotificationAccess::new(Err(NotificationAccessError::Unavailable));
    let (window, cx) = open_privacy(Some(access.clone()), cx);

    assert_eq!(
        explanation(&window, cx),
        "Couldn\u{2019}t check notifications."
    );
    assert_eq!(
        action(&window, cx),
        Some(NotificationAccessAction::CheckAgain)
    );
    access.state.set(Ok(NotificationAccessState::Allowed));
    click(CHECK_AGAIN, cx);

    assert_eq!(
        status(&window, cx),
        NotificationAccessStatus::State(NotificationAccessState::Allowed)
    );
}

#[gpui::test]
fn a_desktop_without_per_application_grants_reports_its_notification_service(
    cx: &mut TestAppContext,
) {
    let access = ScriptedNotificationAccess::new(Ok(NotificationAccessState::ServiceUnavailable));
    let (window, cx) = open_privacy(Some(access.clone()), cx);

    assert_eq!(
        explanation(&window, cx),
        "No notification service is running on this desktop."
    );
    assert_eq!(
        action(&window, cx),
        Some(NotificationAccessAction::CheckAgain)
    );
    access
        .state
        .set(Ok(NotificationAccessState::ServiceAvailable));
    click(CHECK_AGAIN, cx);

    assert_eq!(action(&window, cx), None);
    assert!(
        cx.debug_bounds("settings-notifications-access-state-available")
            .is_some()
    );
    assert!(cx.debug_bounds(REQUEST).is_none());
    assert!(cx.debug_bounds(OPEN_SETTINGS).is_none());
}

#[gpui::test]
fn without_a_capability_the_row_says_notifications_are_unavailable(cx: &mut TestAppContext) {
    let (window, cx) = open_privacy(None, cx);

    assert_eq!(status(&window, cx), NotificationAccessStatus::Unsupported);
    assert_eq!(action(&window, cx), None);
    assert!(
        cx.debug_bounds("settings-notifications-access-state-unavailable")
            .is_some()
    );
}
