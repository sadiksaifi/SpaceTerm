#![cfg_attr(
    not(feature = "desktop"),
    allow(
        unused_imports,
        reason = "desktop consumers use the complete terminal reexport set"
    )
)]

pub(crate) mod accessibility;
#[cfg_attr(
    not(feature = "desktop"),
    allow(
        dead_code,
        reason = "desktop presentation policy shares runtime types and tests"
    )
)]
pub(crate) mod attention;
#[cfg(feature = "desktop")]
pub(crate) mod attention_notification;
#[cfg(feature = "desktop")]
pub(crate) mod attention_runtime;
#[cfg(all(test, feature = "desktop"))]
pub(crate) mod conformance;
pub(crate) mod emulator;
#[cfg_attr(
    not(feature = "desktop"),
    allow(
        dead_code,
        reason = "desktop presentation policy shares runtime types and tests"
    )
)]
pub(crate) mod failure;
#[cfg_attr(
    not(feature = "desktop"),
    allow(
        dead_code,
        reason = "desktop presentation policy shares runtime types and tests"
    )
)]
pub(crate) mod secure_input;
#[cfg(feature = "desktop")]
pub(crate) mod wheel_phase;
#[cfg(test)]
pub(crate) use native_services::file_insertion;
pub(crate) mod find;
pub(crate) mod geometry;
pub(crate) mod graphics;
pub(crate) use native_services::hyperlink;
pub(crate) mod identity;
pub(crate) mod key;
#[cfg(feature = "desktop")]
pub(crate) mod key_input;
pub(crate) mod keyboard_protocol;
pub(crate) mod metadata;
#[cfg_attr(
    not(feature = "desktop"),
    allow(
        dead_code,
        reason = "desktop presentation policy shares runtime types and tests"
    )
)]
pub(crate) mod native_services;
#[cfg_attr(not(feature = "desktop"), allow(dead_code))]
pub(crate) mod server_metadata;
pub(crate) mod title;
pub(crate) use native_services::osc52;
pub(crate) use native_services::paste;
pub(crate) mod permission_request;
pub(crate) use native_services::selection;
pub(crate) mod pointer_input;
pub(crate) mod session;
#[cfg(test)]
pub(crate) mod testing;
#[cfg(feature = "desktop")]
pub(crate) mod workspace_terminal_session_factory;

#[cfg(test)]
pub(crate) use accessibility::{
    AccessibilityCell, AccessibilityCellRef, AccessibilityLine, AccessibilityRowUpdate,
    AccessibilityScreen, AccessibilitySelectionRefs, AccessibilityUpdate,
    TerminalAccessibilityState,
};
pub(crate) use accessibility::{
    AccessibilityGeometry, AccessibilityNotification, AccessibilityNotifications,
    AccessibilityRowId, AccessibilityRowView, TerminalAccessibilityModel,
};
pub(crate) use attention::AttentionFacts;
#[cfg(test)]
pub(crate) use emulator::ScrollbarSnapshot;
pub(crate) use emulator::{
    ActiveScreenSnapshot, CellSemanticSnapshot, CellSnapshot, CursorPositionSnapshot,
    CursorShapeSnapshot, CursorSnapshot, PresentationGeneration, RowSnapshot, ScreenSnapshot,
    TerminalColor, TerminalColorsSnapshot, TerminalDefaultColorSource, TerminalUnderlineSnapshot,
};
#[cfg(test)]
pub(crate) use failure::FailureClass;
pub(crate) use failure::{
    DiagnosticBundle, DiagnosticKeyEventKind, PaneTerminalState, TerminalFailure,
    UnhandledKeyDiagnostic,
};
pub(crate) use find::{FindHighlightSpan, FindQueryGeneration, TerminalFindSnapshot};
pub(crate) use graphics::{GraphicsSnapshot, ImageKey, ImagePlacementSnapshot, ImageSnapshot};
pub(crate) use hyperlink::HyperlinkTarget;
pub(crate) use key::{InputModifiers, KeyAction, KeyInput, OptionAsAltPolicy, PhysicalKey};
#[cfg(all(test, feature = "native-tests"))]
#[cfg(feature = "desktop")]
pub(crate) use key_input::assert_common_adapter_contract;
#[cfg(feature = "desktop")]
pub(crate) use key_input::{
    GpuiTerminalKeyInputAdapter, GpuiTerminalKeyInputAdapterFactory, KeyTranslation,
    TerminalKeyInputAdapter, TerminalKeyInputAdapterFactory, TerminalKeyInputEventKind,
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
pub(crate) use session::AccessibilityDemandSender;
#[cfg(feature = "desktop")]
pub(crate) use session::AccessibilitySelectionSender;
#[cfg(all(test, feature = "desktop"))]
pub(crate) use session::TerminalSessionFactory;
#[cfg(test)]
pub(crate) use session::TerminalSessionFailure;
pub(crate) use session::{SelectionCopyError, TerminalAppearanceUpdate, TerminalSessionEvent};
#[cfg(test)]
pub(crate) use session::{
    TerminalSessionError, TerminalSessionExit, test_terminal_appearance_update,
};
#[cfg(feature = "desktop")]
pub(crate) use workspace_terminal_session_factory::WorkspaceTerminalSessionFactory;

pub(crate) use pointer_input::{
    PointerButton, PointerInput, PointerPhase, ShiftSelectionPolicy, SurfacePosition, WheelInput,
    WheelPhase,
};

#[cfg(feature = "desktop")]
pub(crate) mod changes;
#[cfg(feature = "desktop")]
pub(crate) mod server_graphics;
#[cfg(feature = "desktop")]
pub(crate) mod server_session;
#[cfg(feature = "desktop")]
pub(crate) mod workspace_client;

#[cfg(feature = "desktop")]
pub(crate) use emulator::ScrollbackMovement;
#[cfg(feature = "desktop")]
pub(crate) use find::FindDirection;
#[cfg(feature = "desktop")]
pub(crate) use session::{InteractiveInput, StartedTerminalSession, TerminalSessionHandle};
