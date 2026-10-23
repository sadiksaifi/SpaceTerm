//! Native alerts that show the application icon.
use objc2::{MainThreadMarker, rc::Retained};
use objc2_app_kit::{NSAlert, NSApplication};

/// Creates an alert that shows the application icon. macOS 26 omits the icon from an alert that
/// keeps its default image, and it badges a Critical alert's caution icon only with an explicit
/// one, so every SpaceTerm alert sets the application icon itself.
pub(super) fn application_alert(mtm: MainThreadMarker) -> Retained<NSAlert> {
    let alert = NSAlert::new(mtm);
    if let Some(icon) = NSApplication::sharedApplication(mtm).applicationIconImage() {
        // SAFETY: The icon is a live image, so the setter never receives nil.
        unsafe { alert.setIcon(Some(&icon)) };
    }
    alert
}
