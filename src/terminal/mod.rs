mod accessibility;
pub(crate) mod attention;
#[cfg(test)]
mod conformance;
mod emulator;
mod failure;
mod file_insertion;
mod find;
pub(crate) mod geometry;
mod graphics;
mod hyperlink;
pub(crate) mod identity;
mod key;
mod keyboard_protocol;
pub(crate) mod metadata;
mod native_services;
pub(crate) mod osc52;
mod paste;
mod runtime_observation;
mod selection;
mod session;
#[cfg(test)]
pub(crate) mod testing;
mod workspace_terminal_session_factory;

#[cfg(test)]
pub(crate) use accessibility::{AccessibilityCell, AccessibilityLine};
pub(crate) use accessibility::{
    AccessibilityGeometry, AccessibilityNotification, AccessibilityNotifications,
    TerminalAccessibilityModel,
};
pub(crate) use attention::AttentionFacts;
pub(crate) use emulator::{
    ActiveScreenSnapshot, CellSnapshot, CursorPositionSnapshot, CursorShapeSnapshot,
    CursorSnapshot, PresentationGeneration, RowSnapshot, ScreenSnapshot, TerminalColor,
    TerminalColorsSnapshot, TerminalUnderlineSnapshot,
};
#[cfg(test)]
pub(crate) use emulator::{CellSemanticSnapshot, ScrollbarSnapshot};
pub(crate) use failure::{
    DiagnosticBundle, DiagnosticKeyEventKind, FailureClass, PaneTerminalState, Recoverability,
    TerminalFailure, UnhandledKeyDiagnostic,
};
pub(crate) use find::{
    FindDirection, FindHighlightSpan, FindQueryGeneration, TerminalFindSnapshot,
};
pub(crate) use graphics::{GraphicsSnapshot, ImageKey, ImagePlacementSnapshot, ImageSnapshot};
pub(crate) use hyperlink::HyperlinkTarget;
pub(crate) use key::{
    InputModifiers, KeyAction, KeyInput, KeyInputError, OptionAsAltPolicy, PhysicalKey,
};
pub(crate) use metadata::TerminalLocalFileCapabilities;
pub(crate) use native_services::{
    NativeContextActions, NativeInsertion, NativeServiceCapabilities, NativeServiceOrigin,
    NativeServiceStatus, QuickLookTarget,
};
#[cfg(test)]
pub(crate) use osc52::Osc52AuthorizationId;
pub(crate) use osc52::{
    Osc52Access, Osc52AuthorizationDecision, Osc52AuthorizationRequest, Osc52Target,
};
pub(crate) use paste::{
    MAX_PASTE_BYTES, PasteConfirmation, PasteDecision, PasteRequestOutcome, PasteResolution,
};
#[cfg(test)]
pub(crate) use paste::{PasteConfirmationId, PasteRisk};
#[cfg(test)]
pub(crate) use runtime_observation::RuntimeEventKind;
pub(crate) use runtime_observation::{
    RuntimeLifecycle, RuntimeObservation, RuntimeSample, RuntimeTransition, RuntimeVisibility,
};
pub(crate) use selection::SelectionCopy;
#[cfg(all(target_os = "macos", not(test)))]
pub(crate) use session::AccessibilitySelectionSender;
#[cfg_attr(
    not(test),
    expect(
        unused_imports,
        reason = "SessionFailure is part of the crate-visible SessionEvent interface"
    )
)]
pub(crate) use session::SessionFailure;
pub(crate) use session::{
    AcceptanceSessionFailure, LocalTerminalLaunchPlan, NativeTerminalSessionFactory, PointerButton,
    PointerInput, PointerPhase, RemoteTerminalLaunchPlan, SelectionCopyError, SessionEvent,
    SessionExit, ShiftSelectionPolicy, SurfacePosition, TerminalLaunchPlan, TerminalSessionFactory,
    TerminalSessionHandle, WheelInput, WheelPhase,
};
#[cfg(test)]
pub(crate) use session::{SessionError, StartedTerminalSession};
pub(crate) use workspace_terminal_session_factory::{
    WorkspaceChildLaunchValidation, WorkspaceTerminalSessionFactory,
};
