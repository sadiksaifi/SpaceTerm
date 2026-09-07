use super::{InputModifiers, PresentationGeneration};

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct SurfacePosition {
    pub(crate) x: f32,
    pub(crate) y: f32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PointerButton {
    Left,
    Middle,
    Right,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PointerPhase {
    Press,
    Motion,
    Release,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ShiftSelectionPolicy {
    OverrideApplicationMouse,
    ReportToApplication,
}

impl ShiftSelectionPolicy {
    pub(crate) const fn from_selection_override(enabled: bool) -> Self {
        if enabled {
            Self::OverrideApplicationMouse
        } else {
            Self::ReportToApplication
        }
    }
}

impl Default for ShiftSelectionPolicy {
    fn default() -> Self {
        Self::from_selection_override(true)
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct PointerInput {
    pub(crate) generation: PresentationGeneration,
    pub(crate) phase: PointerPhase,
    pub(crate) button: Option<PointerButton>,
    pub(crate) position: SurfacePosition,
    pub(crate) modifiers: InputModifiers,
    pub(crate) shift_selection: ShiftSelectionPolicy,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct WheelInput {
    pub(crate) generation: PresentationGeneration,
    pub(crate) horizontal_steps: i32,
    pub(crate) vertical_steps: i32,
    pub(crate) phase: WheelPhase,
    pub(crate) position: SurfacePosition,
    pub(crate) modifiers: InputModifiers,
    pub(crate) shift_selection: ShiftSelectionPolicy,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum WheelPhase {
    GestureStarted,
    #[default]
    GestureChanged,
    GestureEnded,
    GestureCancelled,
    MomentumStarted,
    MomentumChanged,
    MomentumEnded,
    MomentumCancelled,
}
