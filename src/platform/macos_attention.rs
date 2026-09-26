use objc2::MainThreadMarker;
use objc2_app_kit::{NSApplication, NSRequestUserAttentionType};
use objc2_foundation::NSInteger;

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
        let mtm = MainThreadMarker::new().ok_or(AttentionFailure::Unavailable)?;
        let request: NSInteger = NSApplication::sharedApplication(mtm)
            .requestUserAttention(NSRequestUserAttentionType::InformationalRequest);
        if request < 0 {
            return Err(AttentionFailure::Unavailable);
        }
        self.request = Some(request);
        Ok(())
    }

    fn cancel(&mut self) {
        if let Some(request) = self.request.take()
            && let Some(mtm) = MainThreadMarker::new()
        {
            NSApplication::sharedApplication(mtm).cancelUserAttentionRequest(request);
        }
    }
}

impl Drop for AppKitDockAttention {
    fn drop(&mut self) {
        self.cancel();
    }
}
