use super::{InputModifiers, PresentationGeneration};

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct SurfacePosition {
    pub x: f32,
    pub y: f32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PointerButton {
    Left,
    Middle,
    Right,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PointerPhase {
    Press,
    Motion,
    Release,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ShiftSelectionPolicy {
    OverrideApplicationMouse,
    ReportToApplication,
}

impl ShiftSelectionPolicy {
    pub const fn from_selection_override(enabled: bool) -> Self {
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
pub struct PointerInput {
    pub generation: PresentationGeneration,
    pub phase: PointerPhase,
    pub button: Option<PointerButton>,
    pub position: SurfacePosition,
    pub modifiers: InputModifiers,
    pub shift_selection: ShiftSelectionPolicy,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WheelInput {
    pub generation: PresentationGeneration,
    pub horizontal_steps: i32,
    pub vertical_steps: i32,
    pub phase: WheelPhase,
    pub position: SurfacePosition,
    pub modifiers: InputModifiers,
    pub shift_selection: ShiftSelectionPolicy,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum WheelPhase {
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
