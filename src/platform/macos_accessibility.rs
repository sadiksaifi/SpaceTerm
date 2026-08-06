use std::ops::Range;

use crate::terminal::AccessibilityNotification;

#[cfg(not(test))]
use cocoa::appkit::NSApp;
#[cfg(not(test))]
use cocoa::base::{id, nil};
#[cfg(not(test))]
use cocoa::foundation::{NSAutoreleasePool, NSString};
#[cfg(not(test))]
use objc::{msg_send, sel, sel_impl};

pub(crate) const VALUE_CHANGED: &str = "AXValueChanged";
pub(crate) const SELECTION_CHANGED: &str = "AXSelectedTextChanged";
pub(crate) const FOCUS_CHANGED: &str = "AXFocusedUIElementChanged";

pub(crate) fn notification_name(notification: AccessibilityNotification) -> &'static str {
    match notification {
        AccessibilityNotification::Value => VALUE_CHANGED,
        AccessibilityNotification::Selection => SELECTION_CHANGED,
        AccessibilityNotification::Focus => FOCUS_CHANGED,
    }
}

#[cfg(not(test))]
#[link(name = "AppKit", kind = "framework")]
unsafe extern "C" {
    fn NSAccessibilityPostNotification(element: id, notification: id);
}

#[cfg(not(test))]
pub(crate) fn post_accessibility_notifications(
    notifications: &[AccessibilityNotification],
    _visible_range: Range<usize>,
) {
    if notifications.is_empty() {
        return;
    }
    // SAFETY: GPUI renders on AppKit's main thread. The key window and content view are borrowed
    // only for these synchronous notification calls; notification names are copied NSStrings.
    unsafe {
        let pool = NSAutoreleasePool::new(nil);
        let application = NSApp();
        let window: id = msg_send![application, keyWindow];
        if window != nil {
            let view: id = msg_send![window, contentView];
            if view != nil {
                for notification in notifications {
                    let name = NSString::alloc(nil)
                        .init_str(notification_name(*notification))
                        .autorelease();
                    NSAccessibilityPostNotification(view, name);
                }
            }
        }
        pool.drain();
    }
}

#[cfg(test)]
pub(crate) fn post_accessibility_notifications(_: &[AccessibilityNotification], _: Range<usize>) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typed_notifications_map_to_native_accessibility_names_without_text() {
        assert_eq!(
            [
                AccessibilityNotification::Value,
                AccessibilityNotification::Selection,
                AccessibilityNotification::Focus,
            ]
            .map(notification_name),
            [VALUE_CHANGED, SELECTION_CHANGED, FOCUS_CHANGED]
        );
    }
}
