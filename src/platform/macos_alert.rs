//! Native alerts that always show an icon.
use objc2::{MainThreadMarker, rc::Retained};
use objc2_app_kit::{NSAlert, NSAlertStyle, NSApplication};

/// What an alert asks of the person, which decides its icon.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum AlertSeverity {
    /// Information, a problem report, or a choice that loses nothing. Shows the application
    /// icon.
    Standard,
    /// A choice that can lose work people may not expect to lose, such as programs running in
    /// Terminal Sessions. Shows the caution symbol badged with the application icon. Apple's
    /// Human Interface Guidelines reserve it for these choices, so a choice that only removes
    /// what the person asked to remove stays Standard.
    Caution,
}

/// Creates an alert whose icon matches `severity`. macOS 26 omits the application icon from a
/// sheet alert that keeps its default image, and it badges a sheet's caution symbol only with an
/// explicit one. App-modal alerts show the icon either way, so every alert sets it and both
/// presentations match.
pub(super) fn application_alert(
    severity: AlertSeverity,
    mtm: MainThreadMarker,
) -> Retained<NSAlert> {
    let alert = NSAlert::new(mtm);
    alert.setAlertStyle(match severity {
        AlertSeverity::Standard => NSAlertStyle::Informational,
        AlertSeverity::Caution => NSAlertStyle::Critical,
    });
    if let Some(icon) = NSApplication::sharedApplication(mtm).applicationIconImage() {
        // SAFETY: The icon is a live image, so the setter never receives nil.
        unsafe { alert.setIcon(Some(&icon)) };
    }
    alert
}
