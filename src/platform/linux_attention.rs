//! Bell and urgency are delivered on the application thread to Workspace windows.
use super::linux_desktop_events::DesktopEventSender;
use crate::terminal::attention_runtime::{AttentionFailure, AudioBell, DockAttentionDriver};
use super::linux_desktop_events::DesktopEventSender;
pub(super) struct LinuxAudioBell(pub(super) DesktopEventSender);
impl AudioBell for LinuxAudioBell { fn play(&mut self) { self.0.bell(); } }
pub(super) struct LinuxWindowAttention(pub(super) DesktopEventSender);
impl DockAttentionDriver for LinuxWindowAttention {
    fn request(&mut self) -> Result<(), AttentionFailure> { self.0.attention(true); Ok(()) }
    fn cancel(&mut self) { self.0.attention(false); }
}
impl Drop for LinuxWindowAttention { fn drop(&mut self) { self.0.attention(false); } }
