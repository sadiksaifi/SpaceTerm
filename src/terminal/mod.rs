mod emulator;
mod geometry;
mod session;
#[cfg(test)]
pub(crate) mod testing;
mod workspace_terminal_session_factory;

#[cfg(test)]
pub(crate) use emulator::ScrollbarSnapshot;
pub(crate) use emulator::{
    CellSnapshot, CursorPositionSnapshot, CursorSnapshot, RowSnapshot, ScreenSnapshot,
};
#[cfg_attr(
    not(test),
    expect(
        unused_imports,
        reason = "SessionFailure is part of the crate-visible SessionEvent interface"
    )
)]
pub(crate) use session::SessionFailure;
pub(crate) use session::{
    GridSize, InputModifiers, KeyCode, KeyInput, NativeTerminalSessionFactory, PointerButton,
    PointerInput, PointerPhase, SessionEvent, SurfacePosition, TerminalSessionFactory,
    TerminalSessionHandle, WheelInput,
};
#[cfg(test)]
pub(crate) use session::{SessionError, StartedTerminalSession};
pub(crate) use workspace_terminal_session_factory::WorkspaceTerminalSessionFactory;
