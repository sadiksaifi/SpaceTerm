//! GPUI owns ordinary gesture phases; optional native facts only fill its missing distinctions.

use gpui::{TouchPhase, Window};

use super::WheelPhase;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum WheelPhaseDetail {
    GestureCancelled,
    MomentumStarted,
    MomentumChanged,
    MomentumEnded,
    MomentumCancelled,
}

pub(crate) trait WheelPhaseEnrichment {
    /// Called synchronously from the wheel callback for this exact window.
    fn current(&self, window: &Window) -> Option<WheelPhaseDetail>;
}

pub(crate) fn resolve_wheel_phase(
    phase: TouchPhase,
    detail: Option<WheelPhaseDetail>,
) -> WheelPhase {
    match detail {
        Some(WheelPhaseDetail::GestureCancelled) => WheelPhase::GestureCancelled,
        Some(WheelPhaseDetail::MomentumStarted) => WheelPhase::MomentumStarted,
        Some(WheelPhaseDetail::MomentumChanged) => WheelPhase::MomentumChanged,
        Some(WheelPhaseDetail::MomentumEnded) => WheelPhase::MomentumEnded,
        Some(WheelPhaseDetail::MomentumCancelled) => WheelPhase::MomentumCancelled,
        None => match phase {
            TouchPhase::Started => WheelPhase::GestureStarted,
            TouchPhase::Moved => WheelPhase::GestureChanged,
            TouchPhase::Ended => WheelPhase::GestureEnded,
        },
    }
}

#[cfg(test)]
pub(crate) struct NoWheelPhaseEnrichment;

#[cfg(test)]
impl WheelPhaseEnrichment for NoWheelPhaseEnrichment {
    fn current(&self, _: &Window) -> Option<WheelPhaseDetail> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gpui_gesture_phases_remain_authoritative_without_missing_native_facts() {
        for (phase, expected) in [
            (TouchPhase::Started, WheelPhase::GestureStarted),
            (TouchPhase::Moved, WheelPhase::GestureChanged),
            (TouchPhase::Ended, WheelPhase::GestureEnded),
        ] {
            assert_eq!(resolve_wheel_phase(phase, None), expected);
        }
    }

    #[test]
    fn missing_cancellation_and_momentum_distinctions_enrich_every_gpui_phase() {
        for phase in [TouchPhase::Started, TouchPhase::Moved, TouchPhase::Ended] {
            for (detail, expected) in [
                (
                    WheelPhaseDetail::GestureCancelled,
                    WheelPhase::GestureCancelled,
                ),
                (
                    WheelPhaseDetail::MomentumStarted,
                    WheelPhase::MomentumStarted,
                ),
                (
                    WheelPhaseDetail::MomentumChanged,
                    WheelPhase::MomentumChanged,
                ),
                (WheelPhaseDetail::MomentumEnded, WheelPhase::MomentumEnded),
                (
                    WheelPhaseDetail::MomentumCancelled,
                    WheelPhase::MomentumCancelled,
                ),
            ] {
                assert_eq!(resolve_wheel_phase(phase, Some(detail)), expected);
            }
        }
    }
}
