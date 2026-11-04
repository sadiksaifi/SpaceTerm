//! Login Items & Extensions: whether macOS lets a LaunchAgent run, as System Settings shows it,
//! and the list where the person turns it on. Reading the status changes nothing.

use std::path::Path;

use objc2::msg_send;
use objc2::runtime::AnyClass;
use objc2_foundation::{NSString, NSURL};

#[link(name = "ServiceManagement", kind = "framework")]
unsafe extern "C" {}

/// `SMAppServiceStatusRequiresApproval`: the item is registered and turned off in Login Items.
const REQUIRES_APPROVAL: isize = 2;

/// The System Settings list of login items and background items.
pub(crate) const LOGIN_ITEMS: &str =
    "x-apple.systempreferences:com.apple.LoginItems-Settings.extension";

/// Whether the LaunchAgent at `plist` is registered but turned off in Login Items, so launchd
/// won't run it until the person turns it on.
pub(crate) fn turned_off(plist: &Path) -> bool {
    let (Some(class), Some(path)) = (AnyClass::get(c"SMAppService"), plist.to_str()) else {
        return false;
    };
    let url = NSURL::fileURLWithPath(&NSString::from_str(path));
    // SAFETY: `+[SMAppService statusForLegacyURL:]` takes a file URL and returns an NSInteger
    // status. It reads Background Task Management's record and changes nothing.
    let status: isize = unsafe { msg_send![class, statusForLegacyURL: &*url] };
    status == REQUIRES_APPROVAL
}
