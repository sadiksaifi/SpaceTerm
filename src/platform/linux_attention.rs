//! Linux Terminal Attention signals. The bell and urgency hint arrive with a later desktop wave;
//! until then both report that the capability is unavailable without failing the caller.
use crate::terminal::attention_runtime::{AttentionFailure, AudioBell, DockAttentionDriver};

pub(super) struct LinuxAudioBell;

impl AudioBell for LinuxAudioBell {
    fn play(&mut self) {}
}

pub(super) struct LinuxWindowAttention;

impl DockAttentionDriver for LinuxWindowAttention {
    fn request(&mut self) -> Result<(), AttentionFailure> {
        Err(AttentionFailure::Unavailable)
    }

    fn cancel(&mut self) {}
}
