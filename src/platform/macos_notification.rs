use std::sync::Mutex;

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::Bool;
use objc2_foundation::{NSArray, NSBundle, NSInteger, NSString};
use objc2_user_notifications::{
    UNAuthorizationOptions, UNMutableNotificationContent, UNNotificationRequest,
    UNNotificationSetting, UNUserNotificationCenter,
};

use crate::application_identity::ApplicationIdentity;
use crate::terminal::attention_notification::{
    AuthorizationCompletion, NotificationAdapter, NotificationAuthorization, NotificationSettings,
    SettingsCompletion, notification_body,
};
use crate::terminal::attention_runtime::AttentionFailure;

const NOTIFICATION_IDENTIFIER: &str = "io.github.sadiksaifi.spaceterm.terminal-attention";
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

#[cfg(all(test, feature = "macos-native-tests"))]
mod tests {
    use super::*;
    #[test]
    fn unbundled_test_process_has_no_native_notification_identity() {
        assert!(!process_has_application_bundle_identity());
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
