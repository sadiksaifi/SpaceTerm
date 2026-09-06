use cocoa::appkit::{NSApp, NSEvent, NSEventType};
use cocoa::base::{id, nil};
use gpui::Window;
use objc::runtime::Object;
use objc::{msg_send, sel, sel_impl};
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
        let view = handle.ns_view.as_ptr().cast::<Object>();
        // SAFETY: this synchronous GPUI wheel callback runs on AppKit's thread. The current
        // event is accepted only when it belongs to this exact GPUI-backed NSWindow.
        unsafe {
            let application = NSApp();
            if application == nil {
                return None;
            }
            let event: id = msg_send![application, currentEvent];
            if event == nil || event.eventType() != NSEventType::NSScrollWheel {
                return None;
            }
            let native_window: id = msg_send![view, window];
            let event_window: id = msg_send![event, window];
            if native_window == nil || event_window != native_window {
                return None;
            }
            let momentum: u64 = msg_send![event, momentumPhase];
            let gesture: u64 = msg_send![event, phase];
            classify_detail(gesture, momentum)
        }
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

#[cfg(test)]
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
    fn native_normal_gesture_classification_is_deleted() {
        for gesture in [0, PHASE_BEGAN, 2, PHASE_ENDED, 32] {
            assert_eq!(classify_detail(gesture, 0), None);
        }
    }
}
