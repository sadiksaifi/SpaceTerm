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
pub(crate) use native_services::selection;
mod pointer_input;
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
#[cfg(test)]
pub(crate) use failure::FailureClass;
pub(crate) use failure::{
    DiagnosticBundle, DiagnosticKeyEventKind, PaneTerminalState, TerminalFailure,
    UnhandledKeyDiagnostic,
};
pub(crate) use find::{
    FindDirection, FindHighlightSpan, FindQueryGeneration, TerminalFindSnapshot,
};
pub(crate) use graphics::{GraphicsSnapshot, ImageKey, ImagePlacementSnapshot, ImageSnapshot};
pub(crate) use hyperlink::HyperlinkTarget;
pub(crate) use key::{
    InputModifiers, KeyAction, KeyInput, KeyInputError, OptionAsAltPolicy, PhysicalKey,
};
#[cfg(all(test, feature = "macos-native-tests"))]
pub(crate) use key_input::assert_common_adapter_contract;
pub(crate) use key_input::{
    GpuiTerminalKeyInputAdapter, GpuiTerminalKeyInputAdapterFactory, KeyTranslation,
    TerminalKeyInputAdapter, TerminalKeyInputAdapterFactory, TerminalKeyInputEventKind,
    UnhandledKeyEvent,
};
pub(crate) use metadata::TerminalLocalFileCapabilities;
pub(crate) use native_services::{
    FilePreviewTarget, NativeContextActions, NativeServiceCapabilities, NativeServiceOrigin,
    NativeServiceStatus, PastePayload,
};
pub(crate) use paste::{
    MAX_PASTE_BYTES, PasteConfirmation, PasteDecision, PasteRequestOutcome, PasteResolution,
};
#[cfg(test)]
pub(crate) use paste::{PasteConfirmationId, PasteRisk};
pub(crate) use selection::SelectionCopy;
#[cfg(test)]
pub(crate) use session::SessionFailure;
pub(crate) use session::{AccessibilityDemandSender, AccessibilitySelectionSender};
#[cfg(test)]
pub(crate) use session::{
    LocalTerminalLaunchPlan, RemoteTerminalLaunchPlan, SessionError, SessionExit,
    TerminalLaunchPlan,
};
pub(crate) use session::{
    NativeTerminalSessionFactory, SelectionCopyError, SessionDirectorySnapshot, SessionEvent,
    StartedTerminalSession, TerminalSessionFactory, TerminalSessionHandle,
};
pub(crate) use workspace_terminal_session_factory::{
    PreparedWorkspaceTerminalLaunch, RemoteChannelRevalidationError, RemoteChannelUnavailable,
    RemoteTerminalChannelProvider, WorkspaceTerminalSessionFactory,
};

pub(crate) use pointer_input::{
    PointerButton, PointerInput, PointerPhase, ShiftSelectionPolicy, SurfacePosition, WheelInput,
    WheelPhase,
};
