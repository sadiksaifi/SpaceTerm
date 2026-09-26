use cocoa::appkit::NSApplication;
use cocoa::base::nil;
use cocoa::foundation::NSInteger;
use objc::{msg_send, sel, sel_impl};

use crate::terminal::attention_runtime::{AttentionFailure, AudioBell, DockAttentionDriver};

pub(crate) struct AppKitAudioBell;

impl AudioBell for AppKitAudioBell {
    fn play(&mut self) {
        unsafe extern "C" {
            fn NSBeep();
        }
        // SAFETY: AppKit's process-global bell takes no arguments and runs on GPUI's UI thread.
        unsafe { NSBeep() };
    }
}

#[derive(Default)]
pub(crate) struct AppKitDockAttention {
    request: Option<NSInteger>,
}

impl DockAttentionDriver for AppKitDockAttention {
    fn request(&mut self) -> Result<(), AttentionFailure> {
        self.cancel();
        // SAFETY: GPUI's UI thread sends documented NSApplication messages. The adapter alone owns
        // the returned native identity; no native value crosses the portable capability interface.
        let request: NSInteger = unsafe {
            let application = NSApplication::sharedApplication(nil);
            msg_send![application, requestUserAttention: 10_u64]
        };
        if request < 0 {
            return Err(AttentionFailure::Unavailable);
        }
        self.request = Some(request);
        Ok(())
    }

    fn cancel(&mut self) {
        if let Some(request) = self.request.take() {
            // SAFETY: The request identity came from NSApplication and cancellation stays on UI.
            unsafe {
                let application = NSApplication::sharedApplication(nil);
                let _: () = msg_send![application, cancelUserAttentionRequest: request];
            }
        }
    }
}

impl Drop for AppKitDockAttention {
    fn drop(&mut self) {
        self.cancel();
    }
}
