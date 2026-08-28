use std::{cell::Cell, rc::Rc, sync::Arc};

use gpui::{App, Context, RenderImage, SharedString, Window};

use super::{
    ModalAction, ModalActivationSource, ModalCloseReason, ModalDesktopPolicy, ModalId,
    ModalLifecycleEvent, ModalPresentationError, ModalPresentationHandle, ModalValidationError,
    core::{
        InternalOutcome, ModalKind, PreparedFocusIntent, PreparedModalRequest,
        PreparedModalSemantics,
    },
};

/// Maximum Unicode scalar count for an Alert message.
pub const MAX_ALERT_MESSAGE_CHARACTERS: usize = 2048;
/// Maximum Unicode scalar count for optional Alert detail.
pub const MAX_ALERT_DETAIL_CHARACTERS: usize = 4096;
/// Maximum Unicode scalar count for an Alert accessory's logical name.
const MAX_ALERT_ACCESSORY_NAME_CHARACTERS: usize = 256;
/// Maximum Unicode scalar count for a suppression choice label.
const MAX_ALERT_SUPPRESSION_LABEL_CHARACTERS: usize = 256;

/// Severity conveyed by an Alert without caller-provided paint.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum AlertIntent {
    /// Neutral information or acknowledgement.
    #[default]
    Informational,
    /// A recoverable condition requiring extra attention.
    Warning,
    /// A serious or destructive decision.
    Critical,
}

/// One bounded semantic accessory slot owned by the shared Alert renderer.
#[derive(Clone)]
pub enum AlertAccessory {
    /// A compact symbolic icon with a mandatory logical name.
    Icon {
        /// Logical name retained for future native accessibility publication.
        accessibility_name: SharedString,
        /// Optional caller-owned image rendered inside the bounded accessory slot.
        image: Option<Arc<RenderImage>>,
    },
    /// Bounded noninteractive media with a mandatory logical name.
    Media {
        /// Logical name retained for future native accessibility publication.
        accessibility_name: SharedString,
        /// Caller-owned image rendered inside the bounded accessory slot.
        image: Arc<RenderImage>,
    },
}

impl AlertAccessory {
    /// Creates a semantic icon slot. A missing image uses the renderer's restrained generic mark.
    pub fn icon(
        accessibility_name: impl Into<SharedString>,
        image: Option<Arc<RenderImage>>,
    ) -> Self {
        Self::Icon {
            accessibility_name: accessibility_name.into(),
            image,
        }
    }

    /// Creates a bounded noninteractive image slot.
    pub fn media(accessibility_name: impl Into<SharedString>, image: Arc<RenderImage>) -> Self {
        Self::Media {
            accessibility_name: accessibility_name.into(),
            image,
        }
    }

    pub(super) fn accessibility_name(&self) -> &str {
        match self {
            Self::Icon {
                accessibility_name, ..
            }
            | Self::Media {
                accessibility_name, ..
            } => accessibility_name.as_ref(),
        }
    }
}

/// Caller-owned suppression choice whose selected value is returned but never persisted by UI.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AlertSuppression {
    label: SharedString,
    selected: bool,
}

impl AlertSuppression {
    /// Creates a suppression choice with its initial selected state.
    pub fn new(label: impl Into<SharedString>, selected: bool) -> Self {
        Self {
            label: label.into(),
            selected,
        }
    }

    /// Returns the localized visible label.
    pub fn label(&self) -> &str {
        self.label.as_ref()
    }

    /// Returns the initial caller-owned choice value.
    pub const fn is_selected(&self) -> bool {
        self.selected
    }
}

/// Typed terminal result delivered exactly once for an Alert presentation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AlertOutcome<A> {
    /// An enabled typed decision or Help action was activated.
    Activated {
        /// Stable caller-owned action identity.
        action_id: A,
        /// Content-free activation path.
        source: ModalActivationSource,
        /// Final suppression choice value, when configured.
        suppression_selected: Option<bool>,
    },
    /// The presentation ended without activating a caller action.
    Dismissed {
        /// Authoritative content-free terminal reason.
        reason: ModalCloseReason,
        /// Final suppression choice value, when configured.
        suppression_selected: Option<bool>,
    },
}

/// A concise, window-modal desktop decision or acknowledgement.
///
/// Alert accepts one to three decision actions plus an optional separate Help action. It never
/// dismisses from an outside press. Initial focus prefers an explicit default or sole ordinary
/// acknowledgement, but a destructive Alert enters on the enabled safe Cancel path. Return uses
/// only an explicit enabled default; Escape and an installed platform cancellation equivalent use
/// only the enabled Cancel action.
/// Caller action order is preserved as logical identity while [`ModalDesktopPolicy`] owns physical
/// placement and the renderer traverses the complete current-frame GPUI tab-stop order.
///
/// Presentations share the Operating-System Window's one active slot and eight-entry waiting FIFO.
/// Results and lifecycle closure are delivered exactly once, including queued dismissal, caller
/// owner removal, Operating-System Window removal, and reentrant result callbacks.
#[derive(Clone)]
pub struct Alert<A> {
    pub(super) id: ModalId,
    pub(super) accessibility_title: SharedString,
    pub(super) title: SharedString,
    pub(super) message: SharedString,
    pub(super) intent: AlertIntent,
    pub(super) actions: Vec<ModalAction<A>>,
    pub(super) detail: Option<SharedString>,
    pub(super) accessory: Option<AlertAccessory>,
    pub(super) help: Option<ModalAction<A>>,
    pub(super) suppression: Option<AlertSuppression>,
}

impl<A> Alert<A> {
    /// Creates an Alert with mandatory logical and visible content plus one to three decisions.
    pub fn new(
        id: ModalId,
        accessibility_title: impl Into<SharedString>,
        title: impl Into<SharedString>,
        message: impl Into<SharedString>,
        actions: Vec<ModalAction<A>>,
    ) -> Self {
        Self {
            id,
            accessibility_title: accessibility_title.into(),
            title: title.into(),
            message: message.into(),
            intent: AlertIntent::Informational,
            actions,
            detail: None,
            accessory: None,
            help: None,
            suppression: None,
        }
    }

    /// Sets the semantic severity without accepting caller paint.
    pub fn intent(mut self, intent: AlertIntent) -> Self {
        self.intent = intent;
        self
    }

    /// Sets bounded secondary detail.
    pub fn detail(mut self, detail: impl Into<SharedString>) -> Self {
        self.detail = Some(detail.into());
        self
    }

    /// Sets the single bounded noninteractive accessory.
    pub fn accessory(mut self, accessory: AlertAccessory) -> Self {
        self.accessory = Some(accessory);
        self
    }

    /// Sets a separately rendered Help action.
    pub fn help_action(mut self, help: ModalAction<A>) -> Self {
        self.help = Some(help);
        self
    }

    /// Sets the caller-owned suppression choice.
    pub fn suppression(mut self, suppression: AlertSuppression) -> Self {
        self.suppression = Some(suppression);
        self
    }

    /// Validates this configuration without presenting or changing caller order.
    pub fn validate(&self, policy: &ModalDesktopPolicy) -> Result<(), ModalValidationError>
    where
        A: Eq,
    {
        policy.validate_alert(self)
    }

    /// Presents this Alert through the shared per-window owner and bounded FIFO queue.
    ///
    /// The callback runs after the private owner reducer releases its GPUI entity update, so it may
    /// reentrantly present another modal. It receives exactly one typed terminal outcome.
    ///
    /// # Errors
    ///
    /// Returns [`ModalPresentationError`] when configuration is invalid, desktop policy is not
    /// installed, the Operating-System Window is unavailable, or eight requests already wait.
    pub fn present<T: 'static>(
        self,
        window: &Window,
        cx: &mut Context<T>,
        on_result: impl FnOnce(AlertOutcome<A>, &mut App) + 'static,
    ) -> Result<ModalPresentationHandle, ModalPresentationError>
    where
        A: Clone + Eq + 'static,
    {
        self.present_with_lifecycle(window, cx, on_result, |_, _| {})
    }

    /// Presents this Alert and observes its public lifecycle after each owner update is released.
    ///
    /// Lifecycle order is `Opened`, optional `ActionRequested`/`Pending` for applicable facades,
    /// `Closing`, then `Closed`. Alert normally emits `Opened`, `Closing`, and `Closed`; a queued
    /// Alert dismissed before opening emits only `Closed`. Result delivery occurs between
    /// `Closing` and `Closed`, and every callback runs outside the owner update.
    ///
    /// # Errors
    ///
    /// Returns [`ModalPresentationError`] under the same conditions as [`Self::present`].
    pub fn present_with_lifecycle<T: 'static>(
        self,
        window: &Window,
        cx: &mut Context<T>,
        on_result: impl FnOnce(AlertOutcome<A>, &mut App) + 'static,
        on_lifecycle: impl Fn(&ModalLifecycleEvent, &mut App) + 'static,
    ) -> Result<ModalPresentationHandle, ModalPresentationError>
    where
        A: Clone + Eq + 'static,
    {
        self.present_with_operation(
            window,
            cx,
            on_result,
            on_lifecycle,
            super::ModalPresentationOperation::Present,
        )
    }

    /// Replaces the currently visible modal with this Alert in one owner transition.
    ///
    /// The predecessor settles exactly once with [`ModalCloseReason::Replaced`] before this
    /// presentation emits `Opened`. Existing queued requests retain their FIFO order behind this
    /// replacement, and the underlay remains continuously inert. Predecessor dismissal,
    /// completion, cancellation, and update authority becomes stale before callbacks run.
    ///
    /// # Errors
    ///
    /// Returns [`ModalPresentationError`] when configuration is invalid, desktop policy is not
    /// installed, the Operating-System Window is unavailable, or no modal is currently visible.
    pub fn replace_active<T: 'static>(
        self,
        window: &Window,
        cx: &mut Context<T>,
        on_result: impl FnOnce(AlertOutcome<A>, &mut App) + 'static,
        on_lifecycle: impl Fn(&ModalLifecycleEvent, &mut App) + 'static,
    ) -> Result<ModalPresentationHandle, ModalPresentationError>
    where
        A: Clone + Eq + 'static,
    {
        self.present_with_operation(
            window,
            cx,
            on_result,
            on_lifecycle,
            super::ModalPresentationOperation::ReplaceActive,
        )
    }

    fn present_with_operation<T: 'static>(
        self,
        window: &Window,
        cx: &mut Context<T>,
        on_result: impl FnOnce(AlertOutcome<A>, &mut App) + 'static,
        on_lifecycle: impl Fn(&ModalLifecycleEvent, &mut App) + 'static,
        operation: super::ModalPresentationOperation,
    ) -> Result<ModalPresentationHandle, ModalPresentationError>
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
        let initial_focus = policy
            .alert_initial_focus(&self.actions)
            .and_then(|focus| match focus {
                super::ModalInitialFocus::Action(index) => Some(index),
                super::ModalInitialFocus::Surface => None,
            })
            .ok_or(ModalValidationError::MissingSafeDismissal)?;
        let suppression = self
            .suppression
            .as_ref()
            .map(|suppression| (suppression.label.clone(), suppression.is_selected()));
        let suppression_flag = suppression
            .as_ref()
            .map(|(_, selected)| Rc::new(Cell::new(*selected)));
        let result_suppression_flag = suppression_flag.clone();
        let mut actions = self.actions;
        if let Some(help) = self.help {
            actions.push(help);
        }
        let action_ids = actions
            .iter()
            .map(|action| action.id().clone())
            .collect::<Vec<_>>();
        let request = PreparedModalRequest::new(
            self.id,
            ModalKind::Alert,
            PreparedModalRequest::erase_actions(actions),
            PreparedModalSemantics::Alert {
                accessibility_title: self.accessibility_title,
                visible_title: self.title,
                message: self.message,
                detail: self.detail,
                intent: self.intent,
                accessory: self.accessory,
                suppression,
                default_action,
                cancel_action,
            },
            PreparedFocusIntent::Action(initial_focus),
            cx.weak_entity().into(),
            Box::new(move |outcome, cx| {
                let outcome = match outcome {
                    InternalOutcome::Activated {
                        action_index,
                        source,
                    } => {
                        let Some(action_id) = action_ids.get(action_index).cloned() else {
                            return;
                        };
                        AlertOutcome::Activated {
                            action_id,
                            source,
                            suppression_selected: result_suppression_flag
                                .as_ref()
                                .map(|flag| flag.get()),
                        }
                    }
                    InternalOutcome::Dismissed(reason) => AlertOutcome::Dismissed {
                        reason,
                        suppression_selected: result_suppression_flag
                            .as_ref()
                            .map(|flag| flag.get()),
                    },
                    InternalOutcome::DialogProgrammaticCompletion
                    | InternalOutcome::Progress(_) => return,
                };
                on_result(outcome, cx);
            }),
        )
        .with_suppression_flag(suppression_flag)
        .with_lifecycle(Some(Rc::new(on_lifecycle)));
        operation.apply(request, window, cx)
    }
}

pub(super) fn validate_accessory(accessory: &AlertAccessory) -> Result<(), ModalValidationError> {
    let name = accessory.accessibility_name();
    super::validate_required_text(name, super::ModalTextField::AccessibilityTitle)?;
    super::validate_bounded_text(
        name,
        super::ModalTextField::AccessibilityTitle,
        MAX_ALERT_ACCESSORY_NAME_CHARACTERS,
    )
}

pub(super) fn validate_suppression(
    suppression: &AlertSuppression,
) -> Result<(), ModalValidationError> {
    super::validate_required_text(suppression.label(), super::ModalTextField::Detail)?;
    super::validate_bounded_text(
        suppression.label(),
        super::ModalTextField::Detail,
        MAX_ALERT_SUPPRESSION_LABEL_CHARACTERS,
    )
}
