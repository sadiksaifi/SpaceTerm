use std::{error::Error, fmt, rc::Rc, time::Duration};

use gpui::{App, Context, SharedString, Window};

use super::{
    ModalAction, ModalActivationSource, ModalCloseReason, ModalDesktopPolicy, ModalId,
    ModalLifecycleEvent, ModalPresentationError, ModalValidationError,
    ProgressCancellationCompletion, ProgressDialogHandle,
    core::{
        InternalOutcome, ModalKind, PreparedFocusIntent, PreparedModalRequest,
        PreparedModalSemantics,
    },
};

/// Maximum Unicode scalar count for ProgressDialog status.
pub const MAX_PROGRESS_STATUS_CHARACTERS: usize = 512;
/// Maximum Unicode scalar count for optional ProgressDialog detail.
pub const MAX_PROGRESS_DETAIL_CHARACTERS: usize = 2048;

/// Error constructing a normalized determinate progress value.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProgressValueError {
    /// NaN and positive or negative infinity cannot represent determinate progress.
    NotFinite,
}

impl fmt::Display for ProgressValueError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "determinate progress must be finite")
    }
}

impl Error for ProgressValueError {}

/// Finite normalized progress in the inclusive range `0.0..=1.0`.
///
/// ```
/// use spaceterm_ui::DeterminateProgress;
///
/// let progress = DeterminateProgress::new(1.25)?;
/// assert_eq!(progress.value(), 1.0);
/// assert!(progress.is_maximum());
/// # Ok::<(), spaceterm_ui::ProgressValueError>(())
/// ```
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DeterminateProgress(f32);

impl DeterminateProgress {
    /// Normalizes a finite value by clamping it to the inclusive unit range.
    pub fn new(value: f64) -> Result<Self, ProgressValueError> {
        if !value.is_finite() {
            return Err(ProgressValueError::NotFinite);
        }
        Ok(Self(value.clamp(0.0, 1.0) as f32))
    }

    /// Returns the normalized finite value.
    pub const fn value(self) -> f32 {
        self.0
    }

    /// Reaching one is presentation state only and does not complete a ProgressDialog.
    pub const fn is_maximum(self) -> bool {
        self.0 >= 1.0
    }
}

/// Progress presentation with explicit determinate and indeterminate states.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ProgressState {
    /// Work has no knowable normalized completion value.
    Indeterminate,
    /// Work has a finite normalized completion value.
    Determinate(DeterminateProgress),
}

/// Safe dismissal policy for a ProgressDialog.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ProgressCancellation<A> {
    /// Declare typed cancellation capability with the action's enabled state as initial availability.
    ///
    /// Retained updates may change availability without changing this immutable semantic action.
    Cancellable(ModalAction<A>),
    /// Permit no user dismissal for a nonzero, bounded interval.
    ProgrammaticOnly {
        /// Deadline after which the modal closes with a typed terminal outcome.
        deadline: Duration,
    },
}

impl<A> ProgressCancellation<A> {
    /// Creates bounded programmatic-only mode.
    pub fn programmatic_only(deadline: Duration) -> Self {
        Self::ProgrammaticOnly { deadline }
    }
}

/// Caller decision for one ProgressDialog cancellation request.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProgressCancelDecision {
    /// Accept cancellation and close exactly once.
    Allow,
    /// Reject cancellation and remain open with caller-owned status/detail updates.
    Deny,
    /// Enter duplicate-safe pending cancellation until authoritative completion.
    Pending,
}

/// Typed terminal result delivered exactly once for a ProgressDialog presentation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProgressDialogOutcome {
    /// The caller explicitly completed the bounded operation.
    Completed,
    /// An allowed typed Cancel action completed the presentation.
    Cancelled {
        /// Content-free path that requested cancellation.
        source: ModalActivationSource,
    },
    /// The operation failed without placing sensitive detail in the outcome.
    Failed,
    /// The programmatic-only deadline expired.
    DeadlineExpired,
    /// The owning Operating-System Window or caller owner was removed.
    OwnerRemoved,
    /// The owner explicitly dismissed the presentation without completing work.
    ProgrammaticDismissal,
    /// A newer authoritative modal presentation replaced this operation.
    Replaced,
}

impl ProgressDialogOutcome {
    pub(super) const fn close_reason(self) -> ModalCloseReason {
        match self {
            Self::Completed | Self::ProgrammaticDismissal => ModalCloseReason::Programmatic,
            Self::Cancelled { .. } => ModalCloseReason::Cancelled,
            Self::Failed => ModalCloseReason::Programmatic,
            Self::DeadlineExpired => ModalCloseReason::DeadlineExpired,
            Self::OwnerRemoved => ModalCloseReason::OwnerRemoved,
            Self::Replaced => ModalCloseReason::Replaced,
        }
    }
}

/// Caller-driven bounded ProgressDialog update.
///
/// An update changes only supplied fields. Each [`ProgressDialogHandle`] clone retains its observed
/// private generation, so an older retained operation cannot overwrite newer status. Determinate
/// `1.0` remains presentation state and never implies terminal completion.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ProgressDialogUpdate {
    pub(super) status: Option<SharedString>,
    pub(super) detail: Option<Option<SharedString>>,
    pub(super) progress: Option<ProgressState>,
    pub(super) cancellation_enabled: Option<bool>,
}

impl ProgressDialogUpdate {
    /// Creates an update with no changed fields.
    pub fn new() -> Self {
        Self::default()
    }

    /// Replaces the succinct status text.
    pub fn status(mut self, status: impl Into<SharedString>) -> Self {
        self.status = Some(status.into());
        self
    }

    /// Replaces determinate or indeterminate progress without completing the modal.
    pub fn progress(mut self, progress: ProgressState) -> Self {
        self.progress = Some(progress);
        self
    }

    /// Replaces or clears concise secondary detail.
    pub fn detail(mut self, detail: Option<impl Into<SharedString>>) -> Self {
        self.detail = Some(detail.map(Into::into));
        self
    }

    /// Changes current typed cancellation availability.
    ///
    /// A generation-bound handle rejects this field with
    /// [`super::ModalUpdateError::CancellationNotSupported`] when the presentation was created in
    /// programmatic-only mode. Context-free [`Self::validate`] intentionally validates only the
    /// update's bounded content because it does not know presentation capability.
    pub fn cancellation_enabled(mut self, enabled: bool) -> Self {
        self.cancellation_enabled = Some(enabled);
        self
    }

    /// Validates bounded update text before an operational handle checks its generation.
    ///
    /// # Errors
    ///
    /// Returns [`ModalValidationError`] when supplied status or detail exceeds its bound or when
    /// status is empty.
    pub fn validate(&self) -> Result<(), ModalValidationError> {
        if let Some(status) = &self.status {
            validate_status(status)?;
        }
        if let Some(Some(detail)) = &self.detail {
            validate_detail(detail)?;
        }
        Ok(())
    }
}

/// A specialized window-modal surface for bounded work.
///
/// Status and progress remain stable on one shared modal surface across updates. A cancellable
/// action's enabled state establishes initial availability, while retained updates may later enable
/// or disable that immutable capability. Cancellable mode enters focus on the currently enabled
/// Cancel action; programmatic-only mode enters on the modal surface and requires a nonzero
/// installed-policy deadline. Escape reaches cancellation only after the
/// focused child declines it and only while cancellation is enabled. Cancellation may allow, deny,
/// or become pending, with duplicate requests blocked until authoritative completion. The opaque
/// completion retains the original activation source, so delayed cancellation cannot relabel it.
///
/// Progress reaches a terminal state only through explicit completion, failure, dismissal, allowed
/// cancellation, owner removal, replacement, or deadline expiry. Every terminal outcome is
/// content-free and delivered exactly once after the invoking GPUI update unwinds.
///
/// # Example
///
/// ```
/// use std::time::Duration;
///
/// use spaceterm_ui::{
///     ModalDesktopPolicy, ModalId, ProgressCancellation, ProgressDialog, ProgressState,
/// };
///
/// let progress = ProgressDialog::<()>::new(
///     ModalId::new("migration-progress"),
///     "Applying required migration",
///     "Applying Migration",
///     "Preparing changes",
///     ProgressState::Indeterminate,
///     ProgressCancellation::programmatic_only(Duration::from_secs(30)),
/// );
///
/// assert_eq!(progress.validate(&ModalDesktopPolicy::mac_os()), Ok(()));
/// ```
#[derive(Clone, Debug, PartialEq)]
pub struct ProgressDialog<A> {
    pub(super) id: ModalId,
    pub(super) accessibility_title: SharedString,
    pub(super) title: SharedString,
    pub(super) status: SharedString,
    pub(super) detail: Option<SharedString>,
    pub(super) progress: ProgressState,
    pub(super) cancellation: ProgressCancellation<A>,
}

impl<A> ProgressDialog<A> {
    /// Creates a ProgressDialog with an explicit safe cancellation or bounded deadline policy.
    pub fn new(
        id: ModalId,
        accessibility_title: impl Into<SharedString>,
        title: impl Into<SharedString>,
        status: impl Into<SharedString>,
        progress: ProgressState,
        cancellation: ProgressCancellation<A>,
    ) -> Self {
        Self {
            id,
            accessibility_title: accessibility_title.into(),
            title: title.into(),
            status: status.into(),
            detail: None,
            progress,
            cancellation,
        }
    }

    /// Sets concise secondary detail.
    pub fn detail(mut self, detail: impl Into<SharedString>) -> Self {
        self.detail = Some(detail.into());
        self
    }

    /// Validates this configuration without presenting or changing caller semantics.
    pub fn validate(&self, policy: &ModalDesktopPolicy) -> Result<(), ModalValidationError>
    where
        A: Eq,
    {
        policy.validate_progress_dialog(self)
    }

    /// Presents this ProgressDialog and returns retained update and completion authority.
    ///
    /// The cancellation callback runs after the invoking GPUI update unwinds and receives opaque
    /// completion authority for the exact presentation, attempt, and activation source. Its
    /// terminal result callback runs exactly once.
    ///
    /// # Errors
    ///
    /// Returns [`ModalPresentationError`] when configuration is invalid, desktop policy is not
    /// installed, the Operating-System Window is unavailable, or eight requests already wait.
    pub fn present<T: 'static>(
        self,
        window: &Window,
        cx: &mut Context<T>,
        on_cancel: impl Fn(
            ModalActivationSource,
            ProgressCancellationCompletion,
            &mut App,
        ) -> ProgressCancelDecision
        + 'static,
        on_result: impl FnOnce(ProgressDialogOutcome, &mut App) + 'static,
    ) -> Result<ProgressDialogHandle, ModalPresentationError>
    where
        A: Clone + Eq + 'static,
    {
        self.present_with_lifecycle(window, cx, on_cancel, on_result, |_, _| {})
    }

    /// Presents this ProgressDialog and observes lifecycle transitions after the invoking GPUI
    /// update unwinds.
    ///
    /// Observable cancellation order is `ActionRequested`, optional `Pending`, `Closing`, result,
    /// then `Closed`. Programmatic terminal outcomes omit action-request transitions. A queued
    /// presentation dismissed before opening emits only `Closed`.
    ///
    /// # Errors
    ///
    /// Returns [`ModalPresentationError`] under the same conditions as [`Self::present`].
    pub fn present_with_lifecycle<T: 'static>(
        self,
        window: &Window,
        cx: &mut Context<T>,
        on_cancel: impl Fn(
            ModalActivationSource,
            ProgressCancellationCompletion,
            &mut App,
        ) -> ProgressCancelDecision
        + 'static,
        on_result: impl FnOnce(ProgressDialogOutcome, &mut App) + 'static,
        on_lifecycle: impl Fn(&ModalLifecycleEvent, &mut App) + 'static,
    ) -> Result<ProgressDialogHandle, ModalPresentationError>
    where
        A: Clone + Eq + 'static,
    {
        self.present_with_operation(
            window,
            cx,
            on_cancel,
            on_result,
            on_lifecycle,
            super::ModalPresentationOperation::Present,
        )
    }

    /// Replaces the currently visible modal with this ProgressDialog in one owner transition.
    ///
    /// The predecessor settles once with replacement before this presentation emits `Opened`.
    /// Waiting requests retain FIFO order behind this replacement, and underlay blocking remains
    /// continuous. Every retained predecessor dismissal, pending or programmatic completion,
    /// cancellation, and update operation is stale before callbacks run.
    ///
    /// # Errors
    ///
    /// Returns [`ModalPresentationError`] when configuration is invalid, desktop policy is not
    /// installed, the Operating-System Window is unavailable, or no modal is currently visible.
    pub fn replace_active<T: 'static>(
        self,
        window: &Window,
        cx: &mut Context<T>,
        on_cancel: impl Fn(
            ModalActivationSource,
            ProgressCancellationCompletion,
            &mut App,
        ) -> ProgressCancelDecision
        + 'static,
        on_result: impl FnOnce(ProgressDialogOutcome, &mut App) + 'static,
        on_lifecycle: impl Fn(&ModalLifecycleEvent, &mut App) + 'static,
    ) -> Result<ProgressDialogHandle, ModalPresentationError>
    where
        A: Clone + Eq + 'static,
    {
        self.present_with_operation(
            window,
            cx,
            on_cancel,
            on_result,
            on_lifecycle,
            super::ModalPresentationOperation::ReplaceActive,
        )
    }

    fn present_with_operation<T: 'static>(
        self,
        window: &Window,
        cx: &mut Context<T>,
        on_cancel: impl Fn(
            ModalActivationSource,
            ProgressCancellationCompletion,
            &mut App,
        ) -> ProgressCancelDecision
        + 'static,
        on_result: impl FnOnce(ProgressDialogOutcome, &mut App) + 'static,
        on_lifecycle: impl Fn(&ModalLifecycleEvent, &mut App) + 'static,
        operation: super::ModalPresentationOperation,
    ) -> Result<ProgressDialogHandle, ModalPresentationError>
    where
        A: Clone + Eq + 'static,
    {
        let policy = cx
            .try_global::<ModalDesktopPolicy>()
            .copied()
            .ok_or(ModalPresentationError::DesktopPolicyNotInstalled)?;
        self.validate(&policy)?;
        let focus_intent = match policy.progress_initial_focus(&self.cancellation) {
            super::ModalInitialFocus::Action(index) => PreparedFocusIntent::Action(index),
            super::ModalInitialFocus::Surface => PreparedFocusIntent::Surface,
        };
        let (actions, cancellation_capable, deadline) = match self.cancellation {
            ProgressCancellation::Cancellable(action) => (vec![action], true, None),
            ProgressCancellation::ProgrammaticOnly { deadline } => {
                (Vec::new(), false, Some(deadline))
            }
        };
        let request = PreparedModalRequest::new(
            self.id,
            ModalKind::Progress,
            PreparedModalRequest::erase_actions(actions),
            PreparedModalSemantics::Progress {
                accessibility_title: self.accessibility_title,
                visible_title: self.title,
                status: self.status,
                detail: self.detail,
                progress: self.progress,
                cancellation_capable,
            },
            focus_intent,
            cx.weak_entity().into(),
            Box::new(move |outcome, cx| {
                if let InternalOutcome::Progress(outcome) = outcome {
                    on_result(outcome, cx);
                }
            }),
        )
        .with_progress_cancel(Rc::new(on_cancel))
        .with_programmatic_deadline(deadline)
        .with_lifecycle(Some(Rc::new(on_lifecycle)));
        operation
            .apply(request, window, cx)
            .map(ProgressDialogHandle::new)
    }
}

pub(super) fn validate_status(status: &str) -> Result<(), ModalValidationError> {
    super::validate_required_text(status, super::ModalTextField::ProgressStatus)?;
    super::validate_bounded_text(
        status,
        super::ModalTextField::ProgressStatus,
        MAX_PROGRESS_STATUS_CHARACTERS,
    )
}

pub(super) fn validate_detail(detail: &str) -> Result<(), ModalValidationError> {
    super::validate_bounded_text(
        detail,
        super::ModalTextField::Detail,
        MAX_PROGRESS_DETAIL_CHARACTERS,
    )
}
