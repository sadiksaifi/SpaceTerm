use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};

#[cfg(not(test))]
use block::ConcreteBlock;
#[cfg(not(test))]
use cocoa::base::{id, nil};
#[cfg(not(test))]
use cocoa::foundation::{NSArray, NSAutoreleasePool, NSInteger, NSString, NSUInteger};
#[cfg(not(test))]
use objc::runtime::{BOOL, NO};
#[cfg(not(test))]
use objc::{class, msg_send, sel, sel_impl};

const NOTIFICATION_IDENTIFIER: &str = "io.github.sadiksaifi.spaceterm.terminal-attention";
const AUTHORIZATION_OPTION_ALERT: usize = 1 << 2;
const AUTHORIZATION_OPTION_PROVISIONAL: usize = 1 << 6;

static NOTIFICATION_GENERATION: AtomicU64 = AtomicU64::new(0);
static LATEST_DELIVERY_GENERATION: AtomicU64 = AtomicU64::new(0);
static LATEST_AGGREGATE_COUNT: AtomicU32 = AtomicU32::new(0);
static AUTHORIZATION_REQUEST_IN_FLIGHT: AtomicBool = AtomicBool::new(false);
static NATIVE_NOTIFICATION_LOCK: Mutex<()> = Mutex::new(());

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum NotificationAuthorization {
    NotDetermined,
    Denied,
    Authorized,
    Provisional,
    Unknown,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum AuthorizationDecision {
    Submit,
    RequestProvisional,
    Suppress,
}

impl NotificationAuthorization {
    const fn from_raw(raw: isize) -> Self {
        match raw {
            0 => Self::NotDetermined,
            1 => Self::Denied,
            2 => Self::Authorized,
            3 => Self::Provisional,
            _ => Self::Unknown,
        }
    }

    const fn permits_delivery(self, alert_enabled: bool, center_enabled: bool) -> bool {
        matches!(self, Self::Authorized | Self::Provisional) && (alert_enabled || center_enabled)
    }
}

const fn authorization_decision(
    authorization: NotificationAuthorization,
    alert_enabled: bool,
    center_enabled: bool,
) -> AuthorizationDecision {
    if authorization.permits_delivery(alert_enabled, center_enabled) {
        AuthorizationDecision::Submit
    } else if matches!(authorization, NotificationAuthorization::NotDetermined) {
        AuthorizationDecision::RequestProvisional
    } else {
        AuthorizationDecision::Suppress
    }
}

fn notification_body(aggregate_count: u32) -> String {
    format!("Terminal requested attention ({})", aggregate_count.max(1))
}

#[cfg(not(test))]
#[link(name = "UserNotifications", kind = "framework")]
unsafe extern "C" {}

#[cfg(not(test))]
pub(crate) fn deliver_terminal_attention(aggregate_count: u32) {
    let generation = NOTIFICATION_GENERATION
        .fetch_add(1, Ordering::AcqRel)
        .wrapping_add(1);
    let aggregate_count = aggregate_count.max(1);
    LATEST_AGGREGATE_COUNT.store(aggregate_count, Ordering::Relaxed);
    // Publish the generation last: its Release/Acquire edge makes the matching count visible to
    // an authorization callback without allowing a new generation to pair with an older count.
    LATEST_DELIVERY_GENERATION.store(generation, Ordering::Release);

    query_settings(generation, aggregate_count);
}

#[cfg(not(test))]
fn query_settings(generation: u64, aggregate_count: u32) {
    // SAFETY: UserNotifications owns a copied heap block for this asynchronous query. The block
    // captures only Copy values, reads borrowed callback objects synchronously, and never touches
    // GPUI state. A provisional request is noninterrupting and cannot move terminal focus.
    unsafe {
        let center: id = msg_send![class!(UNUserNotificationCenter), currentNotificationCenter];
        let settings = ConcreteBlock::new(move |settings: id| {
            if NOTIFICATION_GENERATION.load(Ordering::Acquire) != generation || settings == nil {
                return;
            }
            let authorization: NSInteger = msg_send![settings, authorizationStatus];
            let alert_setting: NSInteger = msg_send![settings, alertSetting];
            let center_setting: NSInteger = msg_send![settings, notificationCenterSetting];
            let authorization = NotificationAuthorization::from_raw(authorization as isize);
            match authorization_decision(authorization, alert_setting == 2, center_setting == 2) {
                AuthorizationDecision::Submit => {
                    submit_notification(generation, aggregate_count);
                }
                AuthorizationDecision::RequestProvisional => {
                    request_provisional_authorization();
                }
                AuthorizationDecision::Suppress => {}
            }
        })
        .copy();
        let _: () = msg_send![center, getNotificationSettingsWithCompletionHandler: &*settings];
    }
}

#[cfg(not(test))]
fn request_provisional_authorization() {
    if AUTHORIZATION_REQUEST_IN_FLIGHT
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        return;
    }

    // UNAuthorizationOptionAlert | UNAuthorizationOptionProvisional. Provisional authorization
    // is deliberately noninterrupting, so an inactive terminal notification never opens a prompt
    // or changes application, window, responder, or Pane focus.
    const OPTIONS: NSUInteger =
        (AUTHORIZATION_OPTION_ALERT | AUTHORIZATION_OPTION_PROVISIONAL) as NSUInteger;
    // SAFETY: The copied block captures no borrowed state. Callback arguments are read only during
    // invocation, no panic may cross the block trampoline, and all follow-up state is atomic.
    unsafe {
        let center: id = msg_send![class!(UNUserNotificationCenter), currentNotificationCenter];
        let completion = ConcreteBlock::new(move |granted: BOOL, error: id| {
            AUTHORIZATION_REQUEST_IN_FLIGHT.store(false, Ordering::Release);
            if granted == NO || error != nil {
                return;
            }
            let generation = NOTIFICATION_GENERATION.load(Ordering::Acquire);
            if LATEST_DELIVERY_GENERATION.load(Ordering::Acquire) != generation {
                return;
            }
            query_settings(
                generation,
                LATEST_AGGREGATE_COUNT.load(Ordering::Acquire).max(1),
            );
        })
        .copy();
        let _: () = msg_send![
            center,
            requestAuthorizationWithOptions: OPTIONS
            completionHandler: &*completion
        ];
    }
}

#[cfg(test)]
pub(crate) fn deliver_terminal_attention(_: u32) {}

#[cfg(not(test))]
fn submit_notification(generation: u64, aggregate_count: u32) {
    let _native = NATIVE_NOTIFICATION_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if NOTIFICATION_GENERATION.load(Ordering::Acquire) != generation {
        return;
    }

    // SAFETY: This callback owns an autorelease pool, releases every +1 object, and submits only
    // immutable NSString-backed content. UserNotifications retains the request synchronously.
    unsafe {
        let pool = NSAutoreleasePool::new(nil);
        let center: id = msg_send![class!(UNUserNotificationCenter), currentNotificationCenter];
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
        let request: id = msg_send![
            class!(UNNotificationRequest),
            requestWithIdentifier: identifier
            content: content
            trigger: nil
        ];
        if NOTIFICATION_GENERATION.load(Ordering::Acquire) == generation {
            let _: () =
                msg_send![center, addNotificationRequest: request withCompletionHandler: nil];
        }
        let _: () = msg_send![content, release];
        pool.drain();
    }
}

pub(crate) fn clear_terminal_attention() {
    let _native = NATIVE_NOTIFICATION_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    NOTIFICATION_GENERATION.fetch_add(1, Ordering::AcqRel);
    clear_delivered_notification();
}

#[cfg(not(test))]
fn clear_delivered_notification() {
    // SAFETY: All temporary Objective-C objects stay within an autorelease pool. The center copies
    // the identifier array synchronously and no terminal or Pane data crosses the native boundary.
    unsafe {
        let pool = NSAutoreleasePool::new(nil);
        let center: id = msg_send![class!(UNUserNotificationCenter), currentNotificationCenter];
        let identifier = NSString::alloc(nil)
            .init_str(NOTIFICATION_IDENTIFIER)
            .autorelease();
        let identifiers = NSArray::arrayWithObjects(nil, &[identifier]);
        let _: () =
            msg_send![center, removePendingNotificationRequestsWithIdentifiers: identifiers];
        let _: () = msg_send![center, removeDeliveredNotificationsWithIdentifiers: identifiers];
        pool.drain();
    }
}

#[cfg(test)]
fn clear_delivered_notification() {}

#[cfg(test)]
mod tests {
    use std::mem::size_of;

    use cocoa::foundation::{NSInteger, NSUInteger};
    use objc::runtime::BOOL;

    use super::*;

    #[test]
    fn authorized_and_provisional_policy_permits_only_enabled_native_surfaces() {
        assert_eq!(
            authorization_decision(NotificationAuthorization::Authorized, true, false),
            AuthorizationDecision::Submit
        );
        assert_eq!(
            authorization_decision(NotificationAuthorization::Provisional, false, true),
            AuthorizationDecision::Submit
        );
        assert_eq!(
            authorization_decision(NotificationAuthorization::Authorized, false, false),
            AuthorizationDecision::Suppress
        );
    }

    #[test]
    fn denied_and_future_authorization_suppress_while_undetermined_requests_provisional() {
        assert_eq!(
            authorization_decision(NotificationAuthorization::Denied, true, true),
            AuthorizationDecision::Suppress
        );
        assert_eq!(
            authorization_decision(NotificationAuthorization::NotDetermined, true, true),
            AuthorizationDecision::RequestProvisional
        );
        assert_eq!(
            authorization_decision(NotificationAuthorization::Unknown, true, true),
            AuthorizationDecision::Suppress
        );
    }

    #[test]
    fn authorization_values_map_to_explicit_native_policy() {
        assert_eq!(
            NotificationAuthorization::from_raw(0),
            NotificationAuthorization::NotDetermined
        );
        assert_eq!(
            NotificationAuthorization::from_raw(1),
            NotificationAuthorization::Denied
        );
        assert_eq!(
            NotificationAuthorization::from_raw(2),
            NotificationAuthorization::Authorized
        );
        assert_eq!(
            NotificationAuthorization::from_raw(3),
            NotificationAuthorization::Provisional
        );
        assert_eq!(
            NotificationAuthorization::from_raw(4),
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
                size_of::<NSUInteger>(),
            ),
            (1, 8, 8)
        );
    }

    #[test]
    fn notification_identity_and_body_are_application_scoped_and_content_free() {
        assert_eq!(
            NOTIFICATION_IDENTIFIER,
            "io.github.sadiksaifi.spaceterm.terminal-attention"
        );
        assert!(!NOTIFICATION_IDENTIFIER.contains("pane"));
        assert_eq!(notification_body(2), "Terminal requested attention (2)");
    }
}
