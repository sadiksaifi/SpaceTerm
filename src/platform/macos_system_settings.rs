//! Opens a System Settings list from the desktop, the active application when a person asks, so
//! macOS brings System Settings forward.

use objc2::MainThreadMarker;
use objc2_app_kit::NSWorkspace;
use objc2_foundation::{NSString, NSURL};

/// Whether Launch Services accepted `uri`. It must run on the main thread.
pub(crate) fn open(uri: &'static str) -> bool {
    if MainThreadMarker::new().is_none() {
        return false;
    }
    NSURL::URLWithString(&NSString::from_str(uri))
        .is_some_and(|url| NSWorkspace::sharedWorkspace().openURL(&url))
}
