mod accessibility;
pub(crate) mod attention;
mod emulator;
mod failure;
mod file_insertion;
pub(crate) mod geometry;
mod hyperlink;
pub(crate) mod identity;
mod key;
mod keyboard_protocol;
pub(crate) mod metadata;
mod native_services;
pub(crate) mod osc52;
mod paste;
mod selection;
mod session;
#[cfg(test)]
pub(crate) mod testing;
mod workspace_terminal_session_factory;

pub(crate) use accessibility::{
    AccessibilityGeometry, AccessibilityNotification, TerminalAccessibilityModel,
};
pub(crate) use attention::AttentionFacts;
#[cfg(test)]
pub(crate) use emulator::{CellSemanticSnapshot, ScrollbarSnapshot};
pub(crate) use emulator::{
    CellSnapshot, CursorPositionSnapshot, CursorShapeSnapshot, CursorSnapshot,
    PresentationGeneration, RowSnapshot, ScreenSnapshot, TerminalColor, TerminalColorsSnapshot,
    TerminalUnderlineSnapshot,
};
#[cfg(test)]
pub(crate) use failure::FailureClass;
pub(crate) use failure::{
    DiagnosticBundle, DiagnosticKeyEventKind, PaneTerminalState, TerminalFailure,
    UnhandledKeyDiagnostic,
};
pub(crate) use hyperlink::HyperlinkTarget;
pub(crate) use key::{
    InputModifiers, KeyAction, KeyInput, KeyInputError, OptionAsAltPolicy, PhysicalKey,
};
pub(crate) use native_services::{NativeContextActions, NativeInsertion};
#[cfg(test)]
pub(crate) use osc52::Osc52AuthorizationId;
pub(crate) use osc52::{
    Osc52Access, Osc52AuthorizationDecision, Osc52AuthorizationRequest, Osc52Target,
};
pub(crate) use paste::{PasteConfirmation, PasteDecision, PasteRequestOutcome, PasteResolution};
#[cfg(test)]
pub(crate) use paste::{PasteConfirmationId, PasteRisk};
pub(crate) use selection::SelectionCopy;
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
    SessionExit, ShiftSelectionPolicy, SurfacePosition, TerminalSessionFactory,
    TerminalSessionHandle, WheelInput, WheelPhase,
};
#[cfg(test)]
pub(crate) use session::{SessionError, StartedTerminalSession};
pub(crate) use workspace_terminal_session_factory::WorkspaceTerminalSessionFactory;
