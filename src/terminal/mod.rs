mod emulator;
mod session;
#[cfg(test)]
pub(crate) mod testing;
mod workspace_terminal_session_factory;

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
pub(crate) use workspace_terminal_session_factory::WorkspaceTerminalSessionFactory;
