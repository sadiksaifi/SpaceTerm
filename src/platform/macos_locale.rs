use cocoa::appkit::NSApplication;
use cocoa::base::nil;
use cocoa::foundation::NSInteger;
use objc::{msg_send, sel, sel_impl};
use spaceterm_ui::TextDirection;

const NS_USER_INTERFACE_LAYOUT_DIRECTION_RIGHT_TO_LEFT: NSInteger = 1;

/// Resolves the logical direction AppKit selected for the current application locale.
pub(crate) fn current_text_direction() -> TextDirection {
    // SAFETY: UI initialization runs on GPUI's AppKit thread. NSApplication owns the shared
    // application, and userInterfaceLayoutDirection returns a scalar with no transferred lifetime.
    let native_direction = unsafe {
        let application = NSApplication::sharedApplication(nil);
        msg_send![application, userInterfaceLayoutDirection]
    };
    text_direction_from_native(native_direction)
}

const fn text_direction_from_native(native_direction: NSInteger) -> TextDirection {
    if native_direction == NS_USER_INTERFACE_LAYOUT_DIRECTION_RIGHT_TO_LEFT {
        TextDirection::RightToLeft
    } else {
        TextDirection::LeftToRight
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_layout_direction_maps_to_bounded_locale_behavior() {
        assert_eq!(
            (
                text_direction_from_native(0),
                text_direction_from_native(NS_USER_INTERFACE_LAYOUT_DIRECTION_RIGHT_TO_LEFT),
            ),
            (TextDirection::LeftToRight, TextDirection::RightToLeft)
        );
    }
}
