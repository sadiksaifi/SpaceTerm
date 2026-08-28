//! Window-modal desktop controls backed by one Operating-System Window-owned mechanism.
//!
//! [`Alert`], [`Dialog`], and [`ProgressDialog`] validate caller-owned typed configuration and
//! compile it into one private presentation model. Each Operating-System Window may show one modal
//! at a time. Up to eight additional requests wait in a bounded FIFO queue, and monotonic
//! [`ModalPresentationId`] values prevent retained completion and update handles from affecting a
//! successor. Each semantic facade can intentionally replace the visible presentation in one
//! owner transition. Replacement settles the predecessor before opening the successor, preserves
//! the existing FIFO behind it, and never exposes an interactive underlay gap. Queued
//! presentations inherit continuous Menu, Command Palette, Tooltip, keyboard, pointer, and
//! Terminal Input Focus blocking from the active modal.
//!
//! The shared renderer owns the viewport scrim, compact surface, adaptive action area, scrollable
//! body, focus scope, and key context. Focus enters according to [`ModalDesktopPolicy`], focused
//! controls are revealed inside the independently scrolling body and footer, the complete
//! current-frame GPUI tab-stop order remains contained in both directions, disabled or removed
//! targets are repaired on the next frame, and
//! restoration is attempted only for a live predecessor or explicit successor that has not been
//! superseded by newer focus ownership. Focused children handle Return and Escape first. This lets
//! an editor decline Return before an explicit default action runs and lets the first Escape cancel
//! input-method composition before a later Escape reaches the modal Cancel path.
//!
//! Caller action order is logical input order, not result identity or physical placement.
//! [`ModalActionRole`], [`ModalActionIntent`], [`ModalActionEmphasis`], enabled state, explicit
//! default state, typed identity, and debug identity remain independent. The installed desktop
//! policy selects locale direction, physical action placement, focus entry, layout axis, and
//! deadline limits. Every Operating-System Window modal root consumes that installed direction;
//! individual modal call sites do not select it. The application separately selects platform key
//! equivalents through [`ModalKeybindingProfile`] and owns and explicitly installs the immutable
//! policy and aggregate [`ModalTheme`].
//!
//! Dialog action callbacks run after the private reducer releases its GPUI entity update. A
//! [`DialogCloseDecision::Pending`] result disables duplicate primary completion and gives the
//! caller an opaque [`DialogPendingCompletion`] tied to that presentation and close attempt. The
//! independently retained [`DialogCompletion`] completes an active or queued Dialog with a typed
//! programmatic outcome and optional guarded successor focus, without synthesizing an action or
//! borrowing pending-action authority. One nested safe Cancel attempt may coexist without
//! replacing the primary authority. Denial preserves
//! any still-live counterpart, and the first allowed terminal decision wins. All terminal callbacks
//! and public close lifecycle transitions are delivered at most once, including caller
//! owner removal, Operating-System Window removal, replacement, queued dismissal, and reentrant
//! callbacks. Reducer state transitions and control disarming remain synchronous, while an
//! owner-owned effect pump defers callbacks until the GPUI update that invoked the public operation
//! has unwound. Reentrant effects and queue advancement retain their deterministic order.
//!
//! Progress updates are caller-driven and generation-checked. Determinate progress reaching `1.0`
//! does not close a [`ProgressDialog`]; the owner must complete, fail, or dismiss it. Immutable
//! typed cancellation capability is retained separately from mutable availability, and active or
//! queued programmatic-only presentations reject availability updates. Cancellation may allow,
//! deny, or become pending, and delayed completion retains its original activation source.
//! Programmatic-only mode requires a nonzero deadline no longer than the installed desktop
//! policy's private bound, and expiry produces a typed terminal outcome.
//!
//! # GPUI 0.2.2 accessibility audit
//!
//! Pinned GPUI 0.2.2 has no general native accessibility-node API for custom GPUI elements. These
//! controls therefore cannot publish native Alert, Dialog, or progress roles; accessible
//! title-description relationships; default, Cancel, enabled, or destructive action state; modal
//! state; progress values or indeterminate state; live status announcements; accessibility focus;
//! or exclusion of an arbitrary underlay from native accessibility traversal. The implementation
//! preserves those facts in private logical semantic snapshots and exposes stable debug selectors
//! for automated behavior tests, but neither is native accessibility evidence. This module makes
//! no VoiceOver, Narrator, or Orca conformance claim. Accessibility-sensitive production workflows
//! must remain on native system prompts until native accessibility-tree support exists and is
//! verified. SpaceTerm's existing `Window::prompt` call sites intentionally remain native.
//!
//! # Example
//!
//! ```
//! use spaceterm_ui::{
//!     Alert, ModalAction, ModalActionRole, ModalDesktopPolicy, ModalId,
//! };
//!
//! #[derive(Clone, Debug, Eq, PartialEq)]
//! enum Decision {
//!     Save,
//!     Cancel,
//! }
//!
//! let alert = Alert::new(
//!     ModalId::new("save-alert"),
//!     "Save changes?",
//!     "Save Changes",
//!     "Choose whether to save the current changes.",
//!     vec![
//!         ModalAction::new(
//!             Decision::Save,
//!             "Save",
//!             ModalActionRole::Affirmative,
//!             "save",
//!         )
//!         .default_action(true),
//!         ModalAction::new(
//!             Decision::Cancel,
//!             "Cancel",
//!             ModalActionRole::Cancel,
//!             "cancel",
//!         ),
//!     ],
//! );
//!
//! assert_eq!(alert.validate(&ModalDesktopPolicy::mac_os()), Ok(()));
//! ```

mod alert;
mod core;
mod dialog;
mod policy;
mod progress_dialog;
mod render;

use std::{error::Error, fmt, time::Duration};

use gpui::{Global, Pixels, Rgba, SharedString, Size, px, size};

pub use alert::{
    Alert, AlertAccessory, AlertIntent, AlertOutcome, AlertSuppression,
    MAX_ALERT_DETAIL_CHARACTERS, MAX_ALERT_MESSAGE_CHARACTERS,
};
pub use core::{
    DialogCompletion, DialogPendingCompletion, ModalPresentationHandle,
    ProgressCancellationCompletion, ProgressDialogHandle,
};
pub use dialog::{
    Dialog, DialogActionRequest, DialogCloseDecision, DialogInitialFocus, DialogOutcome, DialogSize,
};
use policy::{ActionAxis, ModalInitialFocus};
pub use policy::{ModalDesktopPolicy, TextDirection, install_modal_policy};
pub use progress_dialog::{
    DeterminateProgress, MAX_PROGRESS_DETAIL_CHARACTERS, MAX_PROGRESS_STATUS_CHARACTERS,
    ProgressCancelDecision, ProgressCancellation, ProgressDialog, ProgressDialogOutcome,
    ProgressDialogUpdate, ProgressState, ProgressValueError,
};
pub use render::{ModalKeybindingProfile, ModalLayer, install_modal_keybindings};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ModalParentToken {
    pub(crate) window_id: gpui::WindowId,
    pub(crate) presentation: ModalPresentationId,
}

pub(crate) fn current_modal_parent(
    window: &gpui::Window,
    cx: &gpui::App,
) -> Option<ModalParentToken> {
    core::current_modal_parent(window, cx)
}

pub(crate) fn focused_modal_parent(
    window: &gpui::Window,
    cx: &gpui::App,
) -> Option<ModalParentToken> {
    core::focused_modal_parent(window, cx)
}

pub(crate) fn focus_allows_transient_resume(window: &gpui::Window, cx: &gpui::App) -> bool {
    core::focus_allows_transient_resume(window, cx)
}

pub(super) fn init(cx: &mut gpui::App) {
    core::init(cx);
    render::init(cx);
}

#[derive(Clone, Copy)]
pub(super) enum ModalPresentationOperation {
    Present,
    ReplaceActive,
}

impl ModalPresentationOperation {
    fn apply<T: 'static>(
        self,
        request: core::PreparedModalRequest,
        window: &gpui::Window,
        cx: &mut gpui::Context<T>,
    ) -> Result<ModalPresentationHandle, ModalPresentationError> {
        match self {
            Self::Present => core::present(request, window, cx),
            Self::ReplaceActive => core::replace_active(request, window, cx),
        }
    }
}

/// Stable caller-owned identity for one semantic modal.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ModalId(SharedString);

impl ModalId {
    /// Creates a stable identity. Empty identities are rejected during preparation.
    pub fn new(value: impl Into<SharedString>) -> Self {
        Self(value.into())
    }

    /// Returns the identity text used for validation, stable element identity, and diagnostics.
    pub fn as_str(&self) -> &str {
        self.0.as_ref()
    }
}

/// Opaque monotonic identity for one presentation of a modal configuration.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ModalPresentationId(u64);

impl ModalPresentationId {
    pub(super) const fn from_generation(generation: u64) -> Self {
        Self(generation)
    }

    /// Returns a content-free diagnostic generation.
    pub const fn value(self) -> u64 {
        self.0
    }
}

/// Semantic placement of an action, independent of destructive intent and visual emphasis.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ModalActionRole {
    /// Applies or acknowledges the requested operation.
    Affirmative,
    /// Safely abandons or closes the requested operation.
    Cancel,
    /// Performs a secondary operation without being the default decision.
    Auxiliary,
    /// Opens contextual help outside the decision action area.
    Help,
}

/// Consequence of an action, independent of its role or physical position.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ModalActionIntent {
    /// The action is reversible or otherwise non-destructive.
    #[default]
    Ordinary,
    /// The action can irreversibly remove or overwrite caller-owned state.
    Destructive,
}

/// Visual weight of an action, independent of its key equivalent and consequence.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ModalActionEmphasis {
    /// Ordinary desktop action presentation.
    #[default]
    Standard,
    /// The caller's visually prominent action for this decision surface.
    Prominent,
}

/// A typed semantic modal action.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ModalAction<A> {
    pub(super) id: A,
    pub(super) label: SharedString,
    pub(super) role: ModalActionRole,
    pub(super) intent: ModalActionIntent,
    pub(super) emphasis: ModalActionEmphasis,
    pub(super) enabled: bool,
    pub(super) is_default: bool,
    pub(super) debug_identity: SharedString,
}

impl<A> ModalAction<A> {
    /// Creates an enabled ordinary action with no default-key designation.
    pub fn new(
        id: A,
        label: impl Into<SharedString>,
        role: ModalActionRole,
        debug_identity: impl Into<SharedString>,
    ) -> Self {
        Self {
            id,
            label: label.into(),
            role,
            intent: ModalActionIntent::Ordinary,
            emphasis: ModalActionEmphasis::Standard,
            enabled: true,
            is_default: false,
            debug_identity: debug_identity.into(),
        }
    }

    /// Marks whether this action can currently be activated.
    pub fn enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }

    /// Assigns an ordinary or destructive consequence without changing the action role.
    pub fn with_intent(mut self, intent: ModalActionIntent) -> Self {
        self.intent = intent;
        self
    }

    /// Selects visual prominence without changing role, intent, or key behavior.
    pub fn with_emphasis(mut self, emphasis: ModalActionEmphasis) -> Self {
        self.emphasis = emphasis;
        self
    }

    /// Explicitly designates this action as the Return-key default.
    pub fn default_action(mut self, is_default: bool) -> Self {
        self.is_default = is_default;
        self
    }

    /// Returns the caller-owned typed identity.
    pub fn id(&self) -> &A {
        &self.id
    }

    /// Returns the localized visible label.
    pub fn label(&self) -> &str {
        self.label.as_ref()
    }

    /// Returns the semantic action role.
    pub const fn role(&self) -> ModalActionRole {
        self.role
    }

    /// Returns the consequence of activating the action.
    pub const fn intent(&self) -> ModalActionIntent {
        self.intent
    }

    /// Returns the action's visual weight, independent of default-key designation.
    pub const fn emphasis(&self) -> ModalActionEmphasis {
        self.emphasis
    }

    /// Returns whether this action can currently be activated.
    pub const fn is_enabled(&self) -> bool {
        self.enabled
    }

    /// Returns whether Return may activate this action after a focused child declines it.
    pub const fn is_default(&self) -> bool {
        self.is_default
    }

    /// Returns the stable content-free debug and test identity.
    pub fn debug_identity(&self) -> &str {
        self.debug_identity.as_ref()
    }
}

/// Content-free source of a modal action activation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ModalActivationSource {
    /// A primary pointer press and matching release activated the action.
    Pointer,
    /// Space activated the focused reusable button.
    Space,
    /// Return reached the modal after the focused child declined it.
    Return,
    /// Escape reached the modal after the focused child declined it.
    Escape,
    /// Command-Period reached the modal after the focused child declined it.
    CommandPeriod,
    /// The owner explicitly requested the semantic action.
    Programmatic,
}

/// One public lifecycle transition for a modal presentation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ModalLifecycleEvent {
    /// The modal became the active window-owned presentation.
    Opened(ModalPresentationId),
    /// A semantic action was requested and duplicate activation is blocked.
    ActionRequested(ModalPresentationId),
    /// Caller-owned asynchronous work is pending.
    Pending(ModalPresentationId),
    /// The presentation began an authoritative close.
    Closing(ModalPresentationId),
    /// The presentation completed exactly once.
    Closed(ModalPresentationId, ModalCloseReason),
}

/// Content-free reason an authoritative modal presentation closed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ModalCloseReason {
    /// A semantic action completed the presentation.
    Action,
    /// The enabled safe Cancel path completed the presentation.
    Cancelled,
    /// The owner explicitly completed or dismissed the presentation.
    Programmatic,
    /// A bounded programmatic-only deadline expired.
    DeadlineExpired,
    /// The owning Operating-System Window or caller owner was removed.
    OwnerRemoved,
    /// A newer authoritative presentation replaced this one.
    Replaced,
}

/// A bounded logical text field reported by validation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ModalTextField {
    /// Stable modal identity.
    ModalId,
    /// Logical accessibility title.
    AccessibilityTitle,
    /// Visible title.
    VisibleTitle,
    /// Alert message.
    AlertMessage,
    /// Optional concise detail.
    Detail,
    /// Progress status.
    ProgressStatus,
    /// Stable action debug identity.
    ActionDebugIdentity,
    /// Visible action label.
    ActionLabel,
}

/// Deterministic typed error produced while preparing caller-owned modal configuration.
///
/// Validation never repairs or reorders ambiguous caller semantics. Action indexes in errors refer
/// to caller logical order, not policy-selected physical order.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ModalValidationError {
    /// A mandatory logical field was empty.
    EmptyText(ModalTextField),
    /// A bounded logical field exceeded its character limit.
    TextTooLong {
        /// The field that exceeded its bound.
        field: ModalTextField,
        /// Maximum accepted Unicode scalar count.
        maximum: usize,
    },
    /// Two actions carried the same caller-owned identity.
    DuplicateActionIdentity {
        /// Index of the first action in caller order.
        first: usize,
        /// Index of the duplicate action in caller order.
        duplicate: usize,
    },
    /// Two actions carried the same stable debug identity.
    DuplicateActionDebugIdentity {
        /// Index of the first action in caller order.
        first: usize,
        /// Index of the duplicate action in caller order.
        duplicate: usize,
    },
    /// More than one action was explicitly designated as default.
    MultipleDefaultActions {
        /// Index of the first default action in caller order.
        first: usize,
        /// Index of the duplicate default action in caller order.
        duplicate: usize,
    },
    /// More than one action had the Cancel role.
    MultipleCancelActions {
        /// Index of the first Cancel action in caller order.
        first: usize,
        /// Index of the duplicate Cancel action in caller order.
        duplicate: usize,
    },
    /// A disabled action was designated as default.
    DisabledDefaultAction {
        /// Index of the disabled default action in caller order.
        index: usize,
    },
    /// A Cancel-role action was marked destructive instead of representing safe dismissal.
    DestructiveCancelAction {
        /// Index of the destructive Cancel action in caller order.
        index: usize,
    },
    /// Cancel was designated as the default action.
    CancelDefaultAction {
        /// Index of the Cancel default action in caller order.
        index: usize,
    },
    /// Help was placed in an Alert's decision action collection.
    AlertHelpMustBeSeparate {
        /// Index of the misplaced Help action in caller order.
        index: usize,
    },
    /// A separately supplied Alert help action did not have the Help role.
    InvalidHelpActionRole,
    /// Help was destructive or designated as the default decision.
    InvalidHelpAction {
        /// Index of the invalid Help action in combined caller order.
        index: usize,
    },
    /// A Dialog's action initial-focus identity was absent or currently disabled.
    InvalidDialogInitialFocus,
    /// The Alert decision collection was outside the one-to-three bound.
    AlertDecisionCount {
        /// Number of supplied Alert decision actions.
        count: usize,
    },
    /// The facade has no enabled safe dismissal path required by its semantic contract.
    MissingSafeDismissal,
    /// A destructive decision had no enabled Cancel action.
    UnsafeDestructiveDecision,
    /// Progress cancellation was not represented by a Cancel-role action.
    InvalidProgressCancelAction,
    /// A programmatic-only progress deadline was zero or exceeded desktop policy.
    InvalidProgrammaticOnlyDeadline {
        /// Supplied deadline.
        deadline: Duration,
        /// Inclusive installed-policy maximum.
        maximum: Duration,
    },
}

impl fmt::Display for ModalValidationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "invalid modal configuration: {self:?}")
    }
}

impl Error for ModalValidationError {}

/// Typed error returned before a modal receives an Operating-System Window presentation.
///
/// No terminal callback is delivered for a request rejected before admission to the active slot or
/// bounded eight-entry waiting FIFO.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ModalPresentationError {
    /// Typed configuration failed deterministic desktop validation.
    InvalidConfiguration(ModalValidationError),
    /// The per-window bounded FIFO already contains eight requests.
    QueueFull,
    /// No logical desktop policy has been explicitly installed.
    DesktopPolicyNotInstalled,
    /// The requested Operating-System Window no longer exists.
    WindowUnavailable,
    /// Replacement was requested while the Operating-System Window had no visible modal.
    NoActivePresentation,
}

impl fmt::Display for ModalPresentationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "modal presentation failed: {self:?}")
    }
}

impl Error for ModalPresentationError {}

impl From<ModalValidationError> for ModalPresentationError {
    fn from(error: ModalValidationError) -> Self {
        Self::InvalidConfiguration(error)
    }
}

/// Typed proof that retained completion or update authority belongs to an older presentation.
///
/// The attempted and current generation values are content-free and safe for diagnostics.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ModalStaleGenerationError {
    attempted: ModalPresentationId,
    current: Option<ModalPresentationId>,
}

impl ModalStaleGenerationError {
    pub(super) const fn new(
        attempted: ModalPresentationId,
        current: Option<ModalPresentationId>,
    ) -> Self {
        Self { attempted, current }
    }

    /// Returns the rejected presentation identity.
    pub const fn attempted(self) -> ModalPresentationId {
        self.attempted
    }

    /// Returns the current presentation identity when one remains active.
    pub const fn current(self) -> Option<ModalPresentationId> {
        self.current
    }
}

impl fmt::Display for ModalStaleGenerationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "stale modal presentation generation")
    }
}

impl Error for ModalStaleGenerationError {}

/// Typed error returned by a retained modal update handle.
///
/// It distinguishes invalid bounded content, unavailable cancellation capability, stale
/// presentation identity, stale update ordering, authoritative closure, and caller or
/// Operating-System Window removal.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ModalUpdateError {
    /// The update belongs to an older or replaced presentation.
    Stale(ModalStaleGenerationError),
    /// A retained update generation was already superseded by a newer accepted update.
    StaleUpdate {
        /// Rejected update generation.
        attempted: u64,
        /// Current accepted update generation.
        current: u64,
    },
    /// A bounded update field failed validation.
    Invalid(ModalValidationError),
    /// The presentation was created without typed cancellation capability.
    CancellationNotSupported,
    /// The presentation has already reached a terminal outcome.
    Closed,
    /// The caller owner or its Operating-System Window was removed.
    OwnerRemoved,
}

impl fmt::Display for ModalUpdateError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "modal update rejected: {self:?}")
    }
}

impl Error for ModalUpdateError {}

/// Typed error returned for duplicate, closed, removed, or stale terminal authority.
///
/// The shared completion flag makes terminal delivery exactly once across every handle clone and
/// reentrant callback path.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ModalTerminalOutcomeError {
    /// The presentation already delivered its one terminal outcome.
    AlreadyDelivered,
    /// The presentation closed before this operation could complete.
    Closed,
    /// The caller owner or its Operating-System Window was removed.
    OwnerRemoved,
    /// The completion belongs to an older or replaced presentation.
    Stale(ModalStaleGenerationError),
}

impl fmt::Display for ModalTerminalOutcomeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "modal terminal outcome rejected: {self:?}")
    }
}

impl Error for ModalTerminalOutcomeError {}

/// Typed error returned by an opaque presentation dismissal handle.
///
/// Dismissal is generation and Operating-System Window scoped, including while a request waits in
/// the FIFO, so stale handles cannot affect a successor.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ModalDismissalError {
    /// The presentation already closed.
    Closed,
    /// The caller owner or its Operating-System Window was removed.
    OwnerRemoved,
    /// The handle belongs to an older or replaced presentation.
    Stale(ModalStaleGenerationError),
}

impl fmt::Display for ModalDismissalError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "modal dismissal rejected: {self:?}")
    }
}

impl Error for ModalDismissalError {}

/// Returns whether this Operating-System Window currently owns a visible modal presentation.
///
/// This narrow read-only integration is intended for Terminal Input Focus and other underlay
/// policy. It exposes no queue, coordinator, focus-chain, overlay, or transient machinery. Queued
/// requests alone return `false`; promotion keeps the fact continuously true between presentations.
pub fn window_modal_is_open(window: &gpui::Window, cx: &gpui::App) -> bool {
    core::window_modal_is_open(window, cx)
}

/// Application-owned colors for the shared modal renderer.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ModalPaint {
    scrim: Rgba,
    surface: Rgba,
    border: Rgba,
    primary_text: Rgba,
    secondary_text: Rgba,
    divider: Rgba,
    progress_track: Rgba,
    progress_fill: Rgba,
    default_ring: Rgba,
    informational: Rgba,
    informational_background: Rgba,
    warning: Rgba,
    warning_background: Rgba,
    critical: Rgba,
    critical_background: Rgba,
}

impl ModalPaint {
    /// Creates the complete bounded modal paint catalog.
    #[expect(
        clippy::too_many_arguments,
        reason = "the bounded catalog requires every shared semantic paint"
    )]
    pub fn new(
        scrim: Rgba,
        surface: Rgba,
        border: Rgba,
        primary_text: Rgba,
        secondary_text: Rgba,
        divider: Rgba,
        progress_track: Rgba,
        progress_fill: Rgba,
        default_ring: Rgba,
        informational: Rgba,
        informational_background: Rgba,
        warning: Rgba,
        warning_background: Rgba,
        critical: Rgba,
        critical_background: Rgba,
    ) -> Self {
        Self {
            scrim,
            surface,
            border,
            primary_text,
            secondary_text,
            divider,
            progress_track,
            progress_fill,
            default_ring,
            informational,
            informational_background,
            warning,
            warning_background,
            critical,
            critical_background,
        }
    }
}

/// Bounded dimensions for the compact shared modal renderer.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ModalMetrics {
    compact_width: Pixels,
    regular_width: Pixels,
    wide_width: Pixels,
    maximum_height: Pixels,
    alert_height_cap: Pixels,
    dialog_height_cap: Pixels,
    progress_height_cap: Pixels,
    viewport_margin: Pixels,
    top_offset: Pixels,
    surface_padding: Pixels,
    section_gap: Pixels,
    action_gap: Pixels,
    accessory_extent: Pixels,
    horizontal_action_threshold: Pixels,
    minimum_action_width: Pixels,
    corner_radius: Pixels,
    border_width: Pixels,
    progress_track_thickness: Pixels,
    progress_track_radius: Pixels,
    progress_status_region_height: Pixels,
    progress_detail_region_height: Pixels,
    header_maximum_fraction: f32,
    footer_maximum_fraction: f32,
    indeterminate_segment_fraction: f32,
    title_size: Pixels,
    body_size: Pixels,
    detail_size: Pixels,
}

impl ModalMetrics {
    /// Creates compact desktop defaults around the three bounded Dialog widths.
    pub fn new(compact_width: Pixels, regular_width: Pixels, wide_width: Pixels) -> Self {
        let compact_width = bounded_metric(compact_width, 280.0, 440.0, 360.0);
        let regular_width = bounded_metric(regular_width, 360.0, 640.0, 480.0).max(compact_width);
        let wide_width = bounded_metric(wide_width, 480.0, 840.0, 640.0).max(regular_width);
        Self {
            compact_width,
            regular_width,
            wide_width,
            maximum_height: px(620.0),
            alert_height_cap: px(360.0),
            dialog_height_cap: px(520.0),
            progress_height_cap: px(300.0),
            viewport_margin: px(16.0),
            top_offset: px(42.0),
            surface_padding: px(16.0),
            section_gap: px(12.0),
            action_gap: px(8.0),
            accessory_extent: px(40.0),
            horizontal_action_threshold: px(360.0),
            minimum_action_width: px(72.0),
            corner_radius: px(8.0),
            border_width: px(1.0),
            progress_track_thickness: px(8.0),
            progress_track_radius: px(2.0),
            progress_status_region_height: px(48.0),
            progress_detail_region_height: px(36.0),
            header_maximum_fraction: 0.35,
            footer_maximum_fraction: 0.48,
            indeterminate_segment_fraction: 0.18,
            title_size: px(15.0),
            body_size: px(13.0),
            detail_size: px(12.0),
        }
    }

    /// Returns a bounded large-text catalog by scaling every geometry and text metric together.
    pub fn scaled(self, factor: f32) -> Self {
        let factor = if factor.is_finite() {
            factor.clamp(1.0, 2.0)
        } else {
            1.0
        };
        Self {
            compact_width: bounded_metric(self.compact_width * factor, 280.0, 720.0, 360.0),
            regular_width: bounded_metric(self.regular_width * factor, 360.0, 960.0, 480.0),
            wide_width: bounded_metric(self.wide_width * factor, 480.0, 1200.0, 640.0),
            maximum_height: bounded_metric(self.maximum_height * factor, 320.0, 1200.0, 620.0),
            alert_height_cap: bounded_metric(self.alert_height_cap * factor, 200.0, 720.0, 360.0),
            dialog_height_cap: bounded_metric(
                self.dialog_height_cap * factor,
                240.0,
                1040.0,
                520.0,
            ),
            progress_height_cap: bounded_metric(
                self.progress_height_cap * factor,
                180.0,
                600.0,
                300.0,
            ),
            viewport_margin: bounded_metric(self.viewport_margin * factor, 8.0, 48.0, 16.0),
            top_offset: bounded_metric(self.top_offset * factor, 16.0, 96.0, 42.0),
            surface_padding: bounded_metric(self.surface_padding * factor, 10.0, 40.0, 16.0),
            section_gap: bounded_metric(self.section_gap * factor, 6.0, 32.0, 12.0),
            action_gap: bounded_metric(self.action_gap * factor, 4.0, 24.0, 8.0),
            accessory_extent: bounded_metric(self.accessory_extent * factor, 32.0, 96.0, 40.0),
            horizontal_action_threshold: bounded_metric(
                self.horizontal_action_threshold * factor,
                320.0,
                960.0,
                360.0,
            ),
            minimum_action_width: bounded_metric(
                self.minimum_action_width * factor,
                64.0,
                200.0,
                72.0,
            ),
            corner_radius: bounded_metric(self.corner_radius * factor, 4.0, 16.0, 8.0),
            border_width: bounded_metric(self.border_width * factor, 1.0, 3.0, 1.0),
            progress_track_thickness: bounded_metric(
                self.progress_track_thickness * factor,
                4.0,
                20.0,
                8.0,
            ),
            progress_track_radius: bounded_metric(
                self.progress_track_radius * factor,
                1.0,
                8.0,
                2.0,
            ),
            progress_status_region_height: bounded_metric(
                self.progress_status_region_height * factor,
                32.0,
                128.0,
                48.0,
            ),
            progress_detail_region_height: bounded_metric(
                self.progress_detail_region_height * factor,
                24.0,
                96.0,
                36.0,
            ),
            header_maximum_fraction: self.header_maximum_fraction,
            footer_maximum_fraction: self.footer_maximum_fraction,
            indeterminate_segment_fraction: self.indeterminate_segment_fraction,
            title_size: bounded_metric(self.title_size * factor, 12.0, 30.0, 15.0),
            body_size: bounded_metric(self.body_size * factor, 11.0, 28.0, 13.0),
            detail_size: bounded_metric(self.detail_size * factor, 10.0, 26.0, 12.0),
        }
    }

    pub(super) const fn action_gap(self) -> Pixels {
        self.action_gap
    }

    pub(super) const fn accessory_extent(self) -> Pixels {
        self.accessory_extent
    }

    pub(super) const fn horizontal_action_threshold(self) -> Pixels {
        self.horizontal_action_threshold
    }

    pub(super) const fn minimum_action_width(self) -> Pixels {
        self.minimum_action_width
    }

    pub(super) const fn alert_height_cap(self) -> Pixels {
        self.alert_height_cap
    }

    pub(super) const fn dialog_height_cap(self) -> Pixels {
        self.dialog_height_cap
    }

    pub(super) const fn progress_height_cap(self) -> Pixels {
        self.progress_height_cap
    }

    pub(super) const fn progress_track_thickness(self) -> Pixels {
        self.progress_track_thickness
    }

    pub(super) const fn progress_track_radius(self) -> Pixels {
        self.progress_track_radius
    }

    pub(super) const fn progress_status_region_height(self) -> Pixels {
        self.progress_status_region_height
    }

    pub(super) const fn progress_detail_region_height(self) -> Pixels {
        self.progress_detail_region_height
    }

    pub(super) const fn header_maximum_fraction(self) -> f32 {
        self.header_maximum_fraction
    }

    pub(super) const fn footer_maximum_fraction(self) -> f32 {
        self.footer_maximum_fraction
    }

    pub(super) const fn indeterminate_segment_fraction(self) -> f32 {
        self.indeterminate_segment_fraction
    }

    pub(super) const fn viewport_margin(self) -> Pixels {
        self.viewport_margin
    }

    pub(super) const fn maximum_height(self) -> Pixels {
        self.maximum_height
    }

    pub(super) const fn top_offset(self) -> Pixels {
        self.top_offset
    }

    pub(super) const fn width_for(self, size: DialogSize) -> Pixels {
        match size {
            DialogSize::Compact => self.compact_width,
            DialogSize::Regular => self.regular_width,
            DialogSize::Wide => self.wide_width,
        }
    }
}

/// One immutable application-installed modal presentation catalog.
///
/// The application owns all product paint and bounded geometry through this aggregate catalog.
/// Call sites receive no per-modal paint, radius, backdrop, shadow, animation, or raw size escape
/// hatch. Logical desktop semantics remain separately owned by [`ModalDesktopPolicy`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ModalTheme {
    paint: ModalPaint,
    metrics: ModalMetrics,
}

impl ModalTheme {
    /// Creates the complete modal theme. Shared surfaces are intentionally animation-free.
    pub fn new(paint: ModalPaint, metrics: ModalMetrics) -> Self {
        Self { paint, metrics }
    }

    /// Returns `false`; modal correctness and presentation never depend on animation.
    pub const fn surface_animation_enabled(&self) -> bool {
        false
    }
}

impl Global for ModalTheme {}

/// Explicitly installs application-owned aggregate modal paint and bounded metrics.
///
/// The root application normally installs this through its complete control-theme catalog. Policy
/// installation remains an explicit separate operation through [`install_modal_policy`].
pub fn install_modal_theme(cx: &mut gpui::App, theme: ModalTheme) {
    cx.set_global(theme);
}

/// Viewport-local geometry selected without paint or GPUI ownership state.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct ModalSurfaceGeometry {
    pub(crate) origin_x: Pixels,
    pub(crate) origin_y: Pixels,
    pub(crate) size: Size<Pixels>,
}

pub(super) fn clamp_surface_to_viewport(
    viewport: Size<Pixels>,
    desired: Size<Pixels>,
    metrics: ModalMetrics,
) -> ModalSurfaceGeometry {
    let margin = metrics.viewport_margin();
    let available_width = (viewport.width - margin * 2.0).max(px(1.0));
    let available_height = (viewport.height - margin * 2.0).max(px(1.0));
    let width = desired.width.min(available_width).max(px(1.0));
    let height = desired
        .height
        .min(metrics.maximum_height())
        .min(available_height)
        .max(px(1.0));
    ModalSurfaceGeometry {
        origin_x: ((viewport.width - width) / 2.0).max(px(0.0)),
        origin_y: metrics
            .top_offset()
            .min((viewport.height - height).max(px(0.0))),
        size: size(width, height),
    }
}

pub(super) const MAX_MODAL_ID_CHARACTERS: usize = 128;
pub(super) const MAX_MODAL_TITLE_CHARACTERS: usize = 512;
pub(super) const MAX_ACTION_LABEL_CHARACTERS: usize = 256;
pub(super) const MAX_ACTION_DEBUG_IDENTITY_CHARACTERS: usize = 128;

pub(super) fn validate_required_text(
    value: &str,
    field: ModalTextField,
) -> Result<(), ModalValidationError> {
    if value.trim().is_empty() {
        Err(ModalValidationError::EmptyText(field))
    } else {
        Ok(())
    }
}

pub(super) fn validate_bounded_text(
    value: &str,
    field: ModalTextField,
    maximum: usize,
) -> Result<(), ModalValidationError> {
    if value.chars().count() > maximum {
        Err(ModalValidationError::TextTooLong { field, maximum })
    } else {
        Ok(())
    }
}

fn bounded_metric(value: Pixels, minimum: f32, maximum: f32, fallback: f32) -> Pixels {
    let value = f32::from(value);
    px(if value.is_finite() {
        value.clamp(minimum, maximum)
    } else {
        fallback
    })
}

#[cfg(test)]
mod tests {
    use gpui::{px, size};

    use super::*;

    #[test]
    fn modal_metrics_clamp_surface_to_tiny_viewport() {
        let metrics = ModalMetrics::new(px(360.0), px(480.0), px(640.0));
        let geometry = clamp_surface_to_viewport(
            size(px(220.0), px(140.0)),
            size(px(480.0), px(500.0)),
            metrics,
        );

        assert_eq!(geometry.size, size(px(188.0), px(108.0)));
    }

    #[test]
    fn modal_metrics_resolve_complete_ordinary_geometry_catalog() {
        let metrics = ModalMetrics::new(px(360.0), px(480.0), px(640.0));

        assert_eq!(
            (
                metrics.alert_height_cap(),
                metrics.dialog_height_cap(),
                metrics.progress_height_cap(),
                metrics.progress_track_thickness(),
                metrics.progress_track_radius(),
                metrics.progress_status_region_height(),
                metrics.progress_detail_region_height(),
            ),
            (
                px(360.0),
                px(520.0),
                px(300.0),
                px(8.0),
                px(2.0),
                px(48.0),
                px(36.0),
            )
        );
    }

    #[test]
    fn modal_metrics_scale_complete_geometry_for_large_text_with_bounds() {
        let metrics = ModalMetrics::new(px(360.0), px(480.0), px(640.0)).scaled(20.0);

        assert_eq!(
            (
                metrics.width_for(DialogSize::Wide),
                metrics.alert_height_cap(),
                metrics.dialog_height_cap(),
                metrics.progress_height_cap(),
                metrics.progress_track_thickness(),
                metrics.progress_track_radius(),
                metrics.progress_status_region_height(),
                metrics.progress_detail_region_height(),
            ),
            (
                px(1200.0),
                px(720.0),
                px(1040.0),
                px(600.0),
                px(16.0),
                px(4.0),
                px(96.0),
                px(72.0),
            )
        );
    }

    #[test]
    fn modal_metrics_resolve_constrained_and_nonfinite_inputs() {
        let metrics = ModalMetrics::new(px(f32::NAN), px(120.0), px(f32::INFINITY));
        let geometry = clamp_surface_to_viewport(
            size(px(90.0), px(70.0)),
            size(
                metrics.width_for(DialogSize::Wide),
                metrics.progress_height_cap(),
            ),
            metrics,
        );

        assert_eq!(geometry.size, size(px(58.0), px(38.0)));
    }

    #[test]
    fn production_renderer_owns_no_facade_or_progress_track_geometry_literals() {
        let production = include_str!("render.rs")
            .split("#[cfg(test)]")
            .next()
            .unwrap_or_default();

        assert!(
            [
                "px(360.0)",
                "px(520.0)",
                "px(300.0)",
                "max(px(4.0))",
                "rounded(px(2.0))",
            ]
            .into_iter()
            .all(|literal| !production.contains(literal)),
            "production modal geometry bypassed ModalMetrics"
        );
    }

    #[test]
    fn modal_theme_has_no_surface_animation_escape_hatch() {
        let color = gpui::rgba(0x112233ff);
        let paint = ModalPaint::new(
            color, color, color, color, color, color, color, color, color, color, color, color,
            color, color, color,
        );
        let theme = ModalTheme::new(paint, ModalMetrics::new(px(360.0), px(480.0), px(640.0)));

        assert!(!theme.surface_animation_enabled());
    }
}
