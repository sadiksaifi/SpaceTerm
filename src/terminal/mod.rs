mod emulator;
mod session;

#[cfg(test)]
pub(crate) use emulator::ScrollbarSnapshot;
pub(crate) use emulator::{CellSnapshot, RowSnapshot, ScreenSnapshot};
pub(crate) use session::{
    GridSize, InputModifiers, KeyCode, KeyInput, NativeTerminalSessionFactory, PointerButton,
    PointerInput, PointerPhase, SessionEvent, SurfacePosition, TerminalSessionFactory,
    TerminalSessionHandle, WheelInput,
};
#[cfg(test)]
pub(crate) use session::{SessionError, StartedTerminalSession};
