//! PTY and VT ownership and presentation contracts for desktop and headless hosts.

pub use crate::appearance::color::Color;
pub use crate::appearance::resolution::{
    AppearanceGeneration, FontClass, FontStyle, ResolvedFontDescriptor, ResolvedTerminalAppearance,
    ResolvedTerminalTypography,
};
pub use crate::appearance::terminal_theme::{Appearance, CatalogError, TerminalColors, ThemeId};
pub use crate::domain::workspace_collection::{
    CurrentDirectory, RemoteDirectory, RemoteWorkspaceValueError, ValidatedLocalDirectory,
};
pub use crate::local_path::LocalPathSemantics;
pub use crate::platform::local_filesystem::{
    LocalChildDirectories, LocalDirectoryIdentity, LocalFilesystemAuthority, LocalFilesystemError,
    LocalIdentityObservation, LocalIdentitySource, LocalObjectIdentity, LocalObjectKind,
    NewDirectoryTarget, ValidatedLocalFile,
};
pub use crate::platform::native_pty::*;
pub use crate::platform::permission_access::SystemPermission;
pub use crate::platform::shell_integration::*;
pub use crate::platform::shell_launch::{
    PreparedShellLaunch, ShellLaunchFailure, ShellLaunchPlanner,
};
pub use crate::terminal::accessibility::{
    AccessibilityCellRef, AccessibilityCellView, AccessibilityGeometry, AccessibilityRowId,
    AccessibilityRowView, AccessibilityScreen, AccessibilitySelectionRequest,
    TerminalAccessibilityModel,
};
pub use crate::terminal::attention::AttentionEvent;
pub use crate::terminal::emulator::{
    ActiveScreenSnapshot, CellSemanticSnapshot, CellSnapshot, ContentDamageSnapshot,
    CursorPositionSnapshot, CursorShapeSnapshot, CursorSnapshot, PresentationGeneration,
    RowSnapshot, ScreenSizeSnapshot, ScreenSnapshot, ScrollbackMovement, ScrollbarSnapshot,
    SnapshotDamage, TerminalColor, TerminalColorsSnapshot, TerminalDefaultColorSource,
    TerminalUnderlineSnapshot, ViewportSnapshot,
};
pub use crate::terminal::failure::{FailureClass, FailureReason, Recoverability, TerminalFailure};
pub use crate::terminal::find::{
    FindDirection, FindHighlightSpan, FindQueryGeneration, TerminalFindSnapshot,
};
pub use crate::terminal::geometry::*;
pub use crate::terminal::graphics::{
    GraphicsSnapshot, ImageKey, ImagePlacementSnapshot, ImageSnapshot,
};
pub use crate::terminal::key::*;
pub use crate::terminal::metadata::{
    CommandMetadata, CommandState, DirectoryMetadata, DirectoryProvenance, LocalMachine,
    MetadataFreshness, ProgressMetadata, PromptOwner, PromptZone, RemoteMachine,
    RemoteTerminalMetadataContext, TerminalLocalFileCapabilities, TerminalMetadataContext,
    TerminalMetadataSnapshot, TerminalOrigin, TitleMetadata, TitleProvenance,
};
pub use crate::terminal::native_services::hyperlink::{HyperlinkKind, HyperlinkTarget};
pub use crate::terminal::native_services::osc52::{Osc52Operation, Osc52Target, Osc52Terminator};
pub use crate::terminal::native_services::paste::{
    PasteConfirmation, PasteConfirmationId, PasteDecision, PasteIntakeError, PastePayload,
    PasteRejection, PasteRequestOutcome, PasteResolution, PasteRisk,
};
pub use crate::terminal::native_services::selection::{
    SelectionCopy, SelectionCopyOptions, TrailingSpacePolicy,
};
pub use crate::terminal::permission_request::PermissionRequest;
pub use crate::terminal::pointer_input::*;
pub use crate::terminal::session::{
    AccessibilityDemandSender, AccessibilitySelectionSender, ClipboardCompletion, ClipboardRequest,
    InputMethodOutcome, InputMethodRefusal, LocalTerminalLaunchPlan, MAX_INPUT_METHOD_BYTES,
    NativeTerminalSessionFactory, SelectionCopyError, StartedTerminalSession,
    TerminalAppearanceUpdate, TerminalRuntime, TerminalSessionError, TerminalSessionEvent,
    TerminalSessionExit, TerminalSessionFactory, TerminalSessionFailure, TerminalSessionHandle,
    TerminalSubscription,
};
pub use crate::terminal::title::TitleGlyph;

/// Returns the native PTY adapter used by the desktop and headless hosts.
pub fn native_pty_factory() -> std::sync::Arc<dyn NativePtyAdapterFactory> {
    crate::platform::runtime_pty_factory()
}

/// Returns authority for explicit local file and directory operations on this host.
pub fn local_filesystem() -> LocalFilesystemAuthority {
    crate::platform::runtime_local_filesystem()
}

mod server;
#[cfg(all(test, feature = "desktop"))]
pub(crate) use server::screen_encoder;
#[cfg(all(test, feature = "desktop"))]
pub(crate) use server::testing as directory_testing;
pub use server::{DevelopmentPaneHost, HostCommands, Served, SharedWorkspaceHost};
