//! Bell and urgency are delivered on the application thread to their originating windows.
use super::linux_desktop_events::DesktopEventSender;
use crate::terminal::attention_runtime::{AttentionFailure, AudioBell, DockAttentionDriver};
pub(super) struct LinuxAudioBell(pub(super) DesktopEventSender);
impl AudioBell for LinuxAudioBell {
    fn play(&mut self, window: Option<gpui::AnyWindowHandle>) {
        if let Some(window) = window {
            self.0.bell(window);
        }
    }
}
pub(super) struct LinuxWindowAttention {
    events: DesktopEventSender,
    windows: Vec<gpui::AnyWindowHandle>,
    requested: bool,
}
impl LinuxWindowAttention {
    pub(super) fn new(events: DesktopEventSender) -> Self {
        Self {
            events,
            windows: Vec::new(),
            requested: false,
        }
    }
}
impl DockAttentionDriver for LinuxWindowAttention {
    fn set_windows(&mut self, windows: &[gpui::AnyWindowHandle]) {
        if self.requested {
            for window in &self.windows {
                if !windows.contains(window) {
                    self.events.attention(*window, false);
                }
            }
            for window in windows {
                if !self.windows.contains(window) {
                    self.events.attention(*window, true);
                }
            }
        }
        self.windows = windows.to_vec();
    }
    fn request(&mut self) -> Result<(), AttentionFailure> {
        self.requested = true;
        for window in &self.windows {
            self.events.attention(*window, true);
        }
        Ok(())
    }
    fn cancel(&mut self) {
        if std::mem::take(&mut self.requested) {
            for window in &self.windows {
                self.events.attention(*window, false);
            }
        }
    }
}
impl Drop for LinuxWindowAttention {
    fn drop(&mut self) {
        self.cancel();
    }
}
