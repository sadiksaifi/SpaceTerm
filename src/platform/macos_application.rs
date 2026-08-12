use cocoa::appkit::NSApp;
use cocoa::base::nil;
use objc::{msg_send, sel, sel_impl};

#[allow(
    unexpected_cfgs,
    reason = "objc 0.2's msg_send macro probes its historical cargo-clippy cfg"
)]
pub(crate) fn application_is_active() -> bool {
    // SAFETY: NSApp and isActive are read synchronously on GPUI's AppKit thread.
    unsafe {
        let application = NSApp();
        application != nil && msg_send![application, isActive]
    }
}
