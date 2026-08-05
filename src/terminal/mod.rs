mod emulator;
pub(crate) mod geometry;
mod key;
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
#[cfg_attr(
    not(test),
    expect(
        unused_imports,
        reason = "SessionExit is part of the crate-visible SessionEvent interface"
    )
)]
pub(crate) use session::{
    NativeTerminalSessionFactory, PointerButton, PointerInput, PointerPhase, SessionEvent,
    SessionExit, SurfacePosition, TerminalSessionFactory, TerminalSessionHandle, WheelInput,
};
pub(crate) use key::{InputModifiers, KeyAction, KeyInput, PhysicalKey};
#[cfg(test)]
pub(crate) use session::{SessionError, StartedTerminalSession};
pub(crate) use workspace_terminal_session_factory::WorkspaceTerminalSessionFactory;
