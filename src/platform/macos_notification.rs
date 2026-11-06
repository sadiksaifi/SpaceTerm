use std::sync::Mutex;

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::{Bool, NSObject, NSObjectProtocol, ProtocolObject};
use objc2::{AllocAnyThread as _, DefinedClass as _, define_class, msg_send};
use objc2_foundation::{NSArray, NSBundle, NSInteger, NSString};
use objc2_user_notifications::{
    UNAuthorizationOptions, UNMutableNotificationContent, UNNotification,
    UNNotificationPresentationOptions, UNNotificationRequest, UNNotificationResponse,
    UNNotificationSetting, UNUserNotificationCenter, UNUserNotificationCenterDelegate,
};

use crate::application_identity::ApplicationIdentity;
use crate::platform::notification_access::{
    NotificationAccess, NotificationAccessError, NotificationRequestCompletion,
    NotificationStateCompletion,
};
use crate::terminal::attention_notification::{
    AuthorizationCompletion, NotificationAdapter, NotificationAuthorization, NotificationSettings,
    SettingsCompletion, notification_body,
};
use crate::terminal::attention_runtime::AttentionFailure;

const NOTIFICATION_IDENTIFIER: &str = "com.peblon.spaceterm.terminal-attention";
const REMINDER_IDENTIFIER: &str = "com.peblon.spaceterm.update-reminder";
fn process_has_application_bundle_identity() -> bool {
    NSBundle::mainBundle().bundleIdentifier().is_some()
}

fn notification_center() -> Option<Retained<UNUserNotificationCenter>> {
    if !process_has_application_bundle_identity() {
        return None;
    }
    Some(UNUserNotificationCenter::currentNotificationCenter())
}

fn authorization_from_raw(raw: NSInteger) -> NotificationAuthorization {
    match raw {
        0 => NotificationAuthorization::NotDetermined,
        1 => NotificationAuthorization::Denied,
        2 => NotificationAuthorization::Authorized,
        3 => NotificationAuthorization::Provisional,
        _ => NotificationAuthorization::Unknown,
    }
}

pub(crate) struct UserNotificationAdapter {
    identity: ApplicationIdentity,
}

impl UserNotificationAdapter {
    pub(crate) const fn new(identity: ApplicationIdentity) -> Self {
        Self { identity }
    }
}

impl NotificationAdapter for UserNotificationAdapter {
    fn settings(&self, completion: SettingsCompletion) {
        let Some(center) = notification_center() else {
            completion(Err(AttentionFailure::Unavailable));
            return;
        };
        let completion = Mutex::new(Some(completion));
        let settings = RcBlock::new(
            move |settings: std::ptr::NonNull<objc2_user_notifications::UNNotificationSettings>| {
                let Some(completion) = completion
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .take()
                else {
                    return;
                };
                // SAFETY: UserNotifications provides a live settings object for this callback.
                let settings = unsafe { settings.as_ref() };
                completion(Ok(NotificationSettings {
                    authorization: authorization_from_raw(settings.authorizationStatus().0),
                    alert_enabled: settings.alertSetting() == UNNotificationSetting::Enabled,
                    center_enabled: settings.notificationCenterSetting()
                        == UNNotificationSetting::Enabled,
                }));
            },
        );
        center.getNotificationSettingsWithCompletionHandler(&settings);
    }

    fn authorize_provisionally(&self, completion: AuthorizationCompletion) {
        let Some(center) = notification_center() else {
            completion(Err(AttentionFailure::Unavailable));
            return;
        };
        let completion = Mutex::new(Some(completion));
        let completion = RcBlock::new(
            move |granted: Bool, error: *mut objc2_foundation::NSError| {
                let Some(completion) = completion
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .take()
                else {
                    return;
                };
                completion(if !error.is_null() {
                    Err(AttentionFailure::DeliveryFailed)
                } else if !granted.as_bool() {
                    Err(AttentionFailure::Denied)
                } else {
                    Ok(())
                });
            },
        );
        center.requestAuthorizationWithOptions_completionHandler(
            UNAuthorizationOptions::Alert | UNAuthorizationOptions::Provisional,
            &completion,
        );
    }

    fn submit(&self, aggregate_count: u32) -> Result<(), AttentionFailure> {
        let center = notification_center().ok_or(AttentionFailure::Unavailable)?;
        let content = UNMutableNotificationContent::new();
        content.setTitle(&NSString::from_str(self.identity.display_name()));
        content.setBody(&NSString::from_str(&notification_body(aggregate_count)));
        let identifier = NSString::from_str(NOTIFICATION_IDENTIFIER);
        content.setThreadIdentifier(&identifier);
        let request = UNNotificationRequest::requestWithIdentifier_content_trigger(
            &identifier,
            &content,
            None,
        );
        center.addNotificationRequest_withCompletionHandler(&request, None);
        Ok(())
    }

    fn clear(&self) -> Result<(), AttentionFailure> {
        let center = notification_center().ok_or(AttentionFailure::Unavailable)?;
        let identifiers =
            NSArray::from_retained_slice(&[NSString::from_str(NOTIFICATION_IDENTIFIER)]);
        center.removePendingNotificationRequestsWithIdentifiers(&identifiers);
        center.removeDeliveredNotificationsWithIdentifiers(&identifiers);
        Ok(())
    }
}

/// The Privacy section's view of the desktop's own notification authorization.
pub(crate) struct MacosNotificationAccess {
    identity: ApplicationIdentity,
}

impl MacosNotificationAccess {
    pub(crate) const fn new(identity: ApplicationIdentity) -> Self {
        Self { identity }
    }
}

impl NotificationAccess for MacosNotificationAccess {
    fn read(&self, completion: NotificationStateCompletion) {
        UserNotificationAdapter::new(self.identity).settings(Box::new(move |settings| {
            completion(
                settings
                    .map_err(|_| NotificationAccessError::Unavailable)
                    .and_then(NotificationSettings::access_state),
            );
        }));
    }

    /// Asks for alerts without the provisional option, so the system shows its prompt.
    fn request(&self, completion: NotificationRequestCompletion) {
        let Some(center) = notification_center() else {
            completion(Err(NotificationAccessError::Unavailable));
            return;
        };
        let completion = Mutex::new(Some(completion));
        let completion = RcBlock::new(
            move |_granted: Bool, error: *mut objc2_foundation::NSError| {
                let Some(completion) = completion
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .take()
                else {
                    return;
                };
                // A refusal is a decision, which the next read reports.
                completion(if error.is_null() {
                    Ok(())
                } else {
                    Err(NotificationAccessError::Rejected)
                });
            },
        );
        center.requestAuthorizationWithOptions_completionHandler(
            UNAuthorizationOptions::Alert,
            &completion,
        );
    }

    fn open_settings(&self) -> Result<(), NotificationAccessError> {
        super::macos_system_settings::open(&notification_settings_uri(self.identity))
            .then_some(())
            .ok_or(NotificationAccessError::Rejected)
    }
}

/// The desktop's own entry in System Settings > Notifications.
fn notification_settings_uri(identity: ApplicationIdentity) -> String {
    format!(
        "x-apple.systempreferences:com.apple.Notifications-Settings.extension?id={}",
        identity.application_id()
    )
}

/// What the reminder delegate reports clicks to.
struct ReminderDelegateIvars {
    activated: async_channel::Sender<String>,
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements, and define_class! drops the ivars.
    #[unsafe(super(NSObject))]
    #[name = "SpaceTermUpdateReminderDelegate"]
    #[ivars = ReminderDelegateIvars]
    struct ReminderDelegate;

    unsafe impl NSObjectProtocol for ReminderDelegate {}

    unsafe impl UNUserNotificationCenterDelegate for ReminderDelegate {
        /// An overdue reminder shows even while SpaceTerm is in front, because it is due only
        /// while a window is active. Other notifications keep the default of showing nothing.
        #[unsafe(method(userNotificationCenter:willPresentNotification:withCompletionHandler:))]
        fn will_present(
            &self,
            _center: &UNUserNotificationCenter,
            notification: &UNNotification,
            completion: &block2::DynBlock<dyn Fn(UNNotificationPresentationOptions)>,
        ) {
            let identifier = notification.request().identifier().to_string();
            completion.call((if reminder_key(&identifier).is_some() {
                UNNotificationPresentationOptions::Banner | UNNotificationPresentationOptions::List
            } else {
                UNNotificationPresentationOptions(0)
            },));
        }

        #[unsafe(method(userNotificationCenter:didReceiveNotificationResponse:withCompletionHandler:))]
        fn did_receive(
            &self,
            _center: &UNUserNotificationCenter,
            response: &UNNotificationResponse,
            completion: &block2::DynBlock<dyn Fn()>,
        ) {
            let identifier = response.notification().request().identifier().to_string();
            if let Some(key) = reminder_key(&identifier) {
                let _ = self.ivars().activated.try_send(key.to_owned());
            }
            completion.call(());
        }
    }
);

/// The reminder key a notification identifier carries, if it is a reminder's.
fn reminder_key(identifier: &str) -> Option<&str> {
    identifier
        .strip_prefix(REMINDER_IDENTIFIER)?
        .strip_prefix('.')
}

/// Overdue update reminders in Notification Center. Clicking one reports its key.
pub(crate) struct MacosUpdateReminders {
    /// The center holds its delegate weakly, so the reminders keep it.
    delegate: std::cell::RefCell<Option<Retained<ReminderDelegate>>>,
}

impl MacosUpdateReminders {
    pub(crate) fn new() -> Self {
        Self {
            delegate: std::cell::RefCell::new(None),
        }
    }
}

impl crate::updates::reminders::UpdateReminderAdapter for MacosUpdateReminders {
    fn activations(&self, activated: async_channel::Sender<String>) {
        let Some(center) = notification_center() else {
            return;
        };
        let delegate = ReminderDelegate::alloc().set_ivars(ReminderDelegateIvars { activated });
        // SAFETY: NSObject's init is its designated initializer.
        let delegate: Retained<ReminderDelegate> = unsafe { msg_send![super(delegate), init] };
        center.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
        *self.delegate.borrow_mut() = Some(delegate);
    }

    /// Asks for provisional authorization when the person has not decided, then delivers.
    fn notify(&self, key: &str, title: &str, body: &str) {
        let Some(center) = notification_center() else {
            return;
        };
        let identifier = format!("{REMINDER_IDENTIFIER}.{key}");
        let (title, body) = (title.to_owned(), body.to_owned());
        let deliver = RcBlock::new(
            move |_granted: Bool, _error: *mut objc2_foundation::NSError| {
                let content = UNMutableNotificationContent::new();
                content.setTitle(&NSString::from_str(&title));
                content.setBody(&NSString::from_str(&body));
                content.setThreadIdentifier(&NSString::from_str(REMINDER_IDENTIFIER));
                let request = UNNotificationRequest::requestWithIdentifier_content_trigger(
                    &NSString::from_str(&identifier),
                    &content,
                    None,
                );
                UNUserNotificationCenter::currentNotificationCenter()
                    .addNotificationRequest_withCompletionHandler(&request, None);
            },
        );
        center.requestAuthorizationWithOptions_completionHandler(
            UNAuthorizationOptions::Alert | UNAuthorizationOptions::Provisional,
            &deliver,
        );
    }
}

#[cfg(all(test, feature = "native-tests"))]
mod tests {
    use super::*;
    #[test]
    fn unbundled_test_process_has_no_native_notification_identity() {
        assert!(!process_has_application_bundle_identity());
        let adapter = UserNotificationAdapter::new(ApplicationIdentity::current());
        let (sender, settings) = std::sync::mpsc::channel();
        adapter.settings(Box::new(move |result| sender.send(result).unwrap()));
        assert!(matches!(
            settings
                .recv_timeout(std::time::Duration::from_secs(5))
                .unwrap(),
            Err(AttentionFailure::Unavailable)
        ));
        assert!(settings.try_recv().is_err());
        let (sender, authorization) = std::sync::mpsc::channel();
        adapter.authorize_provisionally(Box::new(move |result| sender.send(result).unwrap()));
        assert_eq!(
            authorization
                .recv_timeout(std::time::Duration::from_secs(5))
                .unwrap(),
            Err(AttentionFailure::Unavailable)
        );
        assert!(authorization.try_recv().is_err());
        assert_eq!(adapter.submit(3), Err(AttentionFailure::Unavailable));
        assert_eq!(adapter.clear(), Err(AttentionFailure::Unavailable));
    }
    #[test]
    fn unbundled_test_process_reports_notification_access_unavailable() {
        let access = MacosNotificationAccess::new(ApplicationIdentity::current());
        let (sender, read) = std::sync::mpsc::channel();
        access.read(Box::new(move |result| sender.send(result).unwrap()));
        assert_eq!(
            read.recv_timeout(std::time::Duration::from_secs(5))
                .unwrap(),
            Err(NotificationAccessError::Unavailable)
        );
        let (sender, request) = std::sync::mpsc::channel();
        access.request(Box::new(move |result| sender.send(result).unwrap()));
        assert_eq!(
            request
                .recv_timeout(std::time::Duration::from_secs(5))
                .unwrap(),
            Err(NotificationAccessError::Unavailable)
        );
    }

    #[test]
    fn notification_settings_open_the_desktops_own_entry() {
        assert_eq!(
            notification_settings_uri(ApplicationIdentity::development()),
            "x-apple.systempreferences:com.apple.Notifications-Settings.extension?id=com.peblon.spaceterm-development"
        );
    }

    #[test]
    fn native_authorization_values_map_to_closed_portable_facts() {
        assert_eq!(
            authorization_from_raw(0),
            NotificationAuthorization::NotDetermined
        );
        assert_eq!(authorization_from_raw(1), NotificationAuthorization::Denied);
        assert_eq!(
            authorization_from_raw(2),
            NotificationAuthorization::Authorized
        );
        assert_eq!(
            authorization_from_raw(3),
            NotificationAuthorization::Provisional
        );
        assert_eq!(
            authorization_from_raw(4),
            NotificationAuthorization::Unknown
        );
        assert_eq!(
            (UNAuthorizationOptions::Alert | UNAuthorizationOptions::Provisional).0,
            68
        );
    }
    #[test]
    fn objective_c_callback_types_match_supported_macos_abis() {
        assert_eq!(
            (
                size_of::<Bool>(),
                size_of::<NSInteger>(),
                size_of::<objc2_foundation::NSUInteger>()
            ),
            (1, 8, 8)
        );
    }
}
