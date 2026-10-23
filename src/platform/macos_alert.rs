//! Native alerts that show the application icon.
use objc2::{MainThreadMarker, rc::Retained};
use objc2_app_kit::{NSAlert, NSApplication};

/// Creates an alert that shows the application icon. macOS 26 omits the icon from a sheet alert
/// that keeps its default image, and it badges a Critical sheet's caution icon only with an
/// explicit one. App-modal alerts show the icon either way, so every SpaceTerm alert sets it and
/// both presentations match.
pub(super) fn application_alert(mtm: MainThreadMarker) -> Retained<NSAlert> {
    let alert = NSAlert::new(mtm);
    if let Some(icon) = NSApplication::sharedApplication(mtm).applicationIconImage() {
        // SAFETY: The icon is a live image, so the setter never receives nil.
        unsafe { alert.setIcon(Some(&icon)) };
    }
    alert
}
