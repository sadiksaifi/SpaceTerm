mod emulator;
mod session;

pub(crate) use emulator::{CellSnapshot, RowSnapshot, ScreenSnapshot};
#[allow(
    unused_imports,
    reason = "worker-side interaction contract is re-exported before UI input wiring"
)]
pub(crate) use session::{
    GridSize, InputModifiers, PointerButton, PointerInput, PointerPhase, SessionEvent,
    SurfacePosition, TerminalSession, WheelInput,
};
