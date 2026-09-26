use gpui::Window;
use objc2::MainThreadMarker;
use objc2_app_kit::{NSApplication, NSEventType, NSView};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};

use crate::terminal::wheel_phase::{WheelPhaseDetail, WheelPhaseEnrichment};

const PHASE_BEGAN: u64 = 1;
const PHASE_ENDED: u64 = 8;
const PHASE_CANCELLED: u64 = 16;

pub(crate) struct MacosWheelPhaseEnrichment;

impl WheelPhaseEnrichment for MacosWheelPhaseEnrichment {
    fn current(&self, window: &Window) -> Option<WheelPhaseDetail> {
        let handle = HasWindowHandle::window_handle(window).ok()?;
        let RawWindowHandle::AppKit(handle) = handle.as_raw() else {
            return None;
        };
        let mtm = MainThreadMarker::new()?;
        // SAFETY: GPUI owns this live NSView for the duration of the synchronous wheel callback.
        let view = unsafe { &*handle.ns_view.as_ptr().cast::<NSView>() };
        let event = NSApplication::sharedApplication(mtm).currentEvent()?;
        if event.r#type() != NSEventType::ScrollWheel {
            return None;
        }
        let native_window = view.window()?;
        let event_window = event.window(mtm)?;
        if !std::ptr::eq(&*native_window, &*event_window) {
            return None;
        }
        classify_detail(event.phase().0 as u64, event.momentumPhase().0 as u64)
    }
}

const fn classify_detail(gesture: u64, momentum: u64) -> Option<WheelPhaseDetail> {
    if momentum & PHASE_CANCELLED != 0 {
        Some(WheelPhaseDetail::MomentumCancelled)
    } else if momentum & PHASE_BEGAN != 0 {
        Some(WheelPhaseDetail::MomentumStarted)
    } else if momentum & PHASE_ENDED != 0 {
        Some(WheelPhaseDetail::MomentumEnded)
    } else if momentum != 0 {
        Some(WheelPhaseDetail::MomentumChanged)
    } else if gesture & PHASE_CANCELLED != 0 {
        Some(WheelPhaseDetail::GestureCancelled)
    } else {
        None
    }
}

#[cfg(all(test, feature = "macos-native-tests"))]
mod tests {
    use super::*;

    #[test]
    fn native_momentum_phase_takes_precedence_over_gesture_phase() {
        assert_eq!(
            classify_detail(PHASE_ENDED, PHASE_BEGAN),
            Some(WheelPhaseDetail::MomentumStarted)
        );
        assert_eq!(
            classify_detail(0, PHASE_CANCELLED),
            Some(WheelPhaseDetail::MomentumCancelled)
        );
        assert_eq!(
            classify_detail(PHASE_CANCELLED, 0),
            Some(WheelPhaseDetail::GestureCancelled)
        );
    }

    #[test]
    fn ordinary_gesture_phases_need_no_native_enrichment() {
        for gesture in [0, PHASE_BEGAN, 2, PHASE_ENDED, 32] {
            assert_eq!(classify_detail(gesture, 0), None);
        }
    }
}
