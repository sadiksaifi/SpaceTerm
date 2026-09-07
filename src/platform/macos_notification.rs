use std::sync::Mutex;

use block::ConcreteBlock;
use cocoa::base::{id, nil};
use cocoa::foundation::{NSArray, NSAutoreleasePool, NSInteger, NSString, NSUInteger};
use objc::runtime::{BOOL, NO};
use objc::{class, msg_send, sel, sel_impl};

use crate::terminal::attention_notification::{
    AuthorizationCompletion, NotificationAdapter, NotificationAuthorization, NotificationSettings,
    SettingsCompletion, notification_body,
};
use crate::terminal::attention_runtime::AttentionFailure;

const NOTIFICATION_IDENTIFIER: &str = "io.github.sadiksaifi.spaceterm.terminal-attention";
const AUTHORIZATION_OPTION_ALERT: usize = 1 << 2;
const AUTHORIZATION_OPTION_PROVISIONAL: usize = 1 << 6;

#[link(name = "UserNotifications", kind = "framework")]
unsafe extern "C" {}

fn process_has_application_bundle_identity() -> bool {
    // SAFETY: NSBundle owns both returned objects; their presence is checked synchronously.
    unsafe {
        let bundle: id = msg_send![class!(NSBundle), mainBundle];
        if bundle == nil {
            return false;
        }
        let identifier: id = msg_send![bundle, bundleIdentifier];
        identifier != nil
    }
}

fn notification_center() -> Option<id> {
    if !process_has_application_bundle_identity() {
        return None;
    }
    // SAFETY: The framework permits this operation only with a process application-bundle identity.
    let center: id =
        unsafe { msg_send![class!(UNUserNotificationCenter), currentNotificationCenter] };
    (center != nil).then_some(center)
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

pub(crate) struct UserNotificationAdapter;

impl NotificationAdapter for UserNotificationAdapter {
    fn settings(&self, completion: SettingsCompletion) {
        let Some(center) = notification_center() else {
            completion(Err(AttentionFailure::Unavailable));
            return;
        };
        let completion = Mutex::new(Some(completion));
        // SAFETY: The framework copies this block. Borrowed settings are read only during callback;
        // portable callbacks carry owned closed values and never retain Objective-C objects.
        unsafe {
            let settings = ConcreteBlock::new(move |settings: id| {
                let Some(completion) = completion
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .take()
                else {
                    return;
                };
                if settings == nil {
                    completion(Err(AttentionFailure::Unavailable));
                    return;
                }
                let authorization: NSInteger = msg_send![settings, authorizationStatus];
                let alert_setting: NSInteger = msg_send![settings, alertSetting];
                let center_setting: NSInteger = msg_send![settings, notificationCenterSetting];
                completion(Ok(NotificationSettings {
                    authorization: authorization_from_raw(authorization),
                    alert_enabled: alert_setting == 2,
                    center_enabled: center_setting == 2,
                }));
            })
            .copy();
            let _: () = msg_send![center, getNotificationSettingsWithCompletionHandler: &*settings];
        }
    }

    fn authorize_provisionally(&self, completion: AuthorizationCompletion) {
        let Some(center) = notification_center() else {
            completion(Err(AttentionFailure::Unavailable));
            return;
        };
        let completion = Mutex::new(Some(completion));
        const OPTIONS: NSUInteger =
            (AUTHORIZATION_OPTION_ALERT | AUTHORIZATION_OPTION_PROVISIONAL) as NSUInteger;
        // SAFETY: The copied block owns its callback. Provisional authorization is noninterrupting
        // and cannot change application, Operating-System Window, responder, or Pane focus.
        unsafe {
            let completion = ConcreteBlock::new(move |granted: BOOL, error: id| {
                let Some(completion) = completion
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .take()
                else {
                    return;
                };
                completion(if error != nil {
                    Err(AttentionFailure::DeliveryFailed)
                } else if granted == NO {
                    Err(AttentionFailure::Denied)
                } else {
                    Ok(())
                });
            })
            .copy();
            let _: () = msg_send![center, requestAuthorizationWithOptions: OPTIONS completionHandler: &*completion];
        }
    }

    fn submit(&self, aggregate_count: u32) -> Result<(), AttentionFailure> {
        let center = notification_center().ok_or(AttentionFailure::Unavailable)?;
        // SAFETY: This callback owns its autorelease pool and releases each +1 object. The center
        // retains the immutable request synchronously. Portable ownership serializes submit/clear.
        unsafe {
            let pool = NSAutoreleasePool::new(nil);
            let content: id = msg_send![class!(UNMutableNotificationContent), new];
            let title = NSString::alloc(nil).init_str("SpaceTerm").autorelease();
            let body = NSString::alloc(nil)
                .init_str(&notification_body(aggregate_count))
                .autorelease();
            let identifier = NSString::alloc(nil)
                .init_str(NOTIFICATION_IDENTIFIER)
                .autorelease();
            let _: () = msg_send![content, setTitle: title];
            let _: () = msg_send![content, setBody: body];
            let _: () = msg_send![content, setThreadIdentifier: identifier];
            let request: id = msg_send![class!(UNNotificationRequest), requestWithIdentifier: identifier content: content trigger: nil];
            let _: () =
                msg_send![center, addNotificationRequest: request withCompletionHandler: nil];
            let _: () = msg_send![content, release];
            pool.drain();
        }
        Ok(())
    }

    fn clear(&self) -> Result<(), AttentionFailure> {
        let center = notification_center().ok_or(AttentionFailure::Unavailable)?;
        // SAFETY: The framework copies the identifier array synchronously. No Pane or terminal
        // content crosses the native boundary and temporaries stay within the autorelease pool.
        unsafe {
            let pool = NSAutoreleasePool::new(nil);
            let identifier = NSString::alloc(nil)
                .init_str(NOTIFICATION_IDENTIFIER)
                .autorelease();
            let identifiers = NSArray::arrayWithObjects(nil, &[identifier]);
            let _: () =
                msg_send![center, removePendingNotificationRequestsWithIdentifiers: identifiers];
            let _: () = msg_send![center, removeDeliveredNotificationsWithIdentifiers: identifiers];
            pool.drain();
        }
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
            AUTHORIZATION_OPTION_ALERT | AUTHORIZATION_OPTION_PROVISIONAL,
            68
        );
    }
    #[test]
    fn objective_c_callback_types_match_supported_macos_abis() {
        assert_eq!(
            (
                size_of::<BOOL>(),
                size_of::<NSInteger>(),
                size_of::<NSUInteger>()
            ),
            (1, 8, 8)
        );
    }
}
