#[cfg(not(test))]
use objc2::MainThreadMarker;
#[cfg(not(test))]
use objc2_app_kit::NSApplication;

#[cfg(not(test))]
pub(crate) fn is_active() -> bool {
    MainThreadMarker::new().is_some_and(|mtm| NSApplication::sharedApplication(mtm).isActive())
}

#[cfg(test)]
pub(crate) const fn is_active() -> bool {
    true
}

pub(crate) struct MacosApplicationActivity;

impl super::application_activity::ApplicationActivity for MacosApplicationActivity {
    fn is_active(&self, _: &gpui::App) -> bool {
        is_active()
    }
}

/// The Open Application event's launch property, `keyAEPropData`.
#[cfg(not(test))]
const LAUNCH_PROPERTY: u32 = u32::from_be_bytes(*b"prdt");
/// The launch property value of an application macOS opens at login, `keyAELaunchedAsLogInItem`.
#[cfg(not(test))]
const LAUNCHED_AT_LOGIN: u32 = u32::from_be_bytes(*b"lgit");

/// Whether macOS opened SpaceTerm at login, as a Login Item or by reopening it with the person's
/// windows, rather than the person opening it. Read it while AppKit finishes launching, when the
/// Open Application event is the current Apple Event.
#[cfg(not(test))]
pub(crate) fn launched_at_login() -> bool {
    use objc2::msg_send;
    use objc2::runtime::{AnyClass, AnyObject};
    let Some(manager_class) = AnyClass::get(c"NSAppleEventManager") else {
        return false;
    };
    // SAFETY: These Foundation selectors take no arguments or one FourCharCode, return
    // autoreleased objects or FourCharCodes, and run on the main thread during launch.
    unsafe {
        let manager: *mut AnyObject = msg_send![manager_class, sharedAppleEventManager];
        let Some(manager) = manager.as_ref() else {
            return false;
        };
        let event: *mut AnyObject = msg_send![manager, currentAppleEvent];
        let Some(event) = event.as_ref() else {
            return false;
        };
        let property: *mut AnyObject = msg_send![event, paramDescriptorForKeyword: LAUNCH_PROPERTY];
        let Some(property) = property.as_ref() else {
            return false;
        };
        let value: u32 = msg_send![property, enumCodeValue];
        value == LAUNCHED_AT_LOGIN
    }
}

#[cfg(test)]
pub(crate) const fn launched_at_login() -> bool {
    false
}
