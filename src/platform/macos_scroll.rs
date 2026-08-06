use cocoa::appkit::{NSApp, NSEvent, NSEventType};
use cocoa::base::{id, nil};
use objc::{msg_send, sel, sel_impl};

use crate::terminal::WheelPhase;

const PHASE_BEGAN: u64 = 1;
const PHASE_ENDED: u64 = 8;
const PHASE_CANCELLED: u64 = 16;

pub(crate) fn current_wheel_phase() -> Option<WheelPhase> {
    // SAFETY: GPUI invokes this bridge synchronously while AppKit dispatches the scroll NSEvent.
    unsafe {
        let application = NSApp();
        if application == nil {
            return None;
        }
        let event: id = msg_send![application, currentEvent];
        if event == nil || event.eventType() != NSEventType::NSScrollWheel {
            return None;
        }
        let momentum: u64 = msg_send![event, momentumPhase];
        let gesture: u64 = msg_send![event, phase];
        Some(classify_phase(gesture, momentum))
    }
}

const fn classify_phase(gesture: u64, momentum: u64) -> WheelPhase {
    if momentum & PHASE_CANCELLED != 0 {
        WheelPhase::MomentumCancelled
    } else if momentum & PHASE_BEGAN != 0 {
        WheelPhase::MomentumStarted
    } else if momentum & PHASE_ENDED != 0 {
        WheelPhase::MomentumEnded
    } else if momentum != 0 {
        WheelPhase::MomentumChanged
    } else if gesture & PHASE_CANCELLED != 0 {
        WheelPhase::GestureCancelled
    } else if gesture & PHASE_BEGAN != 0 {
        WheelPhase::GestureStarted
    } else if gesture & PHASE_ENDED != 0 {
        WheelPhase::GestureEnded
    } else {
        WheelPhase::GestureChanged
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_momentum_phase_takes_precedence_over_gesture_phase() {
        assert_eq!(
            classify_phase(PHASE_ENDED, PHASE_BEGAN),
            WheelPhase::MomentumStarted
        );
        assert_eq!(
            classify_phase(0, PHASE_CANCELLED),
            WheelPhase::MomentumCancelled
        );
        assert_eq!(classify_phase(PHASE_ENDED, 0), WheelPhase::GestureEnded);
    }
}
