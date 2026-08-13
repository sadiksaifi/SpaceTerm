#[cfg(not(test))]
use cocoa::appkit::NSApp;
#[cfg(not(test))]
use cocoa::base::nil;
#[cfg(not(test))]
use objc::runtime::{BOOL, NO};
#[cfg(not(test))]
use objc::{msg_send, sel, sel_impl};

#[cfg(all(not(test), target_arch = "aarch64"))]
const _: BOOL = false;

#[cfg(all(not(test), target_arch = "x86_64"))]
const _: BOOL = 0_i8;

#[cfg(not(test))]
#[allow(
    unexpected_cfgs,
    reason = "objc 0.2's msg_send macro probes its historical cargo-clippy cfg"
)]
pub(crate) fn is_active() -> bool {
    // SAFETY: NSApp and isActive are read synchronously on GPUI's AppKit thread.
    unsafe {
        let application = NSApp();
        if application == nil {
            return false;
        }
        let active: BOOL = msg_send![application, isActive];
        !matches!(active, NO)
    }
}

#[cfg(test)]
pub(crate) const fn is_active() -> bool {
    true
}

#[cfg(test)]
mod tests {
    use std::mem::size_of;

    use objc::runtime::BOOL;

    #[test]
    fn objective_c_bool_has_the_supported_macos_abi_width() {
        assert_eq!(size_of::<BOOL>(), 1);
    }
}
