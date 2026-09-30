//! Linux wheel events carry their gesture phase in GPUI, so no native detail enriches them.
use gpui::Window;

use crate::terminal::wheel_phase::{WheelPhaseDetail, WheelPhaseEnrichment};

pub(super) struct LinuxWheelPhaseEnrichment;

impl WheelPhaseEnrichment for LinuxWheelPhaseEnrichment {
    fn current(&self, _: &Window) -> Option<WheelPhaseDetail> {
        None
    }
}
