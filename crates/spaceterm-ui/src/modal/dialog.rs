use std::rc::Rc;

use gpui::{AnyView, App, Context, Entity, FocusHandle, Render, SharedString, Window};

use super::{
    DialogCompletion, DialogPendingCompletion, ModalAction, ModalActivationSource,
    ModalCloseReason, ModalDesktopPolicy, ModalId, ModalLifecycleEvent, ModalPresentationError,
    ModalPresentationId, ModalValidationError,
    core::{
        InternalOutcome, ModalKind, PreparedFocusIntent, PreparedModalRequest,
        PreparedModalSemantics,
    },
};

/// Maximum Unicode scalar count for optional Dialog description.
const MAX_DIALOG_DESCRIPTION_CHARACTERS: usize = 2048;

/// Bounded surface width selected from installed metrics rather than call-site dimensions.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum DialogSize {
    /// Dense prompts and small option groups.
    Compact,
    /// Ordinary forms and scoped tasks.
    #[default]
    Regular,
    /// Forms that need an additional bounded column of content.
    Wide,
}

/// Explicit first-focus contract for a Dialog presentation.
///
/// The renderer resolves this target against the current frame. A missing or no-longer-rendered
/// body target falls back to the first live contained target, and later disablement or removal is
/// repaired without allowing focus to escape the Dialog.
pub enum DialogInitialFocus<A> {
    /// Focus this caller-owned body control when it is live and rendered.
    Body(FocusHandle),
    /// Focus the enabled action with this caller-owned identity.
    Action(A),
}

/// Typed request emitted before a Dialog action may close the presentation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DialogActionRequest<A> {
    action_id: A,
    source: ModalActivationSource,
    presentation: ModalPresentationId,
}

impl<A> DialogActionRequest<A> {
    pub(super) fn new(
        action_id: A,
        source: ModalActivationSource,
        presentation: ModalPresentationId,
    ) -> Self {
        Self {
            action_id,
            source,
            presentation,
        }
    }

    /// Returns the caller-owned typed action identity.
    pub fn action_id(&self) -> &A {
        &self.action_id
    }

    /// Returns the content-free activation path.
    pub const fn source(&self) -> ModalActivationSource {
        self.source
    }

    /// Returns the opaque presentation identity required by asynchronous completion.
    pub const fn presentation(&self) -> ModalPresentationId {
        self.presentation
    }
}

/// Caller decision for one close attempt.
///
/// The request callback runs after the private reducer update is released. [`Self::Pending`]
/// transitions the matching presentation and attempt into duplicate-safe pending state. One safe
/// nested Cancel attempt may coexist with an original pending attempt. The callback's
/// [`DialogPendingCompletion`] later allows or denies its exact attempt; denial preserves any
/// still-live counterpart, while the first allowed terminal decision closes exactly once.
pub enum DialogCloseDecision {
    /// Close with the requested typed action.
    Allow,
    /// Keep all caller-owned values and remain open, optionally focusing the first invalid field.
    Deny {
        /// A body focus owner to receive focus after inline validation is published.
        first_invalid: Option<FocusHandle>,
    },
    /// Enter a duplicate-safe pending state until the matching presentation is completed.
    Pending,
}

/// Typed terminal result delivered exactly once for a Dialog presentation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DialogOutcome<A> {
    /// A typed action was allowed to complete the Dialog.
    Completed {
        /// Stable caller-owned action identity.
        action_id: A,
        /// Content-free activation path.
        source: ModalActivationSource,
    },
    /// The generation-bound completion operation completed the Dialog independently of actions.
    ProgrammaticallyCompleted,
    /// The Dialog ended without an allowed action or programmatic completion.
    Dismissed(ModalCloseReason),
}

/// A compact window-modal task surface for forms, options, and short scoped workflows.
///
/// The shared renderer owns the fixed header and footer, vertically scrollable body viewport,
/// adaptive action area, focus scope, and complete underlay modality. [`Self::body`] attaches one
/// caller-owned reusable GPUI entity without exposing those mechanisms. Tab and Shift-Tab remain
/// contained and are repaired when a target disables or disappears. Focused children receive
/// Return and Escape before the Dialog, including input-method composition cancellation.
///
/// Actions retain caller-owned typed identity and logical order while installed desktop policy
/// owns physical ordering. Denied close attempts preserve caller-owned field values and may focus
/// the first invalid field. Pending attempts block duplicate activation while allowing the safe
/// Cancel path where configured. Close and result callbacks are exactly once and run after the
/// private owner update is released. While a primary action is pending, at most one nested Cancel
/// attempt may be requested; duplicate Cancel activation is disabled without replacing either
/// completion authority.
///
/// # Example
///
/// ```
/// use spaceterm_ui::{
///     Dialog, DialogInitialFocus, ModalAction, ModalActionRole, ModalDesktopPolicy, ModalId,
/// };
///
/// #[derive(Clone, Debug, Eq, PartialEq)]
/// enum Action {
///     Save,
///     Cancel,
/// }
///
/// let dialog = Dialog::new(
///     ModalId::new("settings-dialog"),
///     "Save workspace settings",
///     "Workspace Settings",
///     vec![
///         ModalAction::new(Action::Save, "Save", ModalActionRole::Affirmative, "save")
///             .default_action(true),
///         ModalAction::new(Action::Cancel, "Cancel", ModalActionRole::Cancel, "cancel"),
///     ],
///     DialogInitialFocus::Action(Action::Cancel),
/// );
///
/// assert_eq!(dialog.validate(&ModalDesktopPolicy::mac_os()), Ok(()));
/// ```
pub struct Dialog<A> {
    pub(super) id: ModalId,
    pub(super) accessibility_title: SharedString,
    pub(super) title: SharedString,
    pub(super) description: Option<SharedString>,
    pub(super) size: DialogSize,
    pub(super) actions: Vec<ModalAction<A>>,
    pub(super) initial_focus: DialogInitialFocus<A>,
    pub(super) body: Option<AnyView>,
}

impl<A> Dialog<A> {
    /// Creates a Dialog with an explicit body or action initial-focus target.
    pub fn new(
        id: ModalId,
        accessibility_title: impl Into<SharedString>,
        title: impl Into<SharedString>,
        actions: Vec<ModalAction<A>>,
        initial_focus: DialogInitialFocus<A>,
    ) -> Self {
        Self {
            id,
            accessibility_title: accessibility_title.into(),
            title: title.into(),
            description: None,
            size: DialogSize::Regular,
            actions,
            initial_focus,
            body: None,
        }
    }

    /// Attaches one caller-owned GPUI body entity inside the shared vertical viewport.
    pub fn body<V: Render>(mut self, body: Entity<V>) -> Self {
        self.body = Some(body.into());
        self
    }

    /// Sets concise descriptive text related to the logical title.
    pub fn description(mut self, description: impl Into<SharedString>) -> Self {
        self.description = Some(description.into());
        self
    }

    /// Selects one bounded width from the installed modal metrics.
    pub fn size(mut self, size: DialogSize) -> Self {
        self.size = size;
        self
    }

    /// Validates this configuration without presenting or changing caller order.
    pub fn validate(&self, policy: &ModalDesktopPolicy) -> Result<(), ModalValidationError>
    where
        A: Eq,
    {
        policy.validate_dialog(self)
    }

    /// Presents this Dialog through the shared per-window owner and bounded FIFO queue.
    ///
    /// `on_action` runs after `ActionRequested` and outside the owner update. Returning
    /// [`DialogCloseDecision::Allow`] produces the terminal result, `Deny` restores open state,
    /// and `Pending` retains the supplied [`DialogPendingCompletion`] as asynchronous authority.
    /// A nested Cancel denial restores that authority; a nested Cancel allow closes exactly once.
    ///
    /// # Errors
    ///
    /// Returns [`ModalPresentationError`] when configuration is invalid, desktop policy is not
    /// installed, the Operating-System Window is unavailable, or eight requests already wait.
    pub fn present<T: 'static>(
        self,
        window: &Window,
        cx: &mut Context<T>,
        on_action: impl Fn(
            DialogActionRequest<A>,
            DialogPendingCompletion,
            &mut App,
        ) -> DialogCloseDecision
        + 'static,
        on_result: impl FnOnce(DialogOutcome<A>, &mut App) + 'static,
    ) -> Result<DialogCompletion, ModalPresentationError>
    where
        A: Clone + Eq + 'static,
    {
        self.present_with_lifecycle(window, cx, on_action, on_result, |_, _| {})
    }

    /// Presents this Dialog and observes lifecycle transitions after reducer updates are released.
    ///
    /// Observable order is `Opened`, `ActionRequested`, optional `Pending`, `Closing`, result,
    /// then `Closed`. Denial returns to open state without a close event. A queued Dialog dismissed
    /// before opening emits only `Closed`. Reentrant callbacks cannot enter a locked owner entity.
    ///
    /// # Errors
    ///
    /// Returns [`ModalPresentationError`] under the same conditions as [`Self::present`].
    pub fn present_with_lifecycle<T: 'static>(
        self,
        window: &Window,
        cx: &mut Context<T>,
        on_action: impl Fn(
            DialogActionRequest<A>,
            DialogPendingCompletion,
            &mut App,
        ) -> DialogCloseDecision
        + 'static,
        on_result: impl FnOnce(DialogOutcome<A>, &mut App) + 'static,
        on_lifecycle: impl Fn(&ModalLifecycleEvent, &mut App) + 'static,
    ) -> Result<DialogCompletion, ModalPresentationError>
    where
        A: Clone + Eq + 'static,
    {
        self.present_with_operation(
            window,
            cx,
            on_action,
            on_result,
            on_lifecycle,
            super::ModalPresentationOperation::Present,
        )
    }

    /// Replaces the currently visible modal with this Dialog in one owner transition.
    ///
    /// The predecessor's `Closing`, terminal result, and `Closed(Replaced)` callbacks run before
    /// this Dialog emits `Opened`. Existing queued requests remain in FIFO order behind the
    /// replacement, with no frame in which the underlay can interact. Every retained predecessor
    /// dismissal, pending or programmatic completion, cancellation, and update operation is stale
    /// before callbacks run.
    ///
    /// # Errors
    ///
    /// Returns [`ModalPresentationError`] when configuration is invalid, desktop policy is not
    /// installed, the Operating-System Window is unavailable, or no modal is currently visible.
    pub fn replace_active<T: 'static>(
        self,
        window: &Window,
        cx: &mut Context<T>,
        on_action: impl Fn(
            DialogActionRequest<A>,
            DialogPendingCompletion,
            &mut App,
        ) -> DialogCloseDecision
        + 'static,
        on_result: impl FnOnce(DialogOutcome<A>, &mut App) + 'static,
        on_lifecycle: impl Fn(&ModalLifecycleEvent, &mut App) + 'static,
    ) -> Result<DialogCompletion, ModalPresentationError>
    where
        A: Clone + Eq + 'static,
    {
        self.present_with_operation(
            window,
            cx,
            on_action,
            on_result,
            on_lifecycle,
            super::ModalPresentationOperation::ReplaceActive,
        )
    }

    fn present_with_operation<T: 'static>(
        self,
        window: &Window,
        cx: &mut Context<T>,
        on_action: impl Fn(
            DialogActionRequest<A>,
            DialogPendingCompletion,
            &mut App,
        ) -> DialogCloseDecision
        + 'static,
        on_result: impl FnOnce(DialogOutcome<A>, &mut App) + 'static,
        on_lifecycle: impl Fn(&ModalLifecycleEvent, &mut App) + 'static,
        operation: super::ModalPresentationOperation,
    ) -> Result<DialogCompletion, ModalPresentationError>
    where
        A: Clone + Eq + 'static,
    {
        let policy = cx
            .try_global::<ModalDesktopPolicy>()
            .copied()
            .ok_or(ModalPresentationError::DesktopPolicyNotInstalled)?;
        self.validate(&policy)?;
        let default_action = policy.return_action(&self.actions);
        let cancel_action = policy.cancel_action(&self.actions);
        let focus_intent = match &self.initial_focus {
            DialogInitialFocus::Body(focus) => PreparedFocusIntent::Body(focus.clone()),
            DialogInitialFocus::Action(action_id) => {
                let Some(index) = self
                    .actions
                    .iter()
                    .position(|action| action.id() == action_id)
                else {
                    return Err(ModalValidationError::InvalidDialogInitialFocus.into());
                };
                PreparedFocusIntent::Action(index)
            }
        };
        let action_ids = self
            .actions
            .iter()
            .map(|action| action.id().clone())
            .collect::<Vec<_>>();
        let result_action_ids = action_ids.clone();
        let body = self.body;
        let request = PreparedModalRequest::new(
            self.id,
            ModalKind::Dialog,
            PreparedModalRequest::erase_actions(self.actions),
            PreparedModalSemantics::Dialog {
                accessibility_title: self.accessibility_title,
                visible_title: self.title,
                description: self.description,
                default_action,
                cancel_action,
            },
            focus_intent,
            cx.weak_entity().into(),
            Box::new(move |outcome, cx| {
                let outcome = match outcome {
                    InternalOutcome::Activated {
                        action_index,
                        source,
                    } => {
                        let Some(action_id) = result_action_ids.get(action_index).cloned() else {
                            return;
                        };
                        DialogOutcome::Completed { action_id, source }
                    }
                    InternalOutcome::DialogProgrammaticCompletion => {
                        DialogOutcome::ProgrammaticallyCompleted
                    }
                    InternalOutcome::Dismissed(reason) => DialogOutcome::Dismissed(reason),
                    InternalOutcome::Progress(_) => return,
                };
                on_result(outcome, cx);
            }),
        )
        .with_body(body)
        .with_dialog_size(self.size)
        .with_dialog_action(Rc::new(
            move |action_index, source, presentation, completion, cx| {
                let Some(action_id) = action_ids.get(action_index).cloned() else {
                    return DialogCloseDecision::Deny {
                        first_invalid: None,
                    };
                };
                on_action(
                    DialogActionRequest::new(action_id, source, presentation),
                    completion,
                    cx,
                )
            },
        ))
        .with_lifecycle(Some(Rc::new(on_lifecycle)));
        operation
            .apply(request, window, cx)
            .map(DialogCompletion::new)
    }
}

pub(super) fn validate_description(description: &str) -> Result<(), ModalValidationError> {
    super::validate_bounded_text(
        description,
        super::ModalTextField::Detail,
        MAX_DIALOG_DESCRIPTION_CHARACTERS,
    )
}
