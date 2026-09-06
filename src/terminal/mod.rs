mod accessibility;
pub(crate) mod attention;
pub(crate) mod attention_notification;
pub(crate) mod attention_runtime;
#[cfg(test)]
mod conformance;
mod emulator;
mod failure;
pub(crate) mod secure_input;
pub(crate) mod wheel_phase;
#[cfg(test)]
pub(crate) use native_services::file_insertion;
mod find;
pub(crate) mod geometry;
mod graphics;
pub(crate) use native_services::hyperlink;
pub(crate) mod identity;
mod key;
mod key_input;
mod keyboard_protocol;
pub(crate) mod metadata;
pub(crate) mod native_services;
pub(crate) use native_services::osc52;
pub(crate) use native_services::paste;
mod runtime_observation;
pub(crate) use native_services::selection;
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
#[cfg(test)]
pub(crate) use key_input::assert_common_adapter_contract;
pub(crate) use key_input::{
    GpuiTerminalKeyInputAdapter, GpuiTerminalKeyInputAdapterFactory, KeyTranslation,
    TerminalKeyInputAdapter, TerminalKeyInputAdapterFactory, TerminalKeyInputEventKind,
    UnhandledKeyEvent,
};
pub(crate) use metadata::TerminalLocalFileCapabilities;
pub(crate) use native_services::{
    FilePreviewTarget, NativeContextActions, NativeInsertion, NativeServiceCapabilities,
    NativeServiceOrigin, NativeServiceStatus,
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
    AcceptanceSessionFailure, NativeTerminalSessionFactory, PointerButton, PointerInput,
    PointerPhase, SelectionCopyError, SessionEvent, SessionExit, ShiftSelectionPolicy,
    SurfacePosition, TerminalSessionFactory, TerminalSessionHandle, WheelInput, WheelPhase,
};
#[cfg(test)]
pub(crate) use session::{
    LocalTerminalLaunchPlan, RemoteTerminalLaunchPlan, SessionError, StartedTerminalSession,
    TerminalLaunchPlan,
};
pub(crate) use workspace_terminal_session_factory::{
    PreparedWorkspaceTerminalLaunch, RemoteChannelRevalidationError, RemoteChannelUnavailable,
    RemoteTerminalChannelProvider, WorkspaceChildLaunchValidation, WorkspaceTerminalSessionFactory,
};
