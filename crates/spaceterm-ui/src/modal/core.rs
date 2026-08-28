use std::{
    cell::Cell,
    collections::{HashMap, VecDeque},
    rc::Rc,
    time::Duration,
};

use gpui::{
    AnyView, AnyWeakEntity, App, AppContext, BorrowAppContext, Context, Entity, EntityId,
    FocusHandle, Global, IntoElement, Render, SharedString, Subscription, WeakEntity,
    WeakFocusHandle, Window, WindowId,
};

use crate::button::ModalPressOwner;

use super::{
    AlertAccessory, AlertIntent, DialogCloseDecision, DialogSize, ModalAction, ModalActionEmphasis,
    ModalActionIntent, ModalActionRole, ModalActivationSource, ModalCloseReason,
    ModalDesktopPolicy, ModalDismissalError, ModalId, ModalLifecycleEvent, ModalParentToken,
    ModalPresentationError, ModalPresentationId, ModalStaleGenerationError,
    ModalTerminalOutcomeError, ModalUpdateError, ProgressCancelDecision, ProgressDialogOutcome,
    ProgressDialogUpdate, ProgressState,
    policy::{is_cancel_capability, is_safe_cancel},
};

const MAX_QUEUED_REQUESTS: usize = 8;

type ResultEffect = Box<dyn FnOnce(&mut App)>;
pub(super) type LifecycleHandler = Rc<dyn Fn(&ModalLifecycleEvent, &mut App)>;
pub(super) type ResultSink = Box<dyn FnOnce(InternalOutcome, &mut App)>;
pub(super) type DialogActionHandler = Rc<
    dyn Fn(
        usize,
        ModalActivationSource,
        ModalPresentationId,
        DialogPendingCompletion,
        &mut App,
    ) -> DialogCloseDecision,
>;
pub(super) type ProgressCancelHandler = Rc<
    dyn Fn(
        ModalActivationSource,
        ProgressCancellationCompletion,
        &mut App,
    ) -> ProgressCancelDecision,
>;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ModalKind {
    Alert,
    Dialog,
    Progress,
}

pub(super) struct ErasedAction {
    label: SharedString,
    role: ModalActionRole,
    intent: ModalActionIntent,
    emphasis: ModalActionEmphasis,
    enabled: bool,
    is_default: bool,
    debug_identity: SharedString,
}

impl ErasedAction {
    fn from_action<A>(action: ModalAction<A>) -> Self {
        Self {
            label: action.label,
            role: action.role,
            intent: action.intent,
            emphasis: action.emphasis,
            enabled: action.enabled,
            is_default: action.is_default,
            debug_identity: action.debug_identity,
        }
    }
}

fn progress_cancellation_action_index(actions: &[ErasedAction]) -> Option<usize> {
    actions
        .iter()
        .position(|action| is_cancel_capability(action.role, action.intent))
}

#[derive(Clone, PartialEq)]
pub(super) enum PreparedFocusIntent {
    Action(usize),
    Body(FocusHandle),
    Surface,
}

#[derive(Clone)]
pub(super) enum PreparedModalSemantics {
    Alert {
        accessibility_title: SharedString,
        visible_title: SharedString,
        message: SharedString,
        detail: Option<SharedString>,
        intent: AlertIntent,
        accessory: Option<AlertAccessory>,
        suppression: Option<(SharedString, bool)>,
        default_action: Option<usize>,
        cancel_action: Option<usize>,
    },
    Dialog {
        accessibility_title: SharedString,
        visible_title: SharedString,
        description: Option<SharedString>,
        default_action: Option<usize>,
        cancel_action: Option<usize>,
    },
    Progress {
        accessibility_title: SharedString,
        visible_title: SharedString,
        status: SharedString,
        detail: Option<SharedString>,
        progress: ProgressState,
        cancellation_capable: bool,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CompletionStatus {
    Pending,
    Closed,
    OwnerRemoved,
    Replaced,
}

#[derive(Clone)]
struct CompletionFlag(Rc<Cell<CompletionStatus>>);

impl CompletionFlag {
    fn new() -> Self {
        Self(Rc::new(Cell::new(CompletionStatus::Pending)))
    }

    fn status(&self) -> CompletionStatus {
        self.0.get()
    }

    fn finish(&self, reason: ModalCloseReason) -> bool {
        if self.status() != CompletionStatus::Pending {
            return false;
        }
        let status = match reason {
            ModalCloseReason::OwnerRemoved => CompletionStatus::OwnerRemoved,
            ModalCloseReason::Replaced => CompletionStatus::Replaced,
            ModalCloseReason::Action
            | ModalCloseReason::Cancelled
            | ModalCloseReason::Programmatic
            | ModalCloseReason::DeadlineExpired => CompletionStatus::Closed,
        };
        self.0.set(status);
        true
    }
}

pub(super) struct PreparedModalRequest {
    id: ModalId,
    kind: ModalKind,
    actions: Vec<ErasedAction>,
    semantics: PreparedModalSemantics,
    focus_intent: PreparedFocusIntent,
    caller_owner: AnyWeakEntity,
    result_sink: Option<ResultSink>,
    lifecycle: Option<LifecycleHandler>,
    dialog_action: Option<DialogActionHandler>,
    progress_cancel: Option<ProgressCancelHandler>,
    programmatic_deadline: Option<Duration>,
    body: Option<AnyView>,
    dialog_size: DialogSize,
    suppression_flag: Option<Rc<Cell<bool>>>,
    completion: CompletionFlag,
    _caller_release: Option<Subscription>,
}

impl PreparedModalRequest {
    pub(super) fn new(
        id: ModalId,
        kind: ModalKind,
        actions: Vec<ErasedAction>,
        semantics: PreparedModalSemantics,
        focus_intent: PreparedFocusIntent,
        caller_owner: AnyWeakEntity,
        result_sink: ResultSink,
    ) -> Self {
        Self {
            id,
            kind,
            actions,
            semantics,
            focus_intent,
            caller_owner,
            result_sink: Some(result_sink),
            lifecycle: None,
            dialog_action: None,
            progress_cancel: None,
            programmatic_deadline: None,
            body: None,
            dialog_size: DialogSize::Regular,
            suppression_flag: None,
            completion: CompletionFlag::new(),
            _caller_release: None,
        }
    }

    pub(super) fn with_lifecycle(mut self, lifecycle: Option<LifecycleHandler>) -> Self {
        self.lifecycle = lifecycle;
        self
    }

    pub(super) fn with_dialog_action(mut self, handler: DialogActionHandler) -> Self {
        self.dialog_action = Some(handler);
        self
    }

    pub(super) fn with_progress_cancel(mut self, handler: ProgressCancelHandler) -> Self {
        self.progress_cancel = Some(handler);
        self
    }

    pub(super) fn with_programmatic_deadline(mut self, deadline: Option<Duration>) -> Self {
        self.programmatic_deadline = deadline;
        self
    }

    pub(super) fn with_body(mut self, body: Option<AnyView>) -> Self {
        self.body = body;
        self
    }

    pub(super) fn with_dialog_size(mut self, size: DialogSize) -> Self {
        self.dialog_size = size;
        self
    }

    pub(super) fn with_suppression_flag(mut self, flag: Option<Rc<Cell<bool>>>) -> Self {
        self.suppression_flag = flag;
        self
    }

    pub(super) fn erase_actions<A>(actions: Vec<ModalAction<A>>) -> Vec<ErasedAction> {
        actions.into_iter().map(ErasedAction::from_action).collect()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DialogAttemptPhase {
    ActionRequested,
    Pending,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct DialogAttempt {
    attempt: u64,
    action_index: usize,
    source: ModalActivationSource,
    phase: DialogAttemptPhase,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct DialogPendingState {
    primary: Option<DialogAttempt>,
    cancel: Option<DialogAttempt>,
}

impl DialogPendingState {
    fn attempt(&self, attempt: u64) -> Option<DialogAttempt> {
        self.primary
            .filter(|candidate| candidate.attempt == attempt)
            .or_else(|| self.cancel.filter(|candidate| candidate.attempt == attempt))
    }

    fn attempt_mut(&mut self, attempt: u64) -> Option<&mut DialogAttempt> {
        if self
            .primary
            .is_some_and(|candidate| candidate.attempt == attempt)
        {
            return self.primary.as_mut();
        }
        self.cancel
            .as_mut()
            .filter(|candidate| candidate.attempt == attempt)
    }

    fn remove(&mut self, attempt: u64) {
        if self
            .primary
            .is_some_and(|candidate| candidate.attempt == attempt)
        {
            self.primary = None;
        } else if self
            .cancel
            .is_some_and(|candidate| candidate.attempt == attempt)
        {
            self.cancel = None;
        }
    }

    fn has_authority(&self) -> bool {
        self.primary.is_some() || self.cancel.is_some()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum RuntimeState {
    Open,
    DialogPending(DialogPendingState),
    ProgressActionRequested {
        attempt: u64,
        source: ModalActivationSource,
    },
    ProgressPending {
        attempt: u64,
        source: ModalActivationSource,
    },
    Closing,
    Closed,
}

#[derive(Clone, Debug)]
pub(super) struct ProgressRuntime {
    pub(super) status: SharedString,
    pub(super) detail: Option<SharedString>,
    pub(super) progress: ProgressState,
    pub(super) cancellation_capable: bool,
    pub(super) cancellation_enabled: bool,
}

impl ProgressRuntime {
    fn from_request(request: &PreparedModalRequest) -> Option<Self> {
        match &request.semantics {
            PreparedModalSemantics::Progress {
                status,
                detail,
                progress,
                cancellation_capable,
                ..
            } => Some(Self {
                status: status.clone(),
                detail: detail.clone(),
                progress: *progress,
                cancellation_capable: *cancellation_capable,
                cancellation_enabled: *cancellation_capable
                    && progress_cancellation_action_index(&request.actions)
                        .is_some_and(|index| request.actions[index].enabled),
            }),
            PreparedModalSemantics::Alert { .. } | PreparedModalSemantics::Dialog { .. } => None,
        }
    }

    fn cancellation_available(&self) -> bool {
        self.cancellation_capable && self.cancellation_enabled
    }

    fn apply(&mut self, update: ProgressDialogUpdate) {
        if let Some(status) = update.status {
            self.status = status;
        }
        if let Some(detail) = update.detail {
            self.detail = detail;
        }
        if let Some(value) = update.progress {
            self.progress = value;
        }
        if let Some(enabled) = update.cancellation_enabled {
            self.cancellation_enabled = enabled;
        }
    }
}

struct ActivePresentation {
    id: ModalPresentationId,
    request: PreparedModalRequest,
    state: RuntimeState,
    close_attempt_generation: u64,
    update_generation: u64,
    focus_request_generation: u64,
    successor_focus: Option<FocusHandle>,
    progress: Option<ProgressRuntime>,
}

impl ActivePresentation {
    fn new(id: ModalPresentationId, request: PreparedModalRequest) -> Self {
        let progress = ProgressRuntime::from_request(&request);
        Self::from_parts(id, request, 0, progress)
    }

    fn from_queued(queued: QueuedPresentation) -> Self {
        Self::from_parts(
            queued.id,
            queued.request,
            queued.update_generation,
            queued.progress,
        )
    }

    fn from_parts(
        id: ModalPresentationId,
        request: PreparedModalRequest,
        update_generation: u64,
        progress: Option<ProgressRuntime>,
    ) -> Self {
        Self {
            id,
            request,
            state: RuntimeState::Open,
            close_attempt_generation: 0,
            update_generation,
            focus_request_generation: 0,
            successor_focus: None,
            progress,
        }
    }

    fn progress_cancellation_action_index(&self) -> Option<usize> {
        let progress = self.progress.as_ref()?;
        if !progress.cancellation_capable {
            return None;
        }
        progress_cancellation_action_index(&self.request.actions)
    }

    fn current_focus_intent(&self) -> PreparedFocusIntent {
        if self.request.kind == ModalKind::Progress {
            return self
                .progress
                .as_ref()
                .filter(|progress| progress.cancellation_available())
                .and_then(|_| self.progress_cancellation_action_index())
                .map(PreparedFocusIntent::Action)
                .unwrap_or(PreparedFocusIntent::Surface);
        }
        self.request.focus_intent.clone()
    }

    fn may_request_action(&self, action_index: usize) -> bool {
        let Some(action) = self.request.actions.get(action_index) else {
            return false;
        };
        if self.request.kind == ModalKind::Progress {
            return matches!(self.state, RuntimeState::Open)
                && self.progress_cancellation_action_index() == Some(action_index)
                && self
                    .progress
                    .as_ref()
                    .is_some_and(ProgressRuntime::cancellation_available);
        }
        if !action.enabled
            || (action.role == ModalActionRole::Cancel
                && !is_safe_cancel(action.role, action.intent, action.enabled))
        {
            return false;
        }
        match (&self.request.kind, &self.state) {
            (ModalKind::Alert, RuntimeState::Open) => true,
            (ModalKind::Dialog, RuntimeState::Open) => true,
            (ModalKind::Dialog, RuntimeState::DialogPending(pending)) => {
                let primary_is_pending_non_cancel = pending.primary.is_some_and(|primary| {
                    primary.phase == DialogAttemptPhase::Pending
                        && self
                            .request
                            .actions
                            .get(primary.action_index)
                            .is_some_and(|action| action.role != ModalActionRole::Cancel)
                });
                action.role == ModalActionRole::Cancel
                    && primary_is_pending_non_cancel
                    && pending.cancel.is_none()
            }
            (ModalKind::Progress, _) => false,
            _ => false,
        }
    }

    fn lifecycle_effect(&self, event: ModalLifecycleEvent) -> Option<ResultEffect> {
        let handler = self.request.lifecycle.clone()?;
        Some(Box::new(move |cx| handler(&event, cx)))
    }
}

pub(super) enum InternalOutcome {
    Activated {
        action_index: usize,
        source: ModalActivationSource,
    },
    DialogProgrammaticCompletion,
    Dismissed(ModalCloseReason),
    Progress(ProgressDialogOutcome),
}

struct QueuedPresentation {
    id: ModalPresentationId,
    progress: Option<ProgressRuntime>,
    update_generation: u64,
    request: PreparedModalRequest,
}

impl QueuedPresentation {
    fn new(id: ModalPresentationId, request: PreparedModalRequest) -> Self {
        let progress = ProgressRuntime::from_request(&request);
        Self {
            id,
            progress,
            update_generation: 0,
            request,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum SettlementState {
    #[default]
    Idle,
    Settling,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum EffectPumpState {
    #[default]
    Idle,
    Scheduled,
    Running,
}

struct DeferredEffectBatch {
    effects: VecDeque<ResultEffect>,
    completes_settlement: bool,
}

impl DeferredEffectBatch {
    fn new(effects: Vec<ResultEffect>, completes_settlement: bool) -> Self {
        Self {
            effects: effects.into(),
            completes_settlement,
        }
    }
}

#[derive(Default)]
struct FocusChain {
    generation: Option<ModalPresentationId>,
    predecessor: Option<WeakFocusHandle>,
    successor: Option<FocusHandle>,
    root_scope: Option<WeakFocusHandle>,
    modal_scope: Option<WeakFocusHandle>,
    retired_owned_transient: Option<WeakFocusHandle>,
    restoration_pending: bool,
}

#[derive(Clone, Debug)]
pub(super) struct ModalRenderAction {
    pub(super) label: SharedString,
    pub(super) role: ModalActionRole,
    pub(super) intent: ModalActionIntent,
    pub(super) emphasis: ModalActionEmphasis,
    pub(super) enabled: bool,
    pub(super) is_default: bool,
    pub(super) debug_identity: SharedString,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum LogicalModalRole {
    Alert,
    Dialog,
    Progress,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct LogicalActionSemanticSnapshot {
    name: SharedString,
    role: ModalActionRole,
    intent: ModalActionIntent,
    emphasis: ModalActionEmphasis,
    enabled: bool,
    is_default: bool,
    debug_identity: SharedString,
}

#[derive(Clone, Debug, PartialEq)]
struct LogicalProgressSemanticSnapshot {
    status: SharedString,
    value: Option<f32>,
    indeterminate: bool,
    cancellation_available: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum LogicalFocusEntry {
    Action(SharedString),
    Body,
    Surface,
}

/// Logical facts retained because GPUI 0.2.2 cannot publish them as native accessibility nodes.
///
/// This private value and the renderer's debug selectors support deterministic tests only. They
/// are not VoiceOver, Narrator, Orca, or native accessibility-tree evidence.
#[derive(Clone, Debug, PartialEq)]
struct LogicalModalSemanticSnapshot {
    id: ModalId,
    role: LogicalModalRole,
    modal: bool,
    accessibility_title: SharedString,
    visible_title: SharedString,
    description: Option<SharedString>,
    secondary_detail: Option<SharedString>,
    alert_intent: Option<AlertIntent>,
    accessory_name: Option<SharedString>,
    suppression_label: Option<SharedString>,
    actions: Vec<LogicalActionSemanticSnapshot>,
    default_action: Option<SharedString>,
    cancel_action: Option<SharedString>,
    progress: Option<LogicalProgressSemanticSnapshot>,
    focus_entry: LogicalFocusEntry,
    focus_contained: bool,
    underlay_excluded: bool,
}

impl ActivePresentation {
    fn logical_semantic_snapshot(&self) -> LogicalModalSemanticSnapshot {
        let (
            role,
            accessibility_title,
            visible_title,
            description,
            secondary_detail,
            alert_intent,
            accessory_name,
            suppression_label,
            progress,
        ) = match (&self.request.semantics, &self.progress) {
            (
                PreparedModalSemantics::Alert {
                    accessibility_title,
                    visible_title,
                    message,
                    detail,
                    intent,
                    accessory,
                    suppression,
                    ..
                },
                _,
            ) => (
                LogicalModalRole::Alert,
                accessibility_title.clone(),
                visible_title.clone(),
                Some(message.clone()),
                detail.clone(),
                Some(*intent),
                accessory.as_ref().map(|accessory| match accessory {
                    AlertAccessory::Icon {
                        accessibility_name, ..
                    }
                    | AlertAccessory::Media {
                        accessibility_name, ..
                    } => accessibility_name.clone(),
                }),
                suppression.as_ref().map(|(label, _)| label.clone()),
                None,
            ),
            (
                PreparedModalSemantics::Dialog {
                    accessibility_title,
                    visible_title,
                    description,
                    ..
                },
                _,
            ) => (
                LogicalModalRole::Dialog,
                accessibility_title.clone(),
                visible_title.clone(),
                description.clone(),
                None,
                None,
                None,
                None,
                None,
            ),
            (
                PreparedModalSemantics::Progress {
                    accessibility_title,
                    visible_title,
                    ..
                },
                Some(progress),
            ) => {
                let (value, indeterminate) = match progress.progress {
                    ProgressState::Determinate(value) => (Some(value.value()), false),
                    ProgressState::Indeterminate => (None, true),
                };
                (
                    LogicalModalRole::Progress,
                    accessibility_title.clone(),
                    visible_title.clone(),
                    Some(progress.status.clone()),
                    progress.detail.clone(),
                    None,
                    None,
                    None,
                    Some(LogicalProgressSemanticSnapshot {
                        status: progress.status.clone(),
                        value,
                        indeterminate,
                        cancellation_available: progress.cancellation_available(),
                    }),
                )
            }
            (PreparedModalSemantics::Progress { .. }, None) => unreachable!(
                "validated ProgressDialog presentations always own progress runtime state"
            ),
        };
        let actions = self
            .request
            .actions
            .iter()
            .enumerate()
            .map(|(index, action)| LogicalActionSemanticSnapshot {
                name: action.label.clone(),
                role: action.role,
                intent: action.intent,
                emphasis: action.emphasis,
                enabled: self.may_request_action(index),
                is_default: action.is_default,
                debug_identity: action.debug_identity.clone(),
            })
            .collect::<Vec<_>>();
        let action_identity = |index: usize| {
            actions
                .get(index)
                .map(|action| action.debug_identity.clone())
        };
        let (default_index, cancel_index) = match &self.request.semantics {
            PreparedModalSemantics::Alert {
                default_action,
                cancel_action,
                ..
            }
            | PreparedModalSemantics::Dialog {
                default_action,
                cancel_action,
                ..
            } => (*default_action, *cancel_action),
            PreparedModalSemantics::Progress { .. } => {
                (None, self.progress_cancellation_action_index())
            }
        };
        let default_action = default_index.and_then(action_identity);
        let cancel_action = cancel_index.and_then(action_identity);
        let focus_intent = self.current_focus_intent();
        let focus_entry = match &focus_intent {
            PreparedFocusIntent::Action(index) => action_identity(*index)
                .map(LogicalFocusEntry::Action)
                .unwrap_or(LogicalFocusEntry::Surface),
            PreparedFocusIntent::Body(_) => LogicalFocusEntry::Body,
            PreparedFocusIntent::Surface => LogicalFocusEntry::Surface,
        };
        LogicalModalSemanticSnapshot {
            id: self.request.id.clone(),
            role,
            modal: true,
            accessibility_title,
            visible_title,
            description,
            secondary_detail,
            alert_intent,
            accessory_name,
            suppression_label,
            actions,
            default_action,
            cancel_action,
            progress,
            focus_entry,
            focus_contained: true,
            underlay_excluded: true,
        }
    }
}

#[derive(Clone)]
pub(super) struct ModalRenderSnapshot {
    pub(super) id: ModalId,
    pub(super) presentation: ModalPresentationId,
    pub(super) kind: ModalKind,
    pub(super) semantics: PreparedModalSemantics,
    pub(super) actions: Vec<ModalRenderAction>,
    pub(super) focus_intent: PreparedFocusIntent,
    pub(super) focus_request_generation: u64,
    pub(super) interaction_enabled: bool,
    pub(super) body: Option<AnyView>,
    pub(super) dialog_size: DialogSize,
    pub(super) default_action: Option<usize>,
    pub(super) cancel_action: Option<usize>,
    pub(super) progress: Option<ProgressRuntime>,
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "GPUI 0.2.2 cannot publish the retained logical accessibility snapshot"
        )
    )]
    semantic_snapshot: LogicalModalSemanticSnapshot,
}

pub(super) struct ModalWindowOwner {
    window_id: WindowId,
    next_presentation_generation: u64,
    active: Option<ActivePresentation>,
    queue: VecDeque<QueuedPresentation>,
    settlement: SettlementState,
    effect_pump: EffectPumpState,
    deferred_effects: VecDeque<DeferredEffectBatch>,
    reentrant_effects: VecDeque<DeferredEffectBatch>,
    focus_chain: FocusChain,
    window_available: bool,
    palette_suspension: Option<crate::command_palette::CommandPaletteSuspension>,
    transients_active: bool,
    press_owner: ModalPressOwner,
    #[cfg(test)]
    caller_release_callbacks: usize,
}

impl ModalWindowOwner {
    fn new(window_id: WindowId, cx: &mut Context<Self>) -> Self {
        let entity_id = cx.entity().entity_id();
        cx.on_release(move |state, cx| {
            state.press_owner.disarm(cx);
            remove_coordinator_owner(window_id, entity_id, cx);
            if state.transients_active {
                crate::tooltip::set_window_tooltip_suppression(
                    window_id,
                    crate::tooltip::TooltipSuppression::Modal,
                    false,
                    cx,
                );
                state.transients_active = false;
            }
            if let Some(suspension) = state.palette_suspension.take() {
                crate::command_palette::discard_window_command_palette_suspension(suspension, cx);
            }
            if let Some(parent) = state.active_modal_parent() {
                crate::menu::dismiss_menu_owned_by_modal_parent(parent, cx);
            }
            let effects = state.drain_all(ModalCloseReason::OwnerRemoved);
            defer_released_owner_effects(effects, cx);
        })
        .detach();
        Self {
            window_id,
            next_presentation_generation: 0,
            active: None,
            queue: VecDeque::new(),
            settlement: SettlementState::Idle,
            effect_pump: EffectPumpState::Idle,
            deferred_effects: VecDeque::new(),
            reentrant_effects: VecDeque::new(),
            focus_chain: FocusChain::default(),
            window_available: true,
            palette_suspension: None,
            transients_active: false,
            press_owner: ModalPressOwner::default(),
            #[cfg(test)]
            caller_release_callbacks: 0,
        }
    }

    #[cfg(test)]
    fn new_for_test(window_id: WindowId) -> Self {
        Self {
            window_id,
            next_presentation_generation: 0,
            active: None,
            queue: VecDeque::new(),
            settlement: SettlementState::Idle,
            effect_pump: EffectPumpState::Idle,
            deferred_effects: VecDeque::new(),
            reentrant_effects: VecDeque::new(),
            focus_chain: FocusChain::default(),
            window_available: true,
            palette_suspension: None,
            transients_active: false,
            press_owner: ModalPressOwner::default(),
            caller_release_callbacks: 0,
        }
    }

    pub(super) fn press_owner(&self) -> ModalPressOwner {
        self.press_owner.clone()
    }

    fn begin_transient_coordination(
        &mut self,
        predecessor: Option<WeakFocusHandle>,
        palette_suspension: crate::command_palette::CommandPaletteSuspension,
    ) {
        if self.active.is_none() && self.settlement == SettlementState::Idle {
            self.focus_chain.predecessor = predecessor;
            self.focus_chain.successor = None;
            self.palette_suspension = Some(palette_suspension);
            self.transients_active = true;
        }
    }

    fn finish_transient_coordination(
        &mut self,
    ) -> Option<crate::command_palette::CommandPaletteSuspension> {
        if self.active.is_some()
            || self.settlement == SettlementState::Settling
            || !self.transients_active
        {
            return None;
        }
        self.transients_active = false;
        self.palette_suspension.take()
    }

    fn active_modal_parent(&self) -> Option<ModalParentToken> {
        Some(ModalParentToken {
            window_id: self.window_id,
            presentation: self.active.as_ref()?.id,
        })
    }

    fn active_modal_parent_for(
        &self,
        presentation: ModalPresentationId,
    ) -> Option<ModalParentToken> {
        self.active
            .as_ref()
            .is_some_and(|active| active.id == presentation)
            .then_some(ModalParentToken {
                window_id: self.window_id,
                presentation,
            })
    }

    fn next_presentation_id(&mut self) -> ModalPresentationId {
        self.next_presentation_generation = self.next_presentation_generation.saturating_add(1);
        ModalPresentationId::from_generation(self.next_presentation_generation)
    }

    fn begin_settlement(&mut self) -> bool {
        if self.settlement == SettlementState::Settling {
            return false;
        }
        self.settlement = SettlementState::Settling;
        true
    }

    fn finish_settlement(&mut self) {
        self.settlement = SettlementState::Idle;
    }

    fn queue_is_full(&self) -> bool {
        let reserved_head =
            usize::from(self.active.is_none() && self.settlement == SettlementState::Settling);
        self.queue.len() >= MAX_QUEUED_REQUESTS + reserved_head
    }

    fn submit(
        &mut self,
        request: PreparedModalRequest,
        owner: WeakEntity<Self>,
    ) -> Result<(ModalPresentationId, Vec<ResultEffect>), ModalPresentationError> {
        if !self.window_available {
            return Err(ModalPresentationError::WindowUnavailable);
        }
        let id = self.next_presentation_id();
        if self.active.is_some() || self.settlement == SettlementState::Settling {
            if self.queue_is_full() {
                return Err(ModalPresentationError::QueueFull);
            }
            self.queue.push_back(QueuedPresentation::new(id, request));
            return Ok((id, Vec::new()));
        }
        let active = ActivePresentation::new(id, request);
        let effects = opened_effects(&active, owner, self.window_id);
        self.focus_chain.generation = Some(id);
        self.active = Some(active);
        Ok((id, effects))
    }

    fn replace_active(
        &mut self,
        request: PreparedModalRequest,
        owner: WeakEntity<Self>,
    ) -> Result<(ModalPresentationId, Vec<ResultEffect>), ModalPresentationError> {
        if !self.window_available {
            return Err(ModalPresentationError::WindowUnavailable);
        }
        let Some((previous, kind)) = self
            .active
            .as_ref()
            .map(|active| (active.id, active.request.kind))
        else {
            return Err(ModalPresentationError::NoActivePresentation);
        };
        let id = self.next_presentation_id();
        let mut effects = self
            .close_active(
                previous,
                outcome_for_reason(kind, ModalCloseReason::Replaced),
                ModalCloseReason::Replaced,
            )
            .unwrap_or_default();
        let active = ActivePresentation::new(id, request);
        effects.extend(opened_effects(&active, owner, self.window_id));
        self.focus_chain.generation = Some(id);
        self.active = Some(active);
        Ok((id, effects))
    }

    fn promote(&mut self, owner: WeakEntity<Self>) -> Vec<ResultEffect> {
        if self.active.is_some() {
            return Vec::new();
        }
        let mut effects = Vec::new();
        if let Some(queued) = self.queue.pop_front() {
            let active = ActivePresentation::from_queued(queued);
            effects.extend(opened_effects(&active, owner, self.window_id));
            self.focus_chain.generation = Some(active.id);
            self.active = Some(active);
        }
        effects
    }

    fn close_active(
        &mut self,
        expected: ModalPresentationId,
        outcome: InternalOutcome,
        reason: ModalCloseReason,
    ) -> Result<Vec<ResultEffect>, ModalStaleGenerationError> {
        let current = self.active.as_ref().map(|active| active.id);
        let Some(active) = self.active.as_mut().filter(|active| active.id == expected) else {
            return Err(ModalStaleGenerationError::new(expected, current));
        };
        if matches!(active.state, RuntimeState::Closing | RuntimeState::Closed)
            || !active.request.completion.finish(reason)
        {
            return Ok(Vec::new());
        }
        active.state = RuntimeState::Closing;
        let mut effects = Vec::new();
        if let Some(effect) = active.lifecycle_effect(ModalLifecycleEvent::Closing(expected)) {
            effects.push(effect);
        }
        active.state = RuntimeState::Closed;
        let Some(mut active) = self.active.take() else {
            return Ok(effects);
        };
        if let Some(successor) = active.successor_focus.take() {
            self.focus_chain.successor = Some(successor);
        }
        self.focus_chain.restoration_pending = true;
        if let Some(effect) = completion_effect(expected, &mut active.request, outcome) {
            effects.push(effect);
        }
        if let Some(effect) = active.lifecycle_effect(ModalLifecycleEvent::Closed(expected, reason))
        {
            effects.push(effect);
        }
        Ok(effects)
    }

    fn dismiss(
        &mut self,
        presentation: ModalPresentationId,
        reason: ModalCloseReason,
    ) -> Result<Vec<ResultEffect>, ModalDismissalError> {
        if self
            .active
            .as_ref()
            .is_some_and(|active| active.id == presentation)
        {
            let Some(outcome) = self
                .active
                .as_ref()
                .map(|active| outcome_for_reason(active.request.kind, reason))
            else {
                return Err(ModalDismissalError::Closed);
            };
            return self
                .close_active(presentation, outcome, reason)
                .map_err(ModalDismissalError::Stale);
        }
        if let Some(index) = self
            .queue
            .iter()
            .position(|queued| queued.id == presentation)
        {
            let Some(mut queued) = self.queue.remove(index) else {
                return Err(ModalDismissalError::Closed);
            };
            if !queued.request.completion.finish(reason) {
                return Err(ModalDismissalError::Closed);
            }
            let outcome = outcome_for_reason(queued.request.kind, reason);
            return Ok(queued_completion_effects(
                presentation,
                &mut queued.request,
                outcome,
                reason,
            ));
        }
        Err(ModalDismissalError::Stale(ModalStaleGenerationError::new(
            presentation,
            self.active.as_ref().map(|active| active.id),
        )))
    }

    fn remove_caller(&mut self, caller: EntityId) -> Vec<ResultEffect> {
        let mut effects = Vec::new();
        if self
            .active
            .as_ref()
            .is_some_and(|active| active.request.caller_owner.entity_id() == caller)
        {
            let Some((id, kind)) = self
                .active
                .as_ref()
                .map(|active| (active.id, active.request.kind))
            else {
                return effects;
            };
            effects.extend(
                self.close_active(
                    id,
                    outcome_for_reason(kind, ModalCloseReason::OwnerRemoved),
                    ModalCloseReason::OwnerRemoved,
                )
                .unwrap_or_default(),
            );
        }
        let mut retained = VecDeque::with_capacity(self.queue.len());
        while let Some(mut queued) = self.queue.pop_front() {
            if queued.request.caller_owner.entity_id() == caller {
                let outcome =
                    outcome_for_reason(queued.request.kind, ModalCloseReason::OwnerRemoved);
                if queued
                    .request
                    .completion
                    .finish(ModalCloseReason::OwnerRemoved)
                {
                    effects.extend(queued_completion_effects(
                        queued.id,
                        &mut queued.request,
                        outcome,
                        ModalCloseReason::OwnerRemoved,
                    ));
                }
            } else {
                retained.push_back(queued);
            }
        }
        self.queue = retained;
        effects
    }

    fn drain_all(&mut self, reason: ModalCloseReason) -> Vec<ResultEffect> {
        let mut effects = Vec::new();
        if let Some(active) = self.active.as_ref() {
            let id = active.id;
            let kind = active.request.kind;
            effects.extend(
                self.close_active(id, outcome_for_reason(kind, reason), reason)
                    .unwrap_or_default(),
            );
        }
        while let Some(mut queued) = self.queue.pop_front() {
            let outcome = outcome_for_reason(queued.request.kind, reason);
            if queued.request.completion.finish(reason) {
                effects.extend(queued_completion_effects(
                    queued.id,
                    &mut queued.request,
                    outcome,
                    reason,
                ));
            }
        }
        effects
    }

    fn request_action(
        &mut self,
        presentation: ModalPresentationId,
        action_index: usize,
        source: ModalActivationSource,
        owner: WeakEntity<Self>,
    ) -> Result<Vec<ResultEffect>, ModalTerminalOutcomeError> {
        let current = self.active.as_ref().map(|active| active.id);
        let Some(active) = self
            .active
            .as_mut()
            .filter(|active| active.id == presentation)
        else {
            return Err(ModalTerminalOutcomeError::Stale(
                ModalStaleGenerationError::new(presentation, current),
            ));
        };
        if active.request.completion.status() != CompletionStatus::Pending {
            return Err(ModalTerminalOutcomeError::AlreadyDelivered);
        }
        if !active.may_request_action(action_index) {
            return Err(ModalTerminalOutcomeError::Closed);
        }
        let action_role = active.request.actions[action_index].role;
        match active.request.kind {
            ModalKind::Alert => {
                let reason = if action_role == ModalActionRole::Cancel {
                    ModalCloseReason::Cancelled
                } else {
                    ModalCloseReason::Action
                };
                self.close_active(
                    presentation,
                    InternalOutcome::Activated {
                        action_index,
                        source,
                    },
                    reason,
                )
                .map_err(ModalTerminalOutcomeError::Stale)
            }
            ModalKind::Dialog => {
                active.close_attempt_generation = active.close_attempt_generation.saturating_add(1);
                let attempt = active.close_attempt_generation;
                let requested = DialogAttempt {
                    attempt,
                    action_index,
                    source,
                    phase: DialogAttemptPhase::ActionRequested,
                };
                match &mut active.state {
                    RuntimeState::Open => {
                        active.state = RuntimeState::DialogPending(DialogPendingState {
                            primary: Some(requested),
                            cancel: None,
                        });
                    }
                    RuntimeState::DialogPending(pending) => {
                        pending.cancel = Some(requested);
                    }
                    _ => return Err(ModalTerminalOutcomeError::Closed),
                }
                let mut effects = active
                    .lifecycle_effect(ModalLifecycleEvent::ActionRequested(presentation))
                    .into_iter()
                    .collect::<Vec<_>>();
                let Some(handler) = active.request.dialog_action.clone() else {
                    if let RuntimeState::DialogPending(pending) = &mut active.state {
                        pending.remove(attempt);
                        if !pending.has_authority() {
                            active.state = RuntimeState::Open;
                        }
                    }
                    return Ok(effects);
                };
                let completion = DialogPendingCompletion::new(
                    owner,
                    self.window_id,
                    presentation,
                    attempt,
                    active.request.completion.clone(),
                );
                effects.push(Box::new(move |cx| {
                    let decision =
                        handler(action_index, source, presentation, completion.clone(), cx);
                    let _ = apply_dialog_decision(&completion, decision, None, cx);
                }));
                Ok(effects)
            }
            ModalKind::Progress => {
                active.close_attempt_generation = active.close_attempt_generation.saturating_add(1);
                let attempt = active.close_attempt_generation;
                active.state = RuntimeState::ProgressActionRequested { attempt, source };
                let mut effects = active
                    .lifecycle_effect(ModalLifecycleEvent::ActionRequested(presentation))
                    .into_iter()
                    .collect::<Vec<_>>();
                let Some(handler) = active.request.progress_cancel.clone() else {
                    active.state = RuntimeState::Open;
                    return Ok(effects);
                };
                let completion = ProgressCancellationCompletion::new(
                    owner,
                    self.window_id,
                    presentation,
                    attempt,
                    active.request.completion.clone(),
                );
                effects.push(Box::new(move |cx| {
                    let decision = handler(source, completion.clone(), cx);
                    let _ = apply_progress_cancel_decision(&completion, decision, cx);
                }));
                Ok(effects)
            }
        }
    }

    fn apply_dialog_decision(
        &mut self,
        presentation: ModalPresentationId,
        attempt: u64,
        decision: DialogCloseDecision,
        successor_focus: Option<FocusHandle>,
    ) -> Result<Vec<ResultEffect>, ModalTerminalOutcomeError> {
        let current = self.active.as_ref().map(|active| active.id);
        let Some(active) = self
            .active
            .as_mut()
            .filter(|active| active.id == presentation)
        else {
            return Err(ModalTerminalOutcomeError::Stale(
                ModalStaleGenerationError::new(presentation, current),
            ));
        };
        let requested = match &active.state {
            RuntimeState::DialogPending(pending) => pending.attempt(attempt),
            _ => None,
        }
        .ok_or_else(|| {
            ModalTerminalOutcomeError::Stale(ModalStaleGenerationError::new(presentation, current))
        })?;
        match decision {
            DialogCloseDecision::Allow => {
                active.successor_focus = successor_focus;
                let reason = if active
                    .request
                    .actions
                    .get(requested.action_index)
                    .is_some_and(|action| action.role == ModalActionRole::Cancel)
                {
                    ModalCloseReason::Cancelled
                } else {
                    ModalCloseReason::Action
                };
                self.close_active(
                    presentation,
                    InternalOutcome::Activated {
                        action_index: requested.action_index,
                        source: requested.source,
                    },
                    reason,
                )
                .map_err(ModalTerminalOutcomeError::Stale)
            }
            DialogCloseDecision::Deny { first_invalid } => {
                let RuntimeState::DialogPending(pending) = &mut active.state else {
                    return Err(ModalTerminalOutcomeError::Closed);
                };
                pending.remove(attempt);
                if !pending.has_authority() {
                    active.state = RuntimeState::Open;
                }
                if let Some(first_invalid) = first_invalid {
                    active.request.focus_intent = PreparedFocusIntent::Body(first_invalid);
                    active.focus_request_generation =
                        active.focus_request_generation.saturating_add(1);
                }
                Ok(Vec::new())
            }
            DialogCloseDecision::Pending => {
                let RuntimeState::DialogPending(pending) = &mut active.state else {
                    return Err(ModalTerminalOutcomeError::Closed);
                };
                let Some(requested) = pending.attempt_mut(attempt) else {
                    return Err(ModalTerminalOutcomeError::Closed);
                };
                requested.phase = DialogAttemptPhase::Pending;
                Ok(active
                    .lifecycle_effect(ModalLifecycleEvent::Pending(presentation))
                    .into_iter()
                    .collect())
            }
        }
    }

    fn apply_progress_cancel_decision(
        &mut self,
        presentation: ModalPresentationId,
        attempt: u64,
        decision: ProgressCancelDecision,
    ) -> Result<Vec<ResultEffect>, ModalTerminalOutcomeError> {
        let current = self.active.as_ref().map(|active| active.id);
        let Some(active) = self
            .active
            .as_mut()
            .filter(|active| active.id == presentation)
        else {
            return Err(ModalTerminalOutcomeError::Stale(
                ModalStaleGenerationError::new(presentation, current),
            ));
        };
        let source = match active.state {
            RuntimeState::ProgressActionRequested {
                attempt: current,
                source,
            }
            | RuntimeState::ProgressPending {
                attempt: current,
                source,
            } if current == attempt => source,
            _ => {
                return Err(ModalTerminalOutcomeError::Stale(
                    ModalStaleGenerationError::new(presentation, current),
                ));
            }
        };
        match decision {
            ProgressCancelDecision::Allow => self
                .close_active(
                    presentation,
                    InternalOutcome::Progress(ProgressDialogOutcome::Cancelled { source }),
                    ModalCloseReason::Cancelled,
                )
                .map_err(ModalTerminalOutcomeError::Stale),
            ProgressCancelDecision::Deny => {
                active.state = RuntimeState::Open;
                Ok(Vec::new())
            }
            ProgressCancelDecision::Pending => {
                active.state = RuntimeState::ProgressPending { attempt, source };
                Ok(active
                    .lifecycle_effect(ModalLifecycleEvent::Pending(presentation))
                    .into_iter()
                    .collect())
            }
        }
    }

    fn render_snapshot(&self) -> Option<ModalRenderSnapshot> {
        let active = self.active.as_ref()?;
        let semantic_snapshot = active.logical_semantic_snapshot();
        let (default_action, cancel_action) = match &active.request.semantics {
            PreparedModalSemantics::Alert {
                default_action,
                cancel_action,
                ..
            }
            | PreparedModalSemantics::Dialog {
                default_action,
                cancel_action,
                ..
            } => (*default_action, *cancel_action),
            PreparedModalSemantics::Progress { .. } => {
                (None, active.progress_cancellation_action_index())
            }
        };
        Some(ModalRenderSnapshot {
            id: active.request.id.clone(),
            presentation: active.id,
            kind: active.request.kind,
            semantics: active.request.semantics.clone(),
            actions: active
                .request
                .actions
                .iter()
                .enumerate()
                .map(|(index, action)| ModalRenderAction {
                    label: action.label.clone(),
                    role: action.role,
                    intent: action.intent,
                    emphasis: action.emphasis,
                    enabled: active.may_request_action(index),
                    is_default: action.is_default,
                    debug_identity: action.debug_identity.clone(),
                })
                .collect(),
            focus_intent: active.current_focus_intent(),
            focus_request_generation: active.focus_request_generation,
            interaction_enabled: matches!(active.state, RuntimeState::Open),
            body: active.request.body.clone(),
            dialog_size: active.request.dialog_size,
            default_action,
            cancel_action,
            progress: active.progress.clone(),
            semantic_snapshot,
        })
    }

    fn register_root_scope(&mut self, focus: &FocusHandle) {
        self.focus_chain.root_scope = Some(focus.downgrade());
    }

    pub(super) fn register_modal_scope(
        &mut self,
        presentation: ModalPresentationId,
        focus: &FocusHandle,
    ) {
        if self.focus_chain.generation == Some(presentation) {
            self.focus_chain.modal_scope = Some(focus.downgrade());
        }
    }

    fn restore_focus_if_ready(&mut self, window: &mut Window, cx: &App) {
        if !self.focus_chain.restoration_pending
            || self.active.is_some()
            || !window.is_window_active()
        {
            return;
        }
        if let Some(current) = window.focused(cx) {
            let modal_owned = self
                .focus_chain
                .modal_scope
                .as_ref()
                .and_then(WeakFocusHandle::upgrade)
                .is_some_and(|scope| scope.contains(&current, window));
            let retired_owned = self
                .focus_chain
                .retired_owned_transient
                .as_ref()
                .and_then(WeakFocusHandle::upgrade)
                .is_some_and(|focus| focus.contains(&current, window));
            if !modal_owned && !retired_owned {
                self.focus_chain.restoration_pending = false;
                return;
            }
        }
        let target = self.focus_chain.successor.clone().or_else(|| {
            self.focus_chain
                .predecessor
                .as_ref()
                .and_then(WeakFocusHandle::upgrade)
        });
        let root = self
            .focus_chain
            .root_scope
            .as_ref()
            .and_then(WeakFocusHandle::upgrade);
        if let (Some(target), Some(root)) = (target, root)
            && root.contains(&target, window)
        {
            target.focus(window);
        }
        self.focus_chain.restoration_pending = false;
        self.focus_chain.predecessor = None;
        self.focus_chain.successor = None;
        self.focus_chain.modal_scope = None;
        self.focus_chain.retired_owned_transient = None;
    }

    fn progress_update_disables_active_action(
        &self,
        presentation: ModalPresentationId,
        expected_update_generation: u64,
        update: &ProgressDialogUpdate,
    ) -> bool {
        update.cancellation_enabled == Some(false)
            && self.active.as_ref().is_some_and(|active| {
                active.id == presentation
                    && active.request.completion.status() == CompletionStatus::Pending
                    && active.update_generation == expected_update_generation
                    && active
                        .progress
                        .as_ref()
                        .is_some_and(|progress| progress.cancellation_capable)
            })
    }

    fn update_progress(
        &mut self,
        presentation: ModalPresentationId,
        expected_update_generation: u64,
        update: ProgressDialogUpdate,
    ) -> Result<u64, ModalUpdateError> {
        let current = self.active.as_ref().map(|active| active.id);
        if let Some(active) = self
            .active
            .as_mut()
            .filter(|active| active.id == presentation)
        {
            if active.request.completion.status() != CompletionStatus::Pending {
                return Err(ModalUpdateError::Closed);
            }
            if active.update_generation != expected_update_generation {
                return Err(ModalUpdateError::StaleUpdate {
                    attempted: expected_update_generation,
                    current: active.update_generation,
                });
            }
            let Some(progress) = active.progress.as_mut() else {
                return Err(ModalUpdateError::Closed);
            };
            if update.cancellation_enabled.is_some() && !progress.cancellation_capable {
                return Err(ModalUpdateError::CancellationNotSupported);
            }
            let cancellation_was_disabled = update.cancellation_enabled == Some(false);
            progress.apply(update);
            if cancellation_was_disabled
                && matches!(
                    active.state,
                    RuntimeState::ProgressActionRequested { .. }
                        | RuntimeState::ProgressPending { .. }
                )
            {
                active.state = RuntimeState::Open;
                active.close_attempt_generation = active.close_attempt_generation.saturating_add(1);
            }
            active.update_generation = active.update_generation.saturating_add(1);
            return Ok(active.update_generation);
        }

        let Some(queued) = self
            .queue
            .iter_mut()
            .find(|queued| queued.id == presentation)
        else {
            return Err(ModalUpdateError::Stale(ModalStaleGenerationError::new(
                presentation,
                current,
            )));
        };
        if queued.request.completion.status() != CompletionStatus::Pending {
            return Err(ModalUpdateError::Closed);
        }
        if queued.update_generation != expected_update_generation {
            return Err(ModalUpdateError::StaleUpdate {
                attempted: expected_update_generation,
                current: queued.update_generation,
            });
        }
        let Some(progress) = queued.progress.as_mut() else {
            return Err(ModalUpdateError::Closed);
        };
        if update.cancellation_enabled.is_some() && !progress.cancellation_capable {
            return Err(ModalUpdateError::CancellationNotSupported);
        }
        progress.apply(update);
        queued.update_generation = queued.update_generation.saturating_add(1);
        Ok(queued.update_generation)
    }

    fn finish_dialog(
        &mut self,
        presentation: ModalPresentationId,
        successor_focus: Option<FocusHandle>,
    ) -> Result<Vec<ResultEffect>, ModalTerminalOutcomeError> {
        if let Some(active) = self
            .active
            .as_mut()
            .filter(|active| active.id == presentation)
        {
            if active.request.kind != ModalKind::Dialog {
                return Err(ModalTerminalOutcomeError::Stale(
                    ModalStaleGenerationError::new(
                        presentation,
                        self.active.as_ref().map(|active| active.id),
                    ),
                ));
            }
            active.successor_focus = successor_focus;
            return self
                .close_active(
                    presentation,
                    InternalOutcome::DialogProgrammaticCompletion,
                    ModalCloseReason::Programmatic,
                )
                .map_err(ModalTerminalOutcomeError::Stale);
        }
        if let Some(index) = self
            .queue
            .iter()
            .position(|queued| queued.id == presentation)
        {
            let Some(mut queued) = self.queue.remove(index) else {
                return Err(ModalTerminalOutcomeError::Closed);
            };
            if queued.request.kind != ModalKind::Dialog {
                return Err(ModalTerminalOutcomeError::Stale(
                    ModalStaleGenerationError::new(
                        presentation,
                        self.active.as_ref().map(|active| active.id),
                    ),
                ));
            }
            if !queued
                .request
                .completion
                .finish(ModalCloseReason::Programmatic)
            {
                return Err(ModalTerminalOutcomeError::AlreadyDelivered);
            }
            if let Some(successor_focus) = successor_focus {
                self.focus_chain.successor = Some(successor_focus);
            }
            return Ok(queued_completion_effects(
                presentation,
                &mut queued.request,
                InternalOutcome::DialogProgrammaticCompletion,
                ModalCloseReason::Programmatic,
            ));
        }
        Err(ModalTerminalOutcomeError::Stale(
            ModalStaleGenerationError::new(
                presentation,
                self.active.as_ref().map(|active| active.id),
            ),
        ))
    }

    fn finish_progress(
        &mut self,
        presentation: ModalPresentationId,
        outcome: ProgressDialogOutcome,
    ) -> Result<Vec<ResultEffect>, ModalTerminalOutcomeError> {
        let reason = outcome.close_reason();
        if self
            .active
            .as_ref()
            .is_some_and(|active| active.id == presentation)
        {
            return self
                .close_active(presentation, InternalOutcome::Progress(outcome), reason)
                .map_err(ModalTerminalOutcomeError::Stale);
        }
        if let Some(index) = self
            .queue
            .iter()
            .position(|queued| queued.id == presentation)
        {
            let Some(mut queued) = self.queue.remove(index) else {
                return Err(ModalTerminalOutcomeError::Closed);
            };
            if !queued.request.completion.finish(reason) {
                return Err(ModalTerminalOutcomeError::AlreadyDelivered);
            }
            return Ok(queued_completion_effects(
                presentation,
                &mut queued.request,
                InternalOutcome::Progress(outcome),
                reason,
            ));
        }
        Err(ModalTerminalOutcomeError::Stale(
            ModalStaleGenerationError::new(
                presentation,
                self.active.as_ref().map(|active| active.id),
            ),
        ))
    }
}

impl Render for ModalWindowOwner {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.restore_focus_if_ready(window, cx);
        let snapshot = self.render_snapshot();
        super::render::render_modal_owner(self, snapshot, cx.entity().downgrade(), window, cx)
    }
}

fn opened_effects(
    active: &ActivePresentation,
    owner: WeakEntity<ModalWindowOwner>,
    window_id: WindowId,
) -> Vec<ResultEffect> {
    let presentation = active.id;
    let mut effects: Vec<ResultEffect> = Vec::new();
    if let Some(handler) = active.request.lifecycle.clone() {
        let owner = owner.clone();
        effects.push(Box::new(move |cx: &mut App| {
            if startup_effect_is_live(&owner, window_id, presentation, cx) {
                handler(&ModalLifecycleEvent::Opened(presentation), cx);
            }
        }));
    }
    if let Some(deadline) = active.request.programmatic_deadline {
        effects.push(Box::new(move |cx| {
            if !startup_effect_is_live(&owner, window_id, presentation, cx) {
                return;
            }
            let task = cx.spawn(async move |cx| {
                cx.background_executor().timer(deadline).await;
                let _ = cx
                    .update(|cx| expire_programmatic_deadline(&owner, window_id, presentation, cx));
            });
            task.detach();
        }));
    }
    effects
}

fn startup_effect_is_live(
    owner: &WeakEntity<ModalWindowOwner>,
    window_id: WindowId,
    presentation: ModalPresentationId,
    cx: &App,
) -> bool {
    let Some(owner) = owner.upgrade() else {
        return false;
    };
    owner.read_with(cx, |state, _| {
        state.window_id == window_id
            && state.window_available
            && state
                .active
                .as_ref()
                .is_some_and(|active| active.id == presentation)
    })
}

fn completion_effect(
    _presentation: ModalPresentationId,
    request: &mut PreparedModalRequest,
    outcome: InternalOutcome,
) -> Option<ResultEffect> {
    let sink = request.result_sink.take()?;
    Some(Box::new(move |cx| sink(outcome, cx)))
}

fn queued_completion_effects(
    presentation: ModalPresentationId,
    request: &mut PreparedModalRequest,
    outcome: InternalOutcome,
    reason: ModalCloseReason,
) -> Vec<ResultEffect> {
    let mut effects = completion_effect(presentation, request, outcome)
        .into_iter()
        .collect::<Vec<_>>();
    if let Some(handler) = request.lifecycle.clone() {
        effects.push(Box::new(move |cx| {
            handler(&ModalLifecycleEvent::Closed(presentation, reason), cx);
        }));
    }
    effects
}

fn outcome_for_reason(kind: ModalKind, reason: ModalCloseReason) -> InternalOutcome {
    match kind {
        ModalKind::Alert | ModalKind::Dialog => InternalOutcome::Dismissed(reason),
        ModalKind::Progress => InternalOutcome::Progress(match reason {
            ModalCloseReason::DeadlineExpired => ProgressDialogOutcome::DeadlineExpired,
            ModalCloseReason::OwnerRemoved => ProgressDialogOutcome::OwnerRemoved,
            ModalCloseReason::Action | ModalCloseReason::Cancelled => {
                ProgressDialogOutcome::Cancelled {
                    source: ModalActivationSource::Programmatic,
                }
            }
            ModalCloseReason::Programmatic => ProgressDialogOutcome::ProgrammaticDismissal,
            ModalCloseReason::Replaced => ProgressDialogOutcome::Replaced,
        }),
    }
}

#[derive(Default)]
struct ModalCoordinator {
    owners: HashMap<WindowId, WeakEntity<ModalWindowOwner>>,
}

impl Global for ModalCoordinator {}

pub(super) fn init(cx: &mut App) {
    if !cx.has_global::<ModalCoordinator>() {
        cx.set_global(ModalCoordinator::default());
    }
}

pub(super) fn modal_owner_for_layer(window: &Window, cx: &mut App) -> Entity<ModalWindowOwner> {
    init(cx);
    let window_id = window.window_handle().window_id();
    if let Some(owner) = cx
        .global::<ModalCoordinator>()
        .owners
        .get(&window_id)
        .and_then(WeakEntity::upgrade)
    {
        return owner;
    }
    let owner = cx.new(|cx| ModalWindowOwner::new(window_id, cx));
    let weak = owner.downgrade();
    cx.update_global::<ModalCoordinator, _>(|coordinator, _| {
        coordinator.owners.insert(window_id, weak);
    });
    owner
}

fn remove_coordinator_owner(window_id: WindowId, entity_id: EntityId, cx: &mut App) {
    if !cx.has_global::<ModalCoordinator>() {
        return;
    }
    cx.update_global::<ModalCoordinator, _>(|coordinator, _| {
        if coordinator
            .owners
            .get(&window_id)
            .is_some_and(|owner| owner.entity_id() == entity_id)
        {
            coordinator.owners.remove(&window_id);
        }
    });
}

pub(super) fn retire_window_owner(owner: &Entity<ModalWindowOwner>, cx: &mut App) {
    let (window_id, entity_id) = (owner.read(cx).window_id, owner.entity_id());
    disarm_modal_controls(owner, cx);
    dismiss_owned_menu_before_any_active_close(owner, cx);
    remove_coordinator_owner(window_id, entity_id, cx);
    let (effects, suspension, had_transients) = owner.update(cx, |state, _| {
        state.window_available = false;
        let effects = state.drain_all(ModalCloseReason::OwnerRemoved);
        let suspension = state.palette_suspension.take();
        let had_transients = state.transients_active;
        state.transients_active = false;
        (effects, suspension, had_transients)
    });
    if had_transients {
        crate::tooltip::set_window_tooltip_suppression(
            window_id,
            crate::tooltip::TooltipSuppression::Modal,
            false,
            cx,
        );
    }
    if let Some(suspension) = suspension {
        crate::command_palette::discard_window_command_palette_suspension(suspension, cx);
    }
    defer_owner_effects(owner, effects, cx);
}

pub(super) fn present<T: 'static>(
    mut request: PreparedModalRequest,
    window: &Window,
    cx: &mut Context<T>,
) -> Result<ModalPresentationHandle, ModalPresentationError> {
    if !cx.has_global::<ModalDesktopPolicy>() {
        return Err(ModalPresentationError::DesktopPolicyNotInstalled);
    }
    let caller = cx.weak_entity();
    let caller_id = caller.entity_id();
    let owner = modal_owner_for_layer(window, cx);
    let owner_weak = owner.downgrade();
    let window_id = window.window_handle().window_id();
    let completion = request.completion.clone();
    let release_owner = owner_weak.clone();
    request._caller_release = Some(cx.on_release(move |_, cx| {
        caller_released(&release_owner, window_id, caller_id, cx);
    }));
    let first_visible_request = owner.read_with(cx, |state, _| {
        state.active.is_none() && state.settlement == SettlementState::Idle
    });
    if first_visible_request {
        let menu_predecessor = crate::menu::dismiss_active_menu_for_replacement(window, cx)
            .and_then(|replacement| replacement.0);
        let suspended = crate::command_palette::suspend_window_command_palette(window_id, cx);
        crate::tooltip::set_window_tooltip_suppression(
            window_id,
            crate::tooltip::TooltipSuppression::Modal,
            true,
            cx,
        );
        let predecessor = suspended
            .predecessor
            .clone()
            .or(menu_predecessor)
            .or_else(|| window.focused(cx).map(|focus| focus.downgrade()));
        owner.update(cx, |state, _| {
            state.begin_transient_coordination(predecessor, suspended.token);
        });
    }
    let (presentation, effects) = owner.update(cx, |state, cx| {
        let result = state.submit(request, owner_weak.clone());
        cx.notify();
        result
    })?;
    defer_owner_effects(&owner, effects, cx);
    let window_handle = window.window_handle();
    let owner_until_refresh = owner.clone();
    cx.defer(move |cx| {
        let _ = &owner_until_refresh;
        let _ = cx.update_window(window_handle, |_, window, _| window.refresh());
    });
    Ok(ModalPresentationHandle {
        owner,
        window_id,
        presentation,
        completion,
    })
}

pub(super) fn replace_active<T: 'static>(
    mut request: PreparedModalRequest,
    window: &Window,
    cx: &mut Context<T>,
) -> Result<ModalPresentationHandle, ModalPresentationError> {
    if !cx.has_global::<ModalDesktopPolicy>() {
        return Err(ModalPresentationError::DesktopPolicyNotInstalled);
    }
    let owner =
        modal_owner_for_render(window, cx).ok_or(ModalPresentationError::NoActivePresentation)?;
    let window_id = window.window_handle().window_id();
    let can_replace = owner.read_with(cx, |state, _| {
        state.window_id == window_id && state.window_available && state.active.is_some()
    });
    if !can_replace {
        return Err(if owner.read(cx).window_available {
            ModalPresentationError::NoActivePresentation
        } else {
            ModalPresentationError::WindowUnavailable
        });
    }

    let caller_id = request.caller_owner.entity_id();
    let completion = request.completion.clone();
    disarm_modal_controls(&owner, cx);
    dismiss_owned_menu_before_any_active_close(&owner, cx);
    let owner_weak = owner.downgrade();
    let release_owner = owner_weak.clone();
    request._caller_release = Some(cx.on_release(move |_, cx| {
        caller_released(&release_owner, window_id, caller_id, cx);
    }));
    let (presentation, effects) = owner.update(cx, |state, cx| {
        let result = state.replace_active(request, owner_weak.clone());
        cx.notify();
        result
    })?;
    settle_owner(&owner, effects, cx);
    let window_handle = window.window_handle();
    let owner_until_refresh = owner.clone();
    cx.defer(move |cx| {
        let _ = &owner_until_refresh;
        let _ = cx.update_window(window_handle, |_, window, _| window.refresh());
    });
    Ok(ModalPresentationHandle {
        owner,
        window_id,
        presentation,
        completion,
    })
}

fn caller_released(
    owner: &WeakEntity<ModalWindowOwner>,
    window_id: WindowId,
    caller: EntityId,
    cx: &mut App,
) {
    let Some(owner) = owner.upgrade() else {
        return;
    };
    if owner.read(cx).window_id != window_id {
        return;
    }
    #[cfg(test)]
    owner.update(cx, |state, _| {
        state.caller_release_callbacks += 1;
    });
    let closes_active = owner.read_with(cx, |state, _| {
        state
            .active
            .as_ref()
            .is_some_and(|active| active.request.caller_owner.entity_id() == caller)
    });
    if closes_active {
        disarm_modal_controls(&owner, cx);
        dismiss_owned_menu_before_any_active_close(&owner, cx);
    }
    let effects = owner.update(cx, |state, _| state.remove_caller(caller));
    settle_owner(&owner, effects, cx);
}

fn expire_programmatic_deadline(
    owner: &WeakEntity<ModalWindowOwner>,
    window_id: WindowId,
    presentation: ModalPresentationId,
    cx: &mut App,
) {
    let Some(owner) = owner.upgrade() else {
        return;
    };
    disarm_modal_controls_for_presentation(&owner, presentation, cx);
    dismiss_owned_menu_before_close(&owner, presentation, cx);
    let effects = owner.update(cx, |state, _| {
        if state.window_id != window_id {
            return Vec::new();
        }
        state
            .close_active(
                presentation,
                InternalOutcome::Progress(ProgressDialogOutcome::DeadlineExpired),
                ModalCloseReason::DeadlineExpired,
            )
            .unwrap_or_default()
    });
    settle_owner(&owner, effects, cx);
}

fn disarm_modal_controls(owner: &Entity<ModalWindowOwner>, cx: &mut App) {
    let press_owner = owner.read_with(cx, |state, _| state.press_owner());
    press_owner.disarm(cx);
}

fn disarm_modal_controls_for_presentation(
    owner: &Entity<ModalWindowOwner>,
    presentation: ModalPresentationId,
    cx: &mut App,
) {
    let targets_active = owner.read_with(cx, |state, _| {
        state
            .active
            .as_ref()
            .is_some_and(|active| active.id == presentation)
    });
    if targets_active {
        disarm_modal_controls(owner, cx);
    }
}

fn dismiss_owned_menu_before_any_active_close(owner: &Entity<ModalWindowOwner>, cx: &mut App) {
    let parent = owner.read_with(cx, |state, _| state.active_modal_parent());
    if let Some(parent) = parent
        && let Some(retired_focus) = crate::menu::dismiss_menu_owned_by_modal_parent(parent, cx)
    {
        owner.update(cx, |state, _| {
            state.focus_chain.retired_owned_transient = Some(retired_focus);
        });
    }
}

fn dismiss_owned_menu_before_close(
    owner: &Entity<ModalWindowOwner>,
    presentation: ModalPresentationId,
    cx: &mut App,
) {
    let parent = owner.read_with(cx, |state, _| state.active_modal_parent_for(presentation));
    if let Some(parent) = parent
        && let Some(retired_focus) = crate::menu::dismiss_menu_owned_by_modal_parent(parent, cx)
    {
        owner.update(cx, |state, _| {
            state.focus_chain.retired_owned_transient = Some(retired_focus);
        });
    }
}

fn settle_owner(owner: &Entity<ModalWindowOwner>, effects: Vec<ResultEffect>, cx: &mut App) {
    let owns_settlement = owner.update(cx, |state, cx| {
        let owns_settlement = state.begin_settlement();
        cx.notify();
        owns_settlement
    });
    enqueue_owner_effects(owner, effects, owns_settlement, cx);
}

fn defer_owner_effects(owner: &Entity<ModalWindowOwner>, effects: Vec<ResultEffect>, cx: &mut App) {
    if effects.is_empty() {
        return;
    }
    enqueue_owner_effects(owner, effects, false, cx);
}

fn enqueue_owner_effects(
    owner: &Entity<ModalWindowOwner>,
    effects: Vec<ResultEffect>,
    completes_settlement: bool,
    cx: &mut App,
) {
    if effects.is_empty() && !completes_settlement {
        return;
    }
    let batch = DeferredEffectBatch::new(effects, completes_settlement);
    let should_schedule = owner.update(cx, |state, _| match state.effect_pump {
        EffectPumpState::Idle => {
            state.deferred_effects.push_back(batch);
            state.effect_pump = EffectPumpState::Scheduled;
            true
        }
        EffectPumpState::Scheduled => {
            state.deferred_effects.push_back(batch);
            false
        }
        EffectPumpState::Running => {
            state.reentrant_effects.push_back(batch);
            false
        }
    });
    if should_schedule {
        let owner = owner.clone();
        cx.defer(move |cx| pump_owner_effects(&owner, cx));
    }
}

fn pump_owner_effects(owner: &Entity<ModalWindowOwner>, cx: &mut App) {
    owner.update(cx, |state, _| {
        state.effect_pump = EffectPumpState::Running;
    });
    loop {
        let batch = owner.update(cx, |state, _| state.deferred_effects.pop_front());
        let Some(batch) = batch else {
            break;
        };
        pump_effect_batch(owner, batch, cx);
    }
    owner.update(cx, |state, _| {
        state.effect_pump = EffectPumpState::Idle;
    });
}

fn pump_effect_batch(
    owner: &Entity<ModalWindowOwner>,
    mut batch: DeferredEffectBatch,
    cx: &mut App,
) {
    while let Some(effect) = batch.effects.pop_front() {
        effect(cx);
        pump_reentrant_effects(owner, cx);
    }
    if batch.completes_settlement {
        finish_owner_settlement(owner, cx);
        pump_reentrant_effects(owner, cx);
    }
}

fn pump_reentrant_effects(owner: &Entity<ModalWindowOwner>, cx: &mut App) {
    loop {
        let batches = owner.update(cx, |state, _| std::mem::take(&mut state.reentrant_effects));
        if batches.is_empty() {
            return;
        }
        for batch in batches {
            pump_effect_batch(owner, batch, cx);
        }
    }
}

fn finish_owner_settlement(owner: &Entity<ModalWindowOwner>, cx: &mut App) {
    let weak = owner.downgrade();
    loop {
        let should_promote = owner.read_with(cx, |state, _| {
            state.active.is_none() && !state.queue.is_empty()
        });
        if !should_promote {
            break;
        }
        let promotion = owner.update(cx, |state, cx| {
            let effects = state.promote(weak.clone());
            cx.notify();
            effects
        });
        pump_effect_batch(owner, DeferredEffectBatch::new(promotion, false), cx);
    }

    let (window_id, suspension) = owner.update(cx, |state, _| {
        state.finish_settlement();
        (state.window_id, state.finish_transient_coordination())
    });
    if let Some(suspension) = suspension {
        crate::tooltip::set_window_tooltip_suppression(
            window_id,
            crate::tooltip::TooltipSuppression::Modal,
            false,
            cx,
        );
        crate::command_palette::resume_window_command_palette(suspension, cx);
    }
}

fn defer_released_owner_effects(effects: Vec<ResultEffect>, cx: &mut App) {
    if effects.is_empty() {
        return;
    }
    cx.defer(move |cx| run_effects(effects, cx));
}

fn run_effects(effects: Vec<ResultEffect>, cx: &mut App) {
    for effect in effects {
        effect(cx);
    }
}

/// Opaque authority to dismiss one exact active or queued modal presentation.
///
/// The handle carries its Operating-System Window and monotonic presentation generation. Dismissal
/// never targets by modal identity, label, or queue position, so a stale handle cannot close a
/// promoted successor. All clones share the same exactly-once completion flag.
#[derive(Clone)]
pub struct ModalPresentationHandle {
    owner: Entity<ModalWindowOwner>,
    window_id: WindowId,
    presentation: ModalPresentationId,
    completion: CompletionFlag,
}

impl ModalPresentationHandle {
    /// Returns the monotonic presentation identity carried by retained operations.
    pub const fn presentation_id(&self) -> ModalPresentationId {
        self.presentation
    }

    /// Dismisses this exact active or queued presentation without affecting a successor.
    ///
    /// # Errors
    ///
    /// Returns [`ModalDismissalError::Closed`] after terminal delivery,
    /// [`ModalDismissalError::OwnerRemoved`] after caller or Operating-System Window teardown, or
    /// [`ModalDismissalError::Stale`] for a mismatched window or superseded generation.
    pub fn dismiss(&self, window: &Window, cx: &mut App) -> Result<(), ModalDismissalError> {
        self.check_window(window)?;
        match self.completion.status() {
            CompletionStatus::Closed => return Err(ModalDismissalError::Closed),
            CompletionStatus::OwnerRemoved => return Err(ModalDismissalError::OwnerRemoved),
            CompletionStatus::Replaced => {
                return Err(ModalDismissalError::Stale(ModalStaleGenerationError::new(
                    self.presentation,
                    current_presentation(&self.owner.downgrade(), cx),
                )));
            }
            CompletionStatus::Pending => {}
        }
        let owner = self.owner.clone();
        disarm_modal_controls_for_presentation(&owner, self.presentation, cx);
        dismiss_owned_menu_before_close(&owner, self.presentation, cx);
        let effects = owner.update(cx, |state, _| {
            state.dismiss(self.presentation, ModalCloseReason::Programmatic)
        })?;
        settle_owner(&owner, effects, cx);
        Ok(())
    }

    fn check_window(&self, window: &Window) -> Result<(), ModalDismissalError> {
        if window.window_handle().window_id() == self.window_id {
            Ok(())
        } else {
            Err(ModalDismissalError::Stale(ModalStaleGenerationError::new(
                self.presentation,
                None,
            )))
        }
    }
}

/// Generation-bound programmatic completion authority for one Dialog presentation.
///
/// Active and queued Dialogs may complete independently of semantic actions or pending action
/// authority. Completion may nominate a logical successor focus target; restoration still refuses
/// removed targets, another Operating-System Window, or a newer focus owner. Generic dismissal
/// remains available and continues to produce a dismissed Dialog outcome.
#[derive(Clone)]
pub struct DialogCompletion {
    presentation: ModalPresentationHandle,
}

impl DialogCompletion {
    pub(super) fn new(presentation: ModalPresentationHandle) -> Self {
        Self { presentation }
    }

    /// Returns the exact presentation identity this operation may complete.
    pub const fn presentation_id(&self) -> ModalPresentationId {
        self.presentation.presentation_id()
    }

    /// Completes this exact active or queued Dialog with a typed programmatic outcome.
    ///
    /// The optional successor from an active or queued presentation remains in the window-owned
    /// guarded restoration chain until the visible modal sequence ends. A later explicit successor
    /// supersedes it, while a removed, cross-window, or otherwise superseded target is not focused.
    ///
    /// # Errors
    ///
    /// Returns [`ModalTerminalOutcomeError`] after terminal delivery, owner removal, replacement,
    /// a stale generation, or use with another Operating-System Window.
    pub fn complete(
        &self,
        window: &Window,
        successor_focus: Option<FocusHandle>,
        cx: &mut App,
    ) -> Result<(), ModalTerminalOutcomeError> {
        check_terminal_operation(
            &self.presentation.owner.downgrade(),
            self.presentation.window_id,
            self.presentation.presentation,
            &self.presentation.completion,
            window,
            cx,
        )?;
        let owner = self.presentation.owner.clone();
        disarm_modal_controls_for_presentation(&owner, self.presentation.presentation, cx);
        dismiss_owned_menu_before_close(&owner, self.presentation.presentation, cx);
        let effects = owner.update(cx, |state, _| {
            state.finish_dialog(self.presentation.presentation, successor_focus)
        })?;
        settle_owner(&owner, effects, cx);
        Ok(())
    }

    /// Dismisses this exact Dialog without reporting programmatic completion.
    ///
    /// # Errors
    ///
    /// Returns [`ModalDismissalError`] under the same conditions as
    /// [`ModalPresentationHandle::dismiss`].
    pub fn dismiss(&self, window: &Window, cx: &mut App) -> Result<(), ModalDismissalError> {
        self.presentation.dismiss(window, cx)
    }
}

/// Opaque completion authority for one matching pending Dialog close attempt.
///
/// The authority is bound to the Operating-System Window, presentation generation, close-attempt
/// generation, and exactly-once flag. A primary pending attempt remains authoritative while one
/// nested Cancel attempt is pending; denial of either preserves the still-live counterpart. The
/// authority cannot complete a replacement, a promoted successor, a resolved attempt, or a
/// presentation whose owner was removed.
#[derive(Clone)]
pub struct DialogPendingCompletion {
    owner: WeakEntity<ModalWindowOwner>,
    window_id: WindowId,
    presentation: ModalPresentationId,
    attempt: u64,
    completion: CompletionFlag,
}

impl DialogPendingCompletion {
    fn new(
        owner: WeakEntity<ModalWindowOwner>,
        window_id: WindowId,
        presentation: ModalPresentationId,
        attempt: u64,
        completion: CompletionFlag,
    ) -> Self {
        Self {
            owner,
            window_id,
            presentation,
            attempt,
            completion,
        }
    }

    /// Allows the pending action to close this exact Dialog presentation.
    ///
    /// The optional successor focus is retained by the private focus-restoration chain.
    /// It is restored only if still live and rendered in the same Operating-System Window and no
    /// newer transient or explicit focus owner has superseded it.
    ///
    /// # Errors
    ///
    /// Returns [`ModalTerminalOutcomeError`] when this attempt is stale, already terminal, closed,
    /// owned by a removed caller or window, or applied to another Operating-System Window.
    pub fn allow(
        &self,
        window: &Window,
        successor_focus: Option<FocusHandle>,
        cx: &mut App,
    ) -> Result<(), ModalTerminalOutcomeError> {
        self.complete(window, DialogCloseDecision::Allow, successor_focus, cx)
    }

    /// Rejects the pending action and keeps all caller-owned values intact.
    ///
    /// A live `first_invalid` target becomes the next frame's contained focus entry; a missing or
    /// removed target falls back to another live target inside the Dialog.
    ///
    /// # Errors
    ///
    /// Returns [`ModalTerminalOutcomeError`] under the same stale and terminal conditions as
    /// [`Self::allow`].
    pub fn deny(
        &self,
        window: &Window,
        first_invalid: Option<FocusHandle>,
        cx: &mut App,
    ) -> Result<(), ModalTerminalOutcomeError> {
        self.complete(
            window,
            DialogCloseDecision::Deny { first_invalid },
            None,
            cx,
        )
    }

    fn complete(
        &self,
        window: &Window,
        decision: DialogCloseDecision,
        successor_focus: Option<FocusHandle>,
        cx: &mut App,
    ) -> Result<(), ModalTerminalOutcomeError> {
        check_terminal_operation(
            &self.owner,
            self.window_id,
            self.presentation,
            &self.completion,
            window,
            cx,
        )?;
        let owner = self
            .owner
            .upgrade()
            .ok_or(ModalTerminalOutcomeError::OwnerRemoved)?;
        if matches!(&decision, DialogCloseDecision::Allow) {
            disarm_modal_controls_for_presentation(&owner, self.presentation, cx);
            dismiss_owned_menu_before_close(&owner, self.presentation, cx);
        }
        let effects = owner.update(cx, |state, _| {
            state.apply_dialog_decision(self.presentation, self.attempt, decision, successor_focus)
        })?;
        settle_owner(&owner, effects, cx);
        Ok(())
    }
}

/// Opaque completion authority for one matching pending progress cancellation attempt.
///
/// Clones share presentation, attempt, window, retained activation source, and exactly-once
/// authority. A stale completion cannot cancel a replacement or a later attempt after cancellation
/// was denied.
#[derive(Clone)]
pub struct ProgressCancellationCompletion {
    owner: WeakEntity<ModalWindowOwner>,
    window_id: WindowId,
    presentation: ModalPresentationId,
    attempt: u64,
    completion: CompletionFlag,
}

impl ProgressCancellationCompletion {
    fn new(
        owner: WeakEntity<ModalWindowOwner>,
        window_id: WindowId,
        presentation: ModalPresentationId,
        attempt: u64,
        completion: CompletionFlag,
    ) -> Self {
        Self {
            owner,
            window_id,
            presentation,
            attempt,
            completion,
        }
    }

    /// Allows the matching pending cancellation and closes exactly once with its retained source.
    ///
    /// # Errors
    ///
    /// Returns [`ModalTerminalOutcomeError`] for stale presentation or attempt identity, duplicate
    /// terminal delivery, owner removal, closure, or an Operating-System Window mismatch.
    pub fn allow(&self, window: &Window, cx: &mut App) -> Result<(), ModalTerminalOutcomeError> {
        self.complete(window, ProgressCancelDecision::Allow, cx)
    }

    /// Denies the matching pending cancellation and returns the ProgressDialog to open state.
    ///
    /// # Errors
    ///
    /// Returns [`ModalTerminalOutcomeError`] under the same conditions as [`Self::allow`].
    pub fn deny(&self, window: &Window, cx: &mut App) -> Result<(), ModalTerminalOutcomeError> {
        self.complete(window, ProgressCancelDecision::Deny, cx)
    }

    fn complete(
        &self,
        window: &Window,
        decision: ProgressCancelDecision,
        cx: &mut App,
    ) -> Result<(), ModalTerminalOutcomeError> {
        check_terminal_operation(
            &self.owner,
            self.window_id,
            self.presentation,
            &self.completion,
            window,
            cx,
        )?;
        let owner = self
            .owner
            .upgrade()
            .ok_or(ModalTerminalOutcomeError::OwnerRemoved)?;
        if decision == ProgressCancelDecision::Allow {
            disarm_modal_controls_for_presentation(&owner, self.presentation, cx);
            dismiss_owned_menu_before_close(&owner, self.presentation, cx);
        }
        let effects = owner.update(cx, |state, _| {
            state.apply_progress_cancel_decision(self.presentation, self.attempt, decision)
        })?;
        settle_owner(&owner, effects, cx);
        Ok(())
    }
}

/// Retained authority for bounded updates and terminal completion of one ProgressDialog.
///
/// Each clone retains its observed update generation while all clones share exactly-once terminal
/// authority. Updates and terminal methods target this exact active or queued generation. Queued
/// status, detail, progress, cancellation availability, and update generation transfer unchanged
/// when the presentation is promoted. A clone whose generation was superseded receives a
/// stale-update error instead of overwriting newer status. Updates mutate one stable surface and
/// never auto-close at determinate maximum. Terminal methods are mutually exclusive:
/// the first accepted completion, failure, dismissal, cancellation, deadline, or teardown wins.
#[derive(Clone)]
pub struct ProgressDialogHandle {
    presentation: ModalPresentationHandle,
    update_generation: Cell<u64>,
}

impl ProgressDialogHandle {
    pub(super) fn new(presentation: ModalPresentationHandle) -> Self {
        Self {
            presentation,
            update_generation: Cell::new(0),
        }
    }

    /// Returns the exact presentation identity this handle may update.
    pub const fn presentation_id(&self) -> ModalPresentationId {
        self.presentation.presentation_id()
    }

    /// Applies a bounded status, detail, progress, or cancellation-availability update.
    ///
    /// # Errors
    ///
    /// Returns [`ModalUpdateError::Invalid`] for invalid bounded text,
    /// [`ModalUpdateError::CancellationNotSupported`] when a programmatic-only presentation is
    /// asked to change cancellation availability,
    /// [`ModalUpdateError::StaleUpdate`] when a clone submits an older update generation,
    /// [`ModalUpdateError::Stale`] for a replacement or window mismatch,
    /// [`ModalUpdateError::Closed`] after terminal completion, or
    /// [`ModalUpdateError::OwnerRemoved`] after teardown.
    pub fn update(
        &self,
        update: ProgressDialogUpdate,
        window: &Window,
        cx: &mut App,
    ) -> Result<(), ModalUpdateError> {
        update.validate().map_err(ModalUpdateError::Invalid)?;
        check_update_operation(&self.presentation, window, cx)?;
        let owner = self.presentation.owner.clone();
        let disables_active_action = owner.read_with(cx, |state, _| {
            state.progress_update_disables_active_action(
                self.presentation.presentation,
                self.update_generation.get(),
                &update,
            )
        });
        if disables_active_action {
            disarm_modal_controls(&owner, cx);
        }
        let next = owner.update(cx, |state, cx| {
            let next = state.update_progress(
                self.presentation.presentation,
                self.update_generation.get(),
                update,
            );
            if next.is_ok() {
                cx.notify();
            }
            next
        })?;
        self.update_generation.set(next);
        Ok(())
    }

    /// Completes the operation. A determinate value of one never calls this implicitly.
    ///
    /// # Errors
    ///
    /// Returns [`ModalTerminalOutcomeError`] when terminal delivery already occurred, ownership was
    /// removed, this generation is stale, the presentation closed, or the window does not match.
    pub fn complete(&self, window: &Window, cx: &mut App) -> Result<(), ModalTerminalOutcomeError> {
        self.finish(ProgressDialogOutcome::Completed, window, cx)
    }

    /// Completes the operation with a content-free failure outcome.
    ///
    /// # Errors
    ///
    /// Returns [`ModalTerminalOutcomeError`] under the same conditions as [`Self::complete`].
    pub fn fail(&self, window: &Window, cx: &mut App) -> Result<(), ModalTerminalOutcomeError> {
        self.finish(ProgressDialogOutcome::Failed, window, cx)
    }

    /// Dismisses the operation without reporting successful completion.
    ///
    /// # Errors
    ///
    /// Returns [`ModalTerminalOutcomeError`] under the same conditions as [`Self::complete`].
    pub fn dismiss(&self, window: &Window, cx: &mut App) -> Result<(), ModalTerminalOutcomeError> {
        self.finish(ProgressDialogOutcome::ProgrammaticDismissal, window, cx)
    }

    fn finish(
        &self,
        outcome: ProgressDialogOutcome,
        window: &Window,
        cx: &mut App,
    ) -> Result<(), ModalTerminalOutcomeError> {
        check_terminal_operation(
            &self.presentation.owner.downgrade(),
            self.presentation.window_id,
            self.presentation.presentation,
            &self.presentation.completion,
            window,
            cx,
        )?;
        let owner = self.presentation.owner.clone();
        disarm_modal_controls_for_presentation(&owner, self.presentation.presentation, cx);
        dismiss_owned_menu_before_close(&owner, self.presentation.presentation, cx);
        let effects = owner.update(cx, |state, _| {
            state.finish_progress(self.presentation.presentation, outcome)
        })?;
        settle_owner(&owner, effects, cx);
        Ok(())
    }
}

fn apply_dialog_decision(
    completion: &DialogPendingCompletion,
    decision: DialogCloseDecision,
    successor_focus: Option<FocusHandle>,
    cx: &mut App,
) -> Result<(), ModalTerminalOutcomeError> {
    let Some(owner) = completion.owner.upgrade() else {
        return Err(ModalTerminalOutcomeError::OwnerRemoved);
    };
    if matches!(&decision, DialogCloseDecision::Allow) {
        disarm_modal_controls_for_presentation(&owner, completion.presentation, cx);
        dismiss_owned_menu_before_close(&owner, completion.presentation, cx);
    }
    let effects = owner.update(cx, |state, _| {
        state.apply_dialog_decision(
            completion.presentation,
            completion.attempt,
            decision,
            successor_focus,
        )
    })?;
    settle_owner(&owner, effects, cx);
    Ok(())
}

fn apply_progress_cancel_decision(
    completion: &ProgressCancellationCompletion,
    decision: ProgressCancelDecision,
    cx: &mut App,
) -> Result<(), ModalTerminalOutcomeError> {
    let Some(owner) = completion.owner.upgrade() else {
        return Err(ModalTerminalOutcomeError::OwnerRemoved);
    };
    if decision == ProgressCancelDecision::Allow {
        disarm_modal_controls_for_presentation(&owner, completion.presentation, cx);
        dismiss_owned_menu_before_close(&owner, completion.presentation, cx);
    }
    let effects = owner.update(cx, |state, _| {
        state.apply_progress_cancel_decision(completion.presentation, completion.attempt, decision)
    })?;
    settle_owner(&owner, effects, cx);
    Ok(())
}

fn check_terminal_operation(
    owner: &WeakEntity<ModalWindowOwner>,
    window_id: WindowId,
    presentation: ModalPresentationId,
    completion: &CompletionFlag,
    window: &Window,
    cx: &App,
) -> Result<(), ModalTerminalOutcomeError> {
    if window.window_handle().window_id() != window_id {
        return Err(ModalTerminalOutcomeError::Stale(
            ModalStaleGenerationError::new(presentation, None),
        ));
    }
    match completion.status() {
        CompletionStatus::Pending => Ok(()),
        CompletionStatus::Closed => Err(ModalTerminalOutcomeError::AlreadyDelivered),
        CompletionStatus::OwnerRemoved => Err(ModalTerminalOutcomeError::OwnerRemoved),
        CompletionStatus::Replaced => Err(ModalTerminalOutcomeError::Stale(
            ModalStaleGenerationError::new(presentation, current_presentation(owner, cx)),
        )),
    }
}

fn check_update_operation(
    handle: &ModalPresentationHandle,
    window: &Window,
    cx: &App,
) -> Result<(), ModalUpdateError> {
    if window.window_handle().window_id() != handle.window_id {
        return Err(ModalUpdateError::Stale(ModalStaleGenerationError::new(
            handle.presentation,
            None,
        )));
    }
    match handle.completion.status() {
        CompletionStatus::Pending => Ok(()),
        CompletionStatus::Closed => Err(ModalUpdateError::Closed),
        CompletionStatus::OwnerRemoved => Err(ModalUpdateError::OwnerRemoved),
        CompletionStatus::Replaced => Err(ModalUpdateError::Stale(ModalStaleGenerationError::new(
            handle.presentation,
            current_presentation(&handle.owner.downgrade(), cx),
        ))),
    }
}

fn current_presentation(
    owner: &WeakEntity<ModalWindowOwner>,
    cx: &App,
) -> Option<ModalPresentationId> {
    owner
        .upgrade()
        .and_then(|owner| owner.read(cx).active.as_ref().map(|active| active.id))
}

pub(super) fn modal_owner_for_render(
    window: &Window,
    cx: &App,
) -> Option<Entity<ModalWindowOwner>> {
    if !cx.has_global::<ModalCoordinator>() {
        return None;
    }
    cx.global::<ModalCoordinator>()
        .owners
        .get(&window.window_handle().window_id())
        .and_then(WeakEntity::upgrade)
}

pub(crate) fn current_modal_parent(window: &Window, cx: &App) -> Option<ModalParentToken> {
    let owner = modal_owner_for_render(window, cx)?;
    let presentation = owner.read(cx).active.as_ref()?.id;
    Some(ModalParentToken {
        window_id: window.window_handle().window_id(),
        presentation,
    })
}

pub(crate) fn focused_modal_parent(window: &Window, cx: &App) -> Option<ModalParentToken> {
    let owner = modal_owner_for_render(window, cx)?;
    let state = owner.read(cx);
    let presentation = state.active.as_ref()?.id;
    let scope = state
        .focus_chain
        .modal_scope
        .as_ref()
        .and_then(WeakFocusHandle::upgrade)?;
    let focused = window.focused(cx)?;
    scope
        .contains(&focused, window)
        .then_some(ModalParentToken {
            window_id: window.window_handle().window_id(),
            presentation,
        })
}

pub(crate) fn focus_allows_transient_resume(window: &Window, cx: &App) -> bool {
    if !window.is_window_active() || crate::menu::window_menu_is_open(window, cx) {
        return false;
    }
    let Some(owner) = modal_owner_for_render(window, cx) else {
        return true;
    };
    let state = owner.read(cx);
    if state.active.is_some() || state.settlement == SettlementState::Settling {
        return false;
    }
    let Some(focused) = window.focused(cx) else {
        return true;
    };
    state
        .focus_chain
        .modal_scope
        .as_ref()
        .and_then(WeakFocusHandle::upgrade)
        .is_some_and(|scope| scope.contains(&focused, window))
        || state
            .focus_chain
            .retired_owned_transient
            .as_ref()
            .and_then(WeakFocusHandle::upgrade)
            .is_some_and(|focus| focus.contains(&focused, window))
        || state
            .focus_chain
            .predecessor
            .as_ref()
            .and_then(WeakFocusHandle::upgrade)
            .is_some_and(|focus| focus.contains(&focused, window))
}

pub(super) fn register_root_scope(
    owner: &Entity<ModalWindowOwner>,
    scope: &FocusHandle,
    cx: &mut App,
) {
    owner.update(cx, |state, _| state.register_root_scope(scope));
}

pub(super) fn toggle_alert_suppression(
    owner: &WeakEntity<ModalWindowOwner>,
    presentation: ModalPresentationId,
    cx: &mut App,
) {
    let _ = owner.update(cx, |state, cx| {
        let Some(active) = state.active.as_mut().filter(|active| {
            active.id == presentation && matches!(active.state, RuntimeState::Open)
        }) else {
            return;
        };
        if let PreparedModalSemantics::Alert {
            suppression: Some((_, selected)),
            ..
        } = &mut active.request.semantics
        {
            *selected = !*selected;
            if let Some(flag) = &active.request.suppression_flag {
                flag.set(*selected);
            }
            cx.notify();
        }
    });
}

pub(super) fn request_action_from_renderer(
    owner: &WeakEntity<ModalWindowOwner>,
    presentation: ModalPresentationId,
    action_index: usize,
    source: ModalActivationSource,
    cx: &mut App,
) {
    let Some(owner) = owner.upgrade() else {
        return;
    };
    let weak = owner.downgrade();
    let effects = owner.update(cx, |state, cx| {
        let effects = state
            .request_action(presentation, action_index, source, weak)
            .unwrap_or_default();
        cx.notify();
        effects
    });
    settle_owner(&owner, effects, cx);
}

pub(super) fn window_modal_is_open(window: &Window, cx: &App) -> bool {
    if !cx.has_global::<ModalCoordinator>() {
        return false;
    }
    cx.global::<ModalCoordinator>()
        .owners
        .get(&window.window_handle().window_id())
        .and_then(WeakEntity::upgrade)
        .is_some_and(|owner| {
            owner.read_with(cx, |state, _| {
                state.active.is_some() || state.settlement == SettlementState::Settling
            })
        })
}

#[cfg(test)]
pub(super) fn retire_modal_owner_for_test(window: &Window, cx: &mut App) -> bool {
    let Some(owner) = modal_owner_for_render(window, cx) else {
        return true;
    };
    let press_owner = owner.read_with(cx, |owner, _| owner.press_owner());
    retire_window_owner(&owner, cx);
    press_owner.controls_are_idle(cx)
}

#[cfg(test)]
pub(super) fn modal_controls_are_idle_for_test(window: &Window, cx: &App) -> bool {
    modal_owner_for_render(window, cx)
        .is_none_or(|owner| owner.read(cx).press_owner.controls_are_idle(cx))
}

#[cfg(test)]
pub(super) fn modal_button_controls_are_idle_for_test(window: &Window, cx: &App) -> bool {
    modal_controls_are_idle_for_test(window, cx)
}

#[cfg(test)]
pub(super) fn close_attempt_generation_for_test(window: &Window, cx: &App) -> Option<u64> {
    modal_owner_for_render(window, cx)?
        .read(cx)
        .active
        .as_ref()
        .map(|active| active.close_attempt_generation)
}

#[cfg(test)]
pub(super) fn active_progress_for_test(
    window: &Window,
    cx: &App,
) -> Option<(ModalPresentationId, ProgressRuntime, u64)> {
    modal_owner_for_render(window, cx)?
        .read(cx)
        .active
        .as_ref()
        .and_then(|active| {
            Some((
                active.id,
                active.progress.clone()?,
                active.update_generation,
            ))
        })
}

#[cfg(test)]
pub(super) fn active_progress_presentation_facts_for_test(
    window: &Window,
    cx: &App,
) -> Option<(bool, usize)> {
    let owner = modal_owner_for_render(window, cx)?;
    let owner_state = owner.read(cx);
    let active = owner_state.active.as_ref()?;
    let semantics = active.logical_semantic_snapshot();
    Some((
        semantics.progress?.cancellation_available,
        active.request.actions.len(),
    ))
}

#[cfg(test)]
mod tests {
    use std::{cell::RefCell, rc::Rc};

    use gpui::{AnyWeakEntity, TestAppContext, div};

    use super::*;
    use crate::modal::DeterminateProgress;

    fn test_request(
        id: &'static str,
        owner: AnyWeakEntity,
        outcomes: Rc<RefCell<Vec<ModalCloseReason>>>,
    ) -> PreparedModalRequest {
        PreparedModalRequest::new(
            ModalId::new(id),
            ModalKind::Alert,
            Vec::new(),
            PreparedModalSemantics::Alert {
                accessibility_title: "Alert".into(),
                visible_title: "Alert".into(),
                message: "Message".into(),
                detail: None,
                intent: AlertIntent::Informational,
                accessory: None,
                suppression: None,
                default_action: None,
                cancel_action: None,
            },
            PreparedFocusIntent::Surface,
            owner,
            Box::new(move |outcome, _| {
                if let InternalOutcome::Dismissed(reason) = outcome {
                    outcomes.borrow_mut().push(reason);
                }
            }),
        )
    }

    fn traced_request(
        id: &'static str,
        owner: AnyWeakEntity,
        trace: Rc<RefCell<Vec<String>>>,
    ) -> PreparedModalRequest {
        let result_trace = trace.clone();
        let lifecycle_trace = trace;
        let mut request = test_request(id, owner, Rc::new(RefCell::new(Vec::new())));
        request.result_sink = Some(Box::new(move |_, _| {
            result_trace.borrow_mut().push(format!("{id}:result"));
        }));
        request.with_lifecycle(Some(Rc::new(move |event, _| {
            let transition = match event {
                ModalLifecycleEvent::Opened(_) => "opened",
                ModalLifecycleEvent::ActionRequested(_) => "action-requested",
                ModalLifecycleEvent::Pending(_) => "pending",
                ModalLifecycleEvent::Closing(_) => "closing",
                ModalLifecycleEvent::Closed(_, _) => "closed",
            };
            lifecycle_trace
                .borrow_mut()
                .push(format!("{id}:{transition}"));
        })))
    }

    fn dismiss_retained_handle_for_test(handle: &ModalPresentationHandle, cx: &mut App) {
        let owner = handle.owner.clone();
        let effects = owner
            .update(cx, |state, _| {
                state.dismiss(handle.presentation, ModalCloseReason::Programmatic)
            })
            .expect("retained handle should dismiss its presentation");
        settle_owner(&owner, effects, cx);
    }

    fn progress_request(owner: AnyWeakEntity) -> PreparedModalRequest {
        let cancel = ModalAction::new(
            "cancel",
            "Cancel",
            ModalActionRole::Cancel,
            "cancel-progress",
        );
        PreparedModalRequest::new(
            ModalId::new("progress"),
            ModalKind::Progress,
            PreparedModalRequest::erase_actions(vec![cancel]),
            PreparedModalSemantics::Progress {
                accessibility_title: "Progress".into(),
                visible_title: "Progress".into(),
                status: "Working".into(),
                detail: None,
                progress: ProgressState::Indeterminate,
                cancellation_capable: true,
            },
            PreparedFocusIntent::Action(0),
            owner,
            Box::new(|_, _| {}),
        )
        .with_progress_cancel(Rc::new(|_, _, _| ProgressCancelDecision::Pending))
    }

    fn programmatic_progress_request(owner: AnyWeakEntity) -> PreparedModalRequest {
        PreparedModalRequest::new(
            ModalId::new("programmatic-progress"),
            ModalKind::Progress,
            Vec::new(),
            PreparedModalSemantics::Progress {
                accessibility_title: "Required progress".into(),
                visible_title: "Required Progress".into(),
                status: "Working".into(),
                detail: None,
                progress: ProgressState::Indeterminate,
                cancellation_capable: false,
            },
            PreparedFocusIntent::Surface,
            owner,
            Box::new(|_, _| {}),
        )
        .with_programmatic_deadline(Some(Duration::from_secs(30)))
    }

    fn dialog_request(owner: AnyWeakEntity) -> PreparedModalRequest {
        let save = ModalAction::new("save", "Save", ModalActionRole::Affirmative, "save-dialog");
        let cancel = ModalAction::new("cancel", "Cancel", ModalActionRole::Cancel, "cancel-dialog");
        PreparedModalRequest::new(
            ModalId::new("dialog"),
            ModalKind::Dialog,
            PreparedModalRequest::erase_actions(vec![save, cancel]),
            PreparedModalSemantics::Dialog {
                accessibility_title: "Dialog".into(),
                visible_title: "Dialog".into(),
                description: None,
                default_action: None,
                cancel_action: Some(1),
            },
            PreparedFocusIntent::Action(0),
            owner,
            Box::new(|_, _| {}),
        )
        .with_dialog_action(Rc::new(|_, _, _, _, _| DialogCloseDecision::Pending))
    }

    struct ReleaseListenerRoot;

    impl Render for ReleaseListenerRoot {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div()
        }
    }

    struct ReleaseListenerCaller;

    #[gpui::test]
    fn repeated_settled_presentations_do_not_retain_caller_release_callbacks(
        cx: &mut TestAppContext,
    ) {
        cx.set_global(ModalDesktopPolicy::mac_os());
        let (_, cx) = cx.add_window_view(|_, _| ReleaseListenerRoot);
        let caller = cx.update(|_, cx| cx.new(|_| ReleaseListenerCaller));
        let outcomes = Rc::new(RefCell::new(Vec::new()));
        let mut retained_owner = None;

        for _ in 0..3 {
            let presented = cx.update(|window, cx| {
                caller.update(cx, |_, cx| {
                    present(
                        test_request("presented", cx.weak_entity().into(), outcomes.clone()),
                        window,
                        cx,
                    )
                    .expect("request should present")
                })
            });
            retained_owner.get_or_insert_with(|| presented.owner.clone());
            cx.run_until_parked();

            let replacement = cx.update(|window, cx| {
                caller.update(cx, |_, cx| {
                    replace_active(
                        test_request("replacement", cx.weak_entity().into(), outcomes.clone()),
                        window,
                        cx,
                    )
                    .expect("active request should be replaceable")
                })
            });
            cx.run_until_parked();
            cx.update(|window, cx| {
                caller.update(cx, |_, cx| {
                    replacement
                        .dismiss(window, cx)
                        .expect("replacement should dismiss")
                })
            });
            cx.run_until_parked();
        }

        let active = cx.update(|window, cx| {
            caller.update(cx, |_, cx| {
                present(
                    test_request("live-active", cx.weak_entity().into(), outcomes.clone()),
                    window,
                    cx,
                )
                .expect("live request should present")
            })
        });
        let queued = cx.update(|window, cx| {
            caller.update(cx, |_, cx| {
                present(
                    test_request("live-queued", cx.weak_entity().into(), outcomes.clone()),
                    window,
                    cx,
                )
                .expect("second live request should queue")
            })
        });
        cx.run_until_parked();

        drop(caller);
        cx.update(|_, _| {});
        cx.run_until_parked();

        let owner = retained_owner.expect("modal owner should be retained for inspection");
        assert_eq!(
            (
                owner.read_with(cx, |state, _| state.caller_release_callbacks),
                outcomes.borrow().clone(),
                active.completion.status(),
                queued.completion.status(),
            ),
            (
                2,
                vec![
                    ModalCloseReason::Replaced,
                    ModalCloseReason::Programmatic,
                    ModalCloseReason::Replaced,
                    ModalCloseReason::Programmatic,
                    ModalCloseReason::Replaced,
                    ModalCloseReason::Programmatic,
                    ModalCloseReason::OwnerRemoved,
                    ModalCloseReason::OwnerRemoved,
                ],
                CompletionStatus::OwnerRemoved,
                CompletionStatus::OwnerRemoved,
            )
        );
    }

    #[test]
    fn queue_is_fifo_and_bounded_to_eight_waiting_requests() {
        let window_id = WindowId::from(1);
        let mut owner = ModalWindowOwner::new_for_test(window_id);
        let weak = WeakEntity::new_invalid();
        let caller = AnyWeakEntity::new_invalid();
        let outcomes = Rc::new(RefCell::new(Vec::new()));
        owner
            .submit(
                test_request("active", caller.clone(), outcomes.clone()),
                weak.clone(),
            )
            .expect("first request should open");
        for index in 0..MAX_QUEUED_REQUESTS {
            owner
                .submit(
                    test_request(
                        Box::leak(format!("queued-{index}").into_boxed_str()),
                        caller.clone(),
                        outcomes.clone(),
                    ),
                    weak.clone(),
                )
                .expect("bounded request should queue");
        }

        let overflow = owner.submit(test_request("overflow", caller, outcomes), weak.clone());
        let active = owner.active.as_ref().map(|active| active.id);
        owner
            .close_active(
                active.expect("active request should exist"),
                InternalOutcome::Dismissed(ModalCloseReason::Programmatic),
                ModalCloseReason::Programmatic,
            )
            .expect("active request should close");
        owner.promote(weak);

        assert!(
            matches!(overflow, Err(ModalPresentationError::QueueFull))
                && owner
                    .active
                    .as_ref()
                    .is_some_and(|active| active.id.value() == 2)
        );
    }

    #[test]
    fn settlement_reservation_preserves_eight_waiting_slots_behind_the_fifo_head() {
        let mut owner = ModalWindowOwner::new_for_test(WindowId::from(1));
        let weak = WeakEntity::new_invalid();
        let caller = AnyWeakEntity::new_invalid();
        let outcomes = Rc::new(RefCell::new(Vec::new()));
        let (active, _) = owner
            .submit(
                test_request("active", caller.clone(), outcomes.clone()),
                weak.clone(),
            )
            .expect("first request should open");
        for index in 0..MAX_QUEUED_REQUESTS {
            owner
                .submit(
                    test_request(
                        Box::leak(format!("queued-{index}").into_boxed_str()),
                        caller.clone(),
                        outcomes.clone(),
                    ),
                    weak.clone(),
                )
                .expect("bounded request should queue");
        }
        owner
            .close_active(
                active,
                InternalOutcome::Dismissed(ModalCloseReason::Programmatic),
                ModalCloseReason::Programmatic,
            )
            .expect("active request should close");
        assert!(owner.begin_settlement());

        let additional = owner.submit(
            test_request("additional", caller.clone(), outcomes.clone()),
            weak.clone(),
        );
        let overflow = owner.submit(test_request("overflow", caller, outcomes), weak.clone());
        owner.promote(weak);
        owner.finish_settlement();

        assert!(
            additional.is_ok()
                && matches!(overflow, Err(ModalPresentationError::QueueFull))
                && owner.active.is_some()
                && owner.queue.len() == MAX_QUEUED_REQUESTS
        );
    }

    #[test]
    fn presentation_generations_are_monotonic_across_queue_promotion() {
        let mut owner = ModalWindowOwner::new_for_test(WindowId::from(1));
        let weak = WeakEntity::new_invalid();
        let caller = AnyWeakEntity::new_invalid();
        let outcomes = Rc::new(RefCell::new(Vec::new()));
        let (first, _) = owner
            .submit(
                test_request("first", caller.clone(), outcomes.clone()),
                weak.clone(),
            )
            .expect("first request should open");
        let (second, _) = owner
            .submit(test_request("second", caller, outcomes), weak.clone())
            .expect("second request should queue");
        owner
            .close_active(
                first,
                InternalOutcome::Dismissed(ModalCloseReason::Programmatic),
                ModalCloseReason::Programmatic,
            )
            .expect("first request should close");
        owner.promote(weak);

        assert_eq!(
            (
                first.value(),
                second.value(),
                owner.active.as_ref().map(|active| active.id)
            ),
            (1, 2, Some(second))
        );
    }

    #[test]
    fn independent_window_owners_do_not_share_generations_or_queues() {
        let weak = WeakEntity::new_invalid();
        let caller = AnyWeakEntity::new_invalid();
        let outcomes = Rc::new(RefCell::new(Vec::new()));
        let mut first = ModalWindowOwner::new_for_test(WindowId::from(1));
        let mut second = ModalWindowOwner::new_for_test(WindowId::from(2));
        let (first_id, _) = first
            .submit(
                test_request("first", caller.clone(), outcomes.clone()),
                weak.clone(),
            )
            .expect("first window should open");
        let (second_id, _) = second
            .submit(test_request("second", caller, outcomes), weak)
            .expect("second window should open");

        assert_eq!(
            (
                first_id.value(),
                second_id.value(),
                first.queue.len(),
                second.queue.len()
            ),
            (1, 1, 0, 0)
        );
    }

    #[test]
    fn stale_completion_cannot_close_promoted_successor() {
        let mut owner = ModalWindowOwner::new_for_test(WindowId::from(1));
        let weak = WeakEntity::new_invalid();
        let caller = AnyWeakEntity::new_invalid();
        let outcomes = Rc::new(RefCell::new(Vec::new()));
        let (first, _) = owner
            .submit(
                test_request("first", caller.clone(), outcomes.clone()),
                weak.clone(),
            )
            .expect("first should open");
        let (second, _) = owner
            .submit(test_request("second", caller, outcomes), weak.clone())
            .expect("second should queue");
        owner
            .close_active(
                first,
                InternalOutcome::Dismissed(ModalCloseReason::Programmatic),
                ModalCloseReason::Programmatic,
            )
            .expect("first should close");
        owner.promote(weak);

        let stale = owner.close_active(
            first,
            InternalOutcome::Dismissed(ModalCloseReason::Programmatic),
            ModalCloseReason::Programmatic,
        );

        assert!(matches!(
            stale,
            Err(error) if error == ModalStaleGenerationError::new(first, Some(second))
        ));
    }

    #[test]
    fn owner_removal_resolves_active_and_queued_requests_once() {
        let mut owner = ModalWindowOwner::new_for_test(WindowId::from(1));
        let weak = WeakEntity::new_invalid();
        let caller = AnyWeakEntity::new_invalid();
        let other = AnyWeakEntity::new_invalid();
        let outcomes = Rc::new(RefCell::new(Vec::new()));
        let active_request = test_request("active", caller.clone(), outcomes.clone());
        let active_flag = active_request.completion.clone();
        owner
            .submit(active_request, weak.clone())
            .expect("active request should open");
        let queued_request = test_request("queued", caller.clone(), outcomes.clone());
        let queued_flag = queued_request.completion.clone();
        owner
            .submit(queued_request, weak.clone())
            .expect("matching request should queue");
        let survivor = test_request("survivor", other, outcomes);
        let (survivor_id, _) = owner
            .submit(survivor, weak.clone())
            .expect("unrelated request should queue");

        owner.remove_caller(caller.entity_id());
        owner.promote(weak);

        assert_eq!(
            (
                active_flag.status(),
                queued_flag.status(),
                owner.active.as_ref().map(|active| active.id)
            ),
            (
                CompletionStatus::OwnerRemoved,
                CompletionStatus::OwnerRemoved,
                Some(survivor_id)
            )
        );
    }

    #[test]
    fn replacement_finishes_previous_generation_before_installing_new_active_state() {
        let mut owner = ModalWindowOwner::new_for_test(WindowId::from(1));
        let weak = WeakEntity::new_invalid();
        let caller = AnyWeakEntity::new_invalid();
        let outcomes = Rc::new(RefCell::new(Vec::new()));
        let first_request = test_request("first", caller.clone(), outcomes.clone());
        let first_flag = first_request.completion.clone();
        let (first, _) = owner
            .submit(first_request, weak.clone())
            .expect("first request should open");
        let (queued, _) = owner
            .submit(
                test_request("queued", caller.clone(), outcomes.clone()),
                weak.clone(),
            )
            .expect("waiting request should queue");

        let (replacement, _) = owner
            .replace_active(test_request("replacement", caller, outcomes), weak)
            .expect("active presentation should be replaceable");

        assert_eq!(
            (
                first.value(),
                queued.value(),
                replacement.value(),
                first_flag.status(),
                owner.active.as_ref().map(|active| active.id),
                owner.queue.front().map(|queued| queued.id),
            ),
            (
                1,
                2,
                3,
                CompletionStatus::Replaced,
                Some(replacement),
                Some(queued),
            )
        );
    }

    #[test]
    fn dialog_programmatic_completion_is_independent_of_pending_action_authority() {
        let mut owner = ModalWindowOwner::new_for_test(WindowId::from(1));
        let weak = WeakEntity::new_invalid();
        let request = dialog_request(AnyWeakEntity::new_invalid());
        let completion = request.completion.clone();
        let (presentation, _) = owner
            .submit(request, weak.clone())
            .expect("Dialog should open");
        owner
            .request_action(presentation, 0, ModalActivationSource::Return, weak)
            .expect("action should be requested");
        owner
            .apply_dialog_decision(presentation, 1, DialogCloseDecision::Pending, None)
            .expect("action should become pending");

        owner
            .finish_dialog(presentation, None)
            .expect("programmatic completion should remain independent");

        assert_eq!(completion.status(), CompletionStatus::Closed);
        assert!(owner.active.is_none());
    }

    #[test]
    fn stale_dialog_programmatic_completion_cannot_close_replacement() {
        let mut owner = ModalWindowOwner::new_for_test(WindowId::from(1));
        let weak = WeakEntity::new_invalid();
        let caller = AnyWeakEntity::new_invalid();
        let (presentation, _) = owner
            .submit(dialog_request(caller.clone()), weak.clone())
            .expect("Dialog should open");
        let (replacement, _) = owner
            .replace_active(dialog_request(caller), weak)
            .expect("active Dialog should be replaceable");

        let stale = owner.finish_dialog(presentation, None);

        assert!(
            matches!(
                stale,
                Err(ModalTerminalOutcomeError::Stale(error))
                    if error == ModalStaleGenerationError::new(presentation, Some(replacement))
            ) && owner
                .active
                .as_ref()
                .is_some_and(|active| active.id == replacement)
        );
    }

    #[test]
    fn stale_dialog_pending_completion_cannot_close_replacement() {
        let mut owner = ModalWindowOwner::new_for_test(WindowId::from(1));
        let weak = WeakEntity::new_invalid();
        let caller = AnyWeakEntity::new_invalid();
        let (presentation, _) = owner
            .submit(dialog_request(caller.clone()), weak.clone())
            .expect("dialog should open");
        owner
            .request_action(presentation, 0, ModalActivationSource::Return, weak.clone())
            .expect("dialog action should be requested");
        owner
            .apply_dialog_decision(presentation, 1, DialogCloseDecision::Pending, None)
            .expect("dialog should become pending");
        let (replacement, _) = owner
            .replace_active(dialog_request(caller), weak)
            .expect("active Dialog should be replaceable");

        let stale = owner.apply_dialog_decision(presentation, 1, DialogCloseDecision::Allow, None);

        assert!(
            matches!(stale, Err(ModalTerminalOutcomeError::Stale(_)))
                && owner
                    .active
                    .as_ref()
                    .is_some_and(|active| active.id == replacement)
        );
    }

    fn dialog_with_primary_and_cancel_pending() -> (
        ModalWindowOwner,
        ModalPresentationId,
        WeakEntity<ModalWindowOwner>,
        CompletionFlag,
    ) {
        let mut owner = ModalWindowOwner::new_for_test(WindowId::from(1));
        let weak = WeakEntity::new_invalid();
        let request = dialog_request(AnyWeakEntity::new_invalid());
        let completion = request.completion.clone();
        let (presentation, _) = owner
            .submit(request, weak.clone())
            .expect("dialog should open");
        owner
            .request_action(presentation, 0, ModalActivationSource::Return, weak.clone())
            .expect("primary action should be requested");
        owner
            .apply_dialog_decision(presentation, 1, DialogCloseDecision::Pending, None)
            .expect("primary action should become pending");
        owner
            .request_action(presentation, 1, ModalActivationSource::Escape, weak.clone())
            .expect("nested Cancel should be requested");
        owner
            .apply_dialog_decision(presentation, 2, DialogCloseDecision::Pending, None)
            .expect("nested Cancel should become pending");
        (owner, presentation, weak, completion)
    }

    #[test]
    fn primary_cancel_pending_rejects_repeated_activation_without_advancing_generation() {
        let mut owner = ModalWindowOwner::new_for_test(WindowId::from(1));
        let weak = WeakEntity::new_invalid();
        let (presentation, _) = owner
            .submit(dialog_request(AnyWeakEntity::new_invalid()), weak.clone())
            .expect("dialog should open");
        owner
            .request_action(presentation, 1, ModalActivationSource::Escape, weak.clone())
            .expect("Cancel should be requested");
        owner
            .apply_dialog_decision(presentation, 1, DialogCloseDecision::Pending, None)
            .expect("Cancel should become pending");

        let duplicate = owner.request_action(presentation, 1, ModalActivationSource::Pointer, weak);

        assert!(
            matches!(duplicate, Err(ModalTerminalOutcomeError::Closed))
                && owner.active.as_ref().is_some_and(|active| {
                    active.close_attempt_generation == 1
                        && matches!(
                            &active.state,
                            RuntimeState::DialogPending(DialogPendingState {
                                primary: Some(DialogAttempt {
                                    attempt: 1,
                                    phase: DialogAttemptPhase::Pending,
                                    ..
                                }),
                                cancel: None,
                            })
                        )
                })
        );
    }

    #[test]
    fn nested_cancel_denial_restores_the_original_pending_attempt() {
        let (mut owner, presentation, _, _) = dialog_with_primary_and_cancel_pending();

        owner
            .apply_dialog_decision(
                presentation,
                2,
                DialogCloseDecision::Deny {
                    first_invalid: None,
                },
                None,
            )
            .expect("nested Cancel denial should succeed");

        assert!(owner.active.as_ref().is_some_and(|active| {
            active.close_attempt_generation == 2
                && matches!(
                    &active.state,
                    RuntimeState::DialogPending(DialogPendingState {
                        primary: Some(DialogAttempt {
                            attempt: 1,
                            phase: DialogAttemptPhase::Pending,
                            ..
                        }),
                        cancel: None,
                    })
                )
        }));
    }

    #[test]
    fn pending_nested_cancel_rejects_repeated_activation_without_advancing_generation() {
        let (mut owner, presentation, weak, _) = dialog_with_primary_and_cancel_pending();

        let duplicate = owner.request_action(presentation, 1, ModalActivationSource::Pointer, weak);

        assert!(
            matches!(duplicate, Err(ModalTerminalOutcomeError::Closed))
                && owner.active.as_ref().is_some_and(|active| {
                    active.close_attempt_generation == 2
                        && matches!(
                            &active.state,
                            RuntimeState::DialogPending(DialogPendingState {
                                cancel: Some(DialogAttempt {
                                    attempt: 2,
                                    phase: DialogAttemptPhase::Pending,
                                    ..
                                }),
                                ..
                            })
                        )
                })
        );
    }

    #[test]
    fn original_denial_preserves_nested_cancel_authority_until_cancel_allows() {
        let (mut owner, presentation, _, completion) = dialog_with_primary_and_cancel_pending();
        owner
            .apply_dialog_decision(
                presentation,
                1,
                DialogCloseDecision::Deny {
                    first_invalid: None,
                },
                None,
            )
            .expect("original denial should preserve nested Cancel");

        owner
            .apply_dialog_decision(presentation, 2, DialogCloseDecision::Allow, None)
            .expect("nested Cancel should retain authority");

        assert_eq!(
            (completion.status(), owner.active.is_none()),
            (CompletionStatus::Closed, true)
        );
    }

    #[test]
    fn nested_cancel_denial_preserves_original_authority_until_original_allows() {
        let (mut owner, presentation, _, completion) = dialog_with_primary_and_cancel_pending();
        owner
            .apply_dialog_decision(
                presentation,
                2,
                DialogCloseDecision::Deny {
                    first_invalid: None,
                },
                None,
            )
            .expect("nested Cancel denial should restore original");

        owner
            .apply_dialog_decision(presentation, 1, DialogCloseDecision::Allow, None)
            .expect("original attempt should retain authority");

        assert_eq!(
            (completion.status(), owner.active.is_none()),
            (CompletionStatus::Closed, true)
        );
    }

    #[test]
    fn original_allow_wins_when_it_completes_before_nested_cancel() {
        let (mut owner, presentation, _, completion) = dialog_with_primary_and_cancel_pending();
        owner
            .apply_dialog_decision(presentation, 1, DialogCloseDecision::Allow, None)
            .expect("original allow should close");

        let nested = owner.apply_dialog_decision(presentation, 2, DialogCloseDecision::Allow, None);

        assert!(
            matches!(nested, Err(ModalTerminalOutcomeError::Stale(_)))
                && completion.status() == CompletionStatus::Closed
                && owner.active.is_none()
        );
    }

    #[test]
    fn nested_cancel_allow_wins_when_it_completes_before_original() {
        let (mut owner, presentation, _, completion) = dialog_with_primary_and_cancel_pending();
        owner
            .apply_dialog_decision(presentation, 2, DialogCloseDecision::Allow, None)
            .expect("nested Cancel allow should close");

        let original =
            owner.apply_dialog_decision(presentation, 1, DialogCloseDecision::Allow, None);

        assert!(
            matches!(original, Err(ModalTerminalOutcomeError::Stale(_)))
                && completion.status() == CompletionStatus::Closed
                && owner.active.is_none()
        );
    }

    #[test]
    fn progress_cancellation_denial_reopens_with_a_new_attempt_available() {
        let mut owner = ModalWindowOwner::new_for_test(WindowId::from(1));
        let weak = WeakEntity::new_invalid();
        let (presentation, _) = owner
            .submit(progress_request(AnyWeakEntity::new_invalid()), weak.clone())
            .expect("progress should open");
        owner
            .request_action(presentation, 0, ModalActivationSource::Escape, weak.clone())
            .expect("cancel should be requested");
        owner
            .apply_progress_cancel_decision(presentation, 1, ProgressCancelDecision::Deny)
            .expect("denial should reopen");

        let second = owner.request_action(presentation, 0, ModalActivationSource::Pointer, weak);

        assert!(
            second.is_ok()
                && matches!(
                    owner.active.as_ref().map(|active| active.state.clone()),
                    Some(RuntimeState::ProgressActionRequested {
                        attempt: 2,
                        source: ModalActivationSource::Pointer,
                    })
                )
        );
    }

    #[test]
    fn progress_cancellation_pending_blocks_duplicate_activation() {
        let mut owner = ModalWindowOwner::new_for_test(WindowId::from(1));
        let weak = WeakEntity::new_invalid();
        let (presentation, _) = owner
            .submit(progress_request(AnyWeakEntity::new_invalid()), weak.clone())
            .expect("progress should open");
        owner
            .request_action(presentation, 0, ModalActivationSource::Escape, weak.clone())
            .expect("cancel should be requested");
        owner
            .apply_progress_cancel_decision(presentation, 1, ProgressCancelDecision::Pending)
            .expect("cancellation should become pending");

        let duplicate = owner.request_action(presentation, 0, ModalActivationSource::Pointer, weak);

        assert!(matches!(duplicate, Err(ModalTerminalOutcomeError::Closed)));
    }

    #[test]
    fn progress_cancellation_allow_closes_exactly_once() {
        let mut owner = ModalWindowOwner::new_for_test(WindowId::from(1));
        let weak = WeakEntity::new_invalid();
        let request = progress_request(AnyWeakEntity::new_invalid());
        let completion = request.completion.clone();
        let (presentation, _) = owner
            .submit(request, weak.clone())
            .expect("progress should open");
        owner
            .request_action(presentation, 0, ModalActivationSource::Escape, weak)
            .expect("cancel should be requested");
        owner
            .apply_progress_cancel_decision(presentation, 1, ProgressCancelDecision::Allow)
            .expect("allow should close");

        assert_eq!(
            (completion.status(), owner.active.is_none()),
            (CompletionStatus::Closed, true)
        );
    }

    #[test]
    fn determinate_maximum_update_does_not_close_progress() {
        let mut owner = ModalWindowOwner::new_for_test(WindowId::from(1));
        let weak = WeakEntity::new_invalid();
        let (presentation, _) = owner
            .submit(progress_request(AnyWeakEntity::new_invalid()), weak)
            .expect("progress should open");
        let progress = DeterminateProgress::new(1.0).expect("finite progress should normalize");

        owner
            .update_progress(
                presentation,
                0,
                ProgressDialogUpdate::new().progress(ProgressState::Determinate(progress)),
            )
            .expect("maximum progress update should succeed");

        assert!(owner.active.as_ref().is_some_and(|active| {
            matches!(active.progress.as_ref().map(|state| state.progress), Some(ProgressState::Determinate(value)) if value.is_maximum())
                && matches!(active.state, RuntimeState::Open)
        }));
    }

    #[test]
    fn initially_disabled_progress_cancellation_can_be_enabled_at_runtime() {
        let mut owner = ModalWindowOwner::new_for_test(WindowId::from(1));
        let weak = WeakEntity::new_invalid();
        let mut request = progress_request(AnyWeakEntity::new_invalid());
        request.actions[0].enabled = false;
        let (presentation, _) = owner
            .submit(request, weak)
            .expect("cancellable progress should open");
        let initial = owner
            .render_snapshot()
            .expect("cancellable progress should render");

        owner
            .update_progress(
                presentation,
                0,
                ProgressDialogUpdate::new().cancellation_enabled(true),
            )
            .expect("cancellation should enable");
        let enabled = owner
            .render_snapshot()
            .expect("enabled cancellable progress should render");

        assert!(
            initial.cancel_action == Some(0)
                && !initial.actions[0].enabled
                && matches!(initial.focus_intent, PreparedFocusIntent::Surface)
                && initial.progress.is_some_and(|progress| {
                    progress.cancellation_capable && !progress.cancellation_enabled
                })
                && enabled.cancel_action == Some(0)
                && enabled.actions[0].enabled
                && matches!(enabled.focus_intent, PreparedFocusIntent::Action(0))
                && enabled.progress.is_some_and(|progress| {
                    progress.cancellation_capable && progress.cancellation_enabled
                })
                && owner
                    .active
                    .as_ref()
                    .is_some_and(|active| active.update_generation == 1)
        );
    }

    #[test]
    fn active_programmatic_progress_rejects_cancellation_availability_update_without_mutation() {
        let mut owner = ModalWindowOwner::new_for_test(WindowId::from(1));
        let weak = WeakEntity::new_invalid();
        let (presentation, _) = owner
            .submit(
                programmatic_progress_request(AnyWeakEntity::new_invalid()),
                weak,
            )
            .expect("programmatic progress should open");

        let result = owner.update_progress(
            presentation,
            0,
            ProgressDialogUpdate::new().cancellation_enabled(true),
        );
        let snapshot = owner
            .render_snapshot()
            .expect("programmatic progress should remain active");

        assert!(
            result == Err(ModalUpdateError::CancellationNotSupported)
                && snapshot.actions.is_empty()
                && snapshot
                    .semantic_snapshot
                    .progress
                    .is_some_and(|progress| !progress.cancellation_available)
                && snapshot.progress.is_some_and(|progress| {
                    !progress.cancellation_capable && !progress.cancellation_enabled
                })
                && owner
                    .active
                    .as_ref()
                    .is_some_and(|active| active.update_generation == 0)
        );
    }

    #[test]
    fn queued_programmatic_progress_rejects_cancellation_availability_update_without_mutation() {
        let mut owner = ModalWindowOwner::new_for_test(WindowId::from(1));
        let weak = WeakEntity::new_invalid();
        let outcomes = Rc::new(RefCell::new(Vec::new()));
        let (active, _) = owner
            .submit(
                test_request("active", AnyWeakEntity::new_invalid(), outcomes),
                weak.clone(),
            )
            .expect("blocking alert should open");
        let (queued, _) = owner
            .submit(
                programmatic_progress_request(AnyWeakEntity::new_invalid()),
                weak.clone(),
            )
            .expect("programmatic progress should queue");

        let result = owner.update_progress(
            queued,
            0,
            ProgressDialogUpdate::new().cancellation_enabled(true),
        );
        owner
            .close_active(
                active,
                InternalOutcome::Dismissed(ModalCloseReason::Programmatic),
                ModalCloseReason::Programmatic,
            )
            .expect("blocking alert should close");
        owner.promote(weak);
        let snapshot = owner
            .render_snapshot()
            .expect("queued progress should promote");

        assert!(
            result == Err(ModalUpdateError::CancellationNotSupported)
                && snapshot.presentation == queued
                && snapshot.actions.is_empty()
                && snapshot
                    .semantic_snapshot
                    .progress
                    .is_some_and(|progress| !progress.cancellation_available)
                && snapshot.progress.is_some_and(|progress| {
                    !progress.cancellation_capable && !progress.cancellation_enabled
                })
                && owner
                    .active
                    .as_ref()
                    .is_some_and(|active| active.update_generation == 0)
        );
    }

    #[test]
    fn disabling_cancellable_progress_invalidates_only_the_current_attempt() {
        let mut owner = ModalWindowOwner::new_for_test(WindowId::from(1));
        let weak = WeakEntity::new_invalid();
        let request = progress_request(AnyWeakEntity::new_invalid());
        let completion = request.completion.clone();
        let (presentation, _) = owner
            .submit(request, weak.clone())
            .expect("cancellable progress should open");
        owner
            .request_action(presentation, 0, ModalActivationSource::Escape, weak.clone())
            .expect("first cancellation should be requested");
        owner
            .apply_progress_cancel_decision(presentation, 1, ProgressCancelDecision::Pending)
            .expect("first cancellation should become pending");

        owner
            .update_progress(
                presentation,
                0,
                ProgressDialogUpdate::new().cancellation_enabled(false),
            )
            .expect("cancellation should disable");
        let stale_attempt =
            owner.apply_progress_cancel_decision(presentation, 1, ProgressCancelDecision::Allow);
        owner
            .update_progress(
                presentation,
                1,
                ProgressDialogUpdate::new().cancellation_enabled(true),
            )
            .expect("cancellation should re-enable");
        let next_attempt =
            owner.request_action(presentation, 0, ModalActivationSource::CommandPeriod, weak);

        assert!(
            matches!(stale_attempt, Err(ModalTerminalOutcomeError::Stale(_)))
                && next_attempt.is_ok()
                && completion.status() == CompletionStatus::Pending
                && owner.active.as_ref().is_some_and(|active| {
                    active.id == presentation
                        && active.progress.as_ref().is_some_and(|progress| {
                            progress.cancellation_capable && progress.cancellation_enabled
                        })
                        && matches!(
                            active.state,
                            RuntimeState::ProgressActionRequested {
                                attempt: 3,
                                source: ModalActivationSource::CommandPeriod,
                            }
                        )
                })
        );
    }

    #[test]
    fn stale_progress_update_generation_cannot_overwrite_newer_status() {
        let mut owner = ModalWindowOwner::new_for_test(WindowId::from(1));
        let weak = WeakEntity::new_invalid();
        let (presentation, _) = owner
            .submit(progress_request(AnyWeakEntity::new_invalid()), weak)
            .expect("progress should open");
        owner
            .update_progress(
                presentation,
                0,
                ProgressDialogUpdate::new().status("New status"),
            )
            .expect("first update should succeed");

        let stale = owner.update_progress(
            presentation,
            0,
            ProgressDialogUpdate::new().status("Stale status"),
        );

        assert_eq!(
            stale,
            Err(ModalUpdateError::StaleUpdate {
                attempted: 0,
                current: 1
            })
        );
    }

    #[gpui::test]
    fn terminal_progress_outcome_is_delivered_once(cx: &mut TestAppContext) {
        let outcomes = Rc::new(RefCell::new(Vec::new()));
        let sink_outcomes = outcomes.clone();
        let request = PreparedModalRequest::new(
            ModalId::new("progress"),
            ModalKind::Progress,
            Vec::new(),
            PreparedModalSemantics::Progress {
                accessibility_title: "Progress".into(),
                visible_title: "Progress".into(),
                status: "Working".into(),
                detail: None,
                progress: ProgressState::Indeterminate,
                cancellation_capable: false,
            },
            PreparedFocusIntent::Surface,
            AnyWeakEntity::new_invalid(),
            Box::new(move |outcome, _| {
                if let InternalOutcome::Progress(outcome) = outcome {
                    sink_outcomes.borrow_mut().push(outcome);
                }
            }),
        );
        let owner = cx.new(|cx| ModalWindowOwner::new(WindowId::from(1), cx));
        let weak = owner.downgrade();
        let (presentation, _) = owner
            .update(cx, |owner, _| owner.submit(request, weak))
            .expect("progress should open");
        let effects = owner
            .update(cx, |owner, _| {
                owner.close_active(
                    presentation,
                    InternalOutcome::Progress(ProgressDialogOutcome::Completed),
                    ModalCloseReason::Programmatic,
                )
            })
            .expect("progress should close");
        cx.update(|cx| run_effects(effects, cx));

        let duplicate = owner.update(cx, |owner, _| {
            owner.close_active(
                presentation,
                InternalOutcome::Progress(ProgressDialogOutcome::Failed),
                ModalCloseReason::Programmatic,
            )
        });

        assert!(
            duplicate.is_err()
                && outcomes.borrow().as_slice() == [ProgressDialogOutcome::Completed]
        );
    }

    #[gpui::test]
    fn programmatic_only_deadline_expires_deterministically(cx: &mut TestAppContext) {
        let outcomes = Rc::new(RefCell::new(Vec::new()));
        let sink_outcomes = outcomes.clone();
        let request = PreparedModalRequest::new(
            ModalId::new("deadline"),
            ModalKind::Progress,
            Vec::new(),
            PreparedModalSemantics::Progress {
                accessibility_title: "Progress".into(),
                visible_title: "Progress".into(),
                status: "Working".into(),
                detail: None,
                progress: ProgressState::Indeterminate,
                cancellation_capable: false,
            },
            PreparedFocusIntent::Surface,
            AnyWeakEntity::new_invalid(),
            Box::new(move |outcome, _| {
                if let InternalOutcome::Progress(outcome) = outcome {
                    sink_outcomes.borrow_mut().push(outcome);
                }
            }),
        )
        .with_programmatic_deadline(Some(Duration::from_secs(5)));
        let owner = cx.new(|cx| ModalWindowOwner::new(WindowId::from(1), cx));
        let weak = owner.downgrade();
        let (_, effects) = owner
            .update(cx, |owner, _| owner.submit(request, weak))
            .expect("programmatic-only progress should open");
        cx.update(|cx| run_effects(effects, cx));

        cx.executor().advance_clock(Duration::from_secs(5));
        cx.run_until_parked();

        assert_eq!(
            outcomes.borrow().as_slice(),
            [ProgressDialogOutcome::DeadlineExpired]
        );
    }

    #[gpui::test]
    fn predecessor_result_can_dismiss_queued_successor_without_opening_it(cx: &mut TestAppContext) {
        let owner = cx.new(|cx| ModalWindowOwner::new(WindowId::from(1), cx));
        let weak = owner.downgrade();
        let trace = Rc::new(RefCell::new(Vec::new()));
        let successor_handle = Rc::new(RefCell::new(None));
        let callback_handle = successor_handle.clone();
        let mut predecessor = test_request(
            "predecessor",
            AnyWeakEntity::new_invalid(),
            Rc::new(RefCell::new(Vec::new())),
        );
        predecessor.result_sink = Some(Box::new(move |_, cx| {
            let handle = callback_handle.borrow().clone();
            let handle = handle.expect("queued successor handle should be retained");
            dismiss_retained_handle_for_test(&handle, cx);
        }));
        let (predecessor_id, _) = owner
            .update(cx, |state, _| state.submit(predecessor, weak.clone()))
            .expect("predecessor should open");
        let successor = traced_request("successor", AnyWeakEntity::new_invalid(), trace.clone());
        let successor_completion = successor.completion.clone();
        let (successor_id, _) = owner
            .update(cx, |state, _| state.submit(successor, weak))
            .expect("successor should queue");
        *successor_handle.borrow_mut() = Some(ModalPresentationHandle {
            owner: owner.clone(),
            window_id: WindowId::from(1),
            presentation: successor_id,
            completion: successor_completion,
        });
        let effects = owner
            .update(cx, |state, _| {
                state.close_active(
                    predecessor_id,
                    InternalOutcome::Dismissed(ModalCloseReason::Programmatic),
                    ModalCloseReason::Programmatic,
                )
            })
            .expect("predecessor should close");

        cx.update(|cx| settle_owner(&owner, effects, cx));

        assert_eq!(
            trace.borrow().as_slice(),
            ["successor:result", "successor:closed"]
        );
    }

    #[gpui::test]
    fn predecessor_lifecycle_can_dismiss_queued_successor_without_opening_it(
        cx: &mut TestAppContext,
    ) {
        let owner = cx.new(|cx| ModalWindowOwner::new(WindowId::from(1), cx));
        let weak = owner.downgrade();
        let trace = Rc::new(RefCell::new(Vec::new()));
        let successor_handle = Rc::new(RefCell::new(None));
        let callback_handle = successor_handle.clone();
        let predecessor = test_request(
            "predecessor",
            AnyWeakEntity::new_invalid(),
            Rc::new(RefCell::new(Vec::new())),
        )
        .with_lifecycle(Some(Rc::new(move |event, cx| {
            if matches!(event, ModalLifecycleEvent::Closing(_)) {
                let handle = callback_handle
                    .borrow()
                    .clone()
                    .expect("queued successor handle should be retained");
                dismiss_retained_handle_for_test(&handle, cx);
            }
        })));
        let (predecessor_id, _) = owner
            .update(cx, |state, _| state.submit(predecessor, weak.clone()))
            .expect("predecessor should open");
        let successor = traced_request("successor", AnyWeakEntity::new_invalid(), trace.clone());
        let successor_completion = successor.completion.clone();
        let (successor_id, _) = owner
            .update(cx, |state, _| state.submit(successor, weak))
            .expect("successor should queue");
        *successor_handle.borrow_mut() = Some(ModalPresentationHandle {
            owner: owner.clone(),
            window_id: WindowId::from(1),
            presentation: successor_id,
            completion: successor_completion,
        });
        let effects = owner
            .update(cx, |state, _| {
                state.close_active(
                    predecessor_id,
                    InternalOutcome::Dismissed(ModalCloseReason::Programmatic),
                    ModalCloseReason::Programmatic,
                )
            })
            .expect("predecessor should close");

        cx.update(|cx| settle_owner(&owner, effects, cx));

        assert_eq!(
            trace.borrow().as_slice(),
            ["successor:result", "successor:closed"]
        );
    }

    #[gpui::test]
    fn reentrant_submission_stays_behind_surviving_reserved_fifo_head(cx: &mut TestAppContext) {
        let owner = cx.new(|cx| ModalWindowOwner::new(WindowId::from(1), cx));
        let weak = owner.downgrade();
        let callback_third = Rc::new(Cell::new(None));
        let callback_third_sink = callback_third.clone();
        let callback_owner = owner.downgrade();
        let outcomes = Rc::new(RefCell::new(Vec::new()));
        let mut first = test_request(
            "first",
            AnyWeakEntity::new_invalid(),
            Rc::new(RefCell::new(Vec::new())),
        );
        first.result_sink = Some(Box::new(move |_, cx| {
            let Some(owner) = callback_owner.upgrade() else {
                return;
            };
            let third = test_request(
                "third",
                AnyWeakEntity::new_invalid(),
                Rc::new(RefCell::new(Vec::new())),
            );
            let weak = owner.downgrade();
            if let Ok((presentation, effects)) =
                owner.update(cx, |owner, _| owner.submit(third, weak))
            {
                callback_third_sink.set(Some(presentation));
                run_effects(effects, cx);
            }
        }));
        let (first_id, _) = owner
            .update(cx, |owner, _| owner.submit(first, weak.clone()))
            .expect("first should open");
        let (second_id, _) = owner
            .update(cx, |owner, _| {
                owner.submit(
                    test_request("second", AnyWeakEntity::new_invalid(), outcomes),
                    weak.clone(),
                )
            })
            .expect("second should queue");
        let effects = owner
            .update(cx, |owner, _| {
                owner.close_active(
                    first_id,
                    InternalOutcome::Dismissed(ModalCloseReason::Programmatic),
                    ModalCloseReason::Programmatic,
                )
            })
            .expect("first should close");

        cx.update(|cx| settle_owner(&owner, effects, cx));

        let state = owner.read_with(cx, |owner, _| {
            (
                owner.active.as_ref().map(|active| active.id),
                owner.queue.front().map(|queued| queued.id),
            )
        });
        assert_eq!(state, (Some(second_id), callback_third.get()));
    }

    #[gpui::test]
    fn reentrant_submission_promotes_only_after_reserved_head_queued_close_effects(
        cx: &mut TestAppContext,
    ) {
        let owner = cx.new(|cx| ModalWindowOwner::new(WindowId::from(1), cx));
        let weak = owner.downgrade();
        let trace = Rc::new(RefCell::new(Vec::new()));
        let successor_handle = Rc::new(RefCell::new(None));
        let callback_handle = successor_handle.clone();
        let callback_owner = owner.downgrade();
        let callback_trace = trace.clone();
        let mut predecessor = test_request(
            "predecessor",
            AnyWeakEntity::new_invalid(),
            Rc::new(RefCell::new(Vec::new())),
        );
        predecessor.result_sink = Some(Box::new(move |_, cx| {
            let handle = callback_handle
                .borrow()
                .clone()
                .expect("queued successor handle should be retained");
            dismiss_retained_handle_for_test(&handle, cx);
            let owner = callback_owner
                .upgrade()
                .expect("modal owner should survive callback");
            let following = traced_request(
                "following",
                AnyWeakEntity::new_invalid(),
                callback_trace.clone(),
            );
            let weak = owner.downgrade();
            let (_, effects) = owner
                .update(cx, |state, _| state.submit(following, weak))
                .expect("following request should submit");
            run_effects(effects, cx);
        }));
        let (predecessor_id, _) = owner
            .update(cx, |state, _| state.submit(predecessor, weak.clone()))
            .expect("predecessor should open");
        let successor = traced_request("successor", AnyWeakEntity::new_invalid(), trace.clone());
        let successor_completion = successor.completion.clone();
        let (successor_id, _) = owner
            .update(cx, |state, _| state.submit(successor, weak))
            .expect("successor should queue");
        *successor_handle.borrow_mut() = Some(ModalPresentationHandle {
            owner: owner.clone(),
            window_id: WindowId::from(1),
            presentation: successor_id,
            completion: successor_completion,
        });
        let effects = owner
            .update(cx, |state, _| {
                state.close_active(
                    predecessor_id,
                    InternalOutcome::Dismissed(ModalCloseReason::Programmatic),
                    ModalCloseReason::Programmatic,
                )
            })
            .expect("predecessor should close");

        cx.update(|cx| settle_owner(&owner, effects, cx));

        assert_eq!(
            trace.borrow().as_slice(),
            ["successor:result", "successor:closed", "following:opened"]
        );
    }

    #[gpui::test]
    fn nested_replacement_skips_superseded_successor_startup_effects(cx: &mut TestAppContext) {
        let owner = cx.new(|cx| ModalWindowOwner::new(WindowId::from(1), cx));
        let weak = owner.downgrade();
        let trace = Rc::new(RefCell::new(Vec::new()));
        let replacement_id = Rc::new(Cell::new(None));
        let callback_replacement_id = replacement_id.clone();
        let callback_owner = owner.downgrade();
        let callback_trace = trace.clone();
        let mut predecessor = traced_request("a", AnyWeakEntity::new_invalid(), trace.clone());
        predecessor.result_sink = Some(Box::new(move |outcome, cx| {
            assert!(matches!(
                outcome,
                InternalOutcome::Dismissed(ModalCloseReason::Replaced)
            ));
            callback_trace.borrow_mut().push("a:result".into());
            let owner = callback_owner
                .upgrade()
                .expect("modal owner should survive the terminal callback");
            let weak = owner.downgrade();
            let replacement =
                traced_request("c", AnyWeakEntity::new_invalid(), callback_trace.clone());
            let (presentation, effects) = owner
                .update(cx, |state, _| state.replace_active(replacement, weak))
                .expect("terminal callback should replace b with c");
            callback_replacement_id.set(Some(presentation));
            settle_owner(&owner, effects, cx);
        }));
        let (predecessor_id, startup) = owner
            .update(cx, |state, _| state.submit(predecessor, weak.clone()))
            .expect("a should open");
        cx.update(|cx| defer_owner_effects(&owner, startup, cx));
        cx.run_until_parked();

        let mut superseded = programmatic_progress_request(AnyWeakEntity::new_invalid());
        let superseded_completion = superseded.completion.clone();
        let result_trace = trace.clone();
        superseded.result_sink = Some(Box::new(move |outcome, _| {
            assert!(matches!(
                outcome,
                InternalOutcome::Progress(ProgressDialogOutcome::Replaced)
            ));
            result_trace.borrow_mut().push("b:result".into());
        }));
        let lifecycle_trace = trace.clone();
        superseded.lifecycle = Some(Rc::new(move |event, _| {
            let transition = match event {
                ModalLifecycleEvent::Opened(_) => "opened",
                ModalLifecycleEvent::Closing(_) => "closing",
                ModalLifecycleEvent::Closed(_, ModalCloseReason::Replaced) => "closed",
                ModalLifecycleEvent::ActionRequested(_)
                | ModalLifecycleEvent::Pending(_)
                | ModalLifecycleEvent::Closed(_, _) => return,
            };
            lifecycle_trace.borrow_mut().push(format!("b:{transition}"));
        }));
        let (superseded_id, effects) = owner
            .update(cx, |state, _| {
                state.replace_active(superseded, weak.clone())
            })
            .expect("a should be replaceable with b");
        cx.update(|cx| settle_owner(&owner, effects, cx));
        cx.run_until_parked();

        cx.executor().advance_clock(Duration::from_secs(30));
        cx.run_until_parked();

        let replacement_id = replacement_id
            .get()
            .expect("a terminal callback should install c");
        let active = owner.read_with(cx, |state, _| state.active.as_ref().map(|active| active.id));
        assert_eq!(
            (
                trace.borrow().clone(),
                predecessor_id.value(),
                superseded_id.value(),
                replacement_id.value(),
                superseded_completion.status(),
                active,
            ),
            (
                vec![
                    "a:opened".to_owned(),
                    "a:closing".to_owned(),
                    "a:result".to_owned(),
                    "b:closing".to_owned(),
                    "b:result".to_owned(),
                    "b:closed".to_owned(),
                    "c:opened".to_owned(),
                    "a:closed".to_owned(),
                ],
                1,
                2,
                3,
                CompletionStatus::Replaced,
                Some(replacement_id),
            )
        );
    }

    #[gpui::test]
    fn promoted_successor_opened_callback_can_dismiss_itself_in_documented_order(
        cx: &mut TestAppContext,
    ) {
        let owner = cx.new(|cx| ModalWindowOwner::new(WindowId::from(1), cx));
        let weak = owner.downgrade();
        let trace = Rc::new(RefCell::new(Vec::new()));
        let lifecycle_trace = trace.clone();
        let result_trace = trace.clone();
        let successor_handle = Rc::new(RefCell::new(None));
        let callback_handle = successor_handle.clone();
        let mut successor = test_request(
            "successor",
            AnyWeakEntity::new_invalid(),
            Rc::new(RefCell::new(Vec::new())),
        );
        successor.result_sink = Some(Box::new(move |_, _| {
            result_trace.borrow_mut().push("result");
        }));
        successor.lifecycle = Some(Rc::new(move |event, cx| match event {
            ModalLifecycleEvent::Opened(_) => {
                lifecycle_trace.borrow_mut().push("opened");
                let handle = callback_handle
                    .borrow()
                    .clone()
                    .expect("promoted successor handle should be retained");
                dismiss_retained_handle_for_test(&handle, cx);
            }
            ModalLifecycleEvent::Closing(_) => lifecycle_trace.borrow_mut().push("closing"),
            ModalLifecycleEvent::Closed(_, _) => lifecycle_trace.borrow_mut().push("closed"),
            ModalLifecycleEvent::ActionRequested(_) | ModalLifecycleEvent::Pending(_) => {}
        }));
        let (predecessor_id, _) = owner
            .update(cx, |state, _| {
                state.submit(
                    test_request(
                        "predecessor",
                        AnyWeakEntity::new_invalid(),
                        Rc::new(RefCell::new(Vec::new())),
                    ),
                    weak.clone(),
                )
            })
            .expect("predecessor should open");
        let successor_completion = successor.completion.clone();
        let (successor_id, _) = owner
            .update(cx, |state, _| state.submit(successor, weak))
            .expect("successor should queue");
        *successor_handle.borrow_mut() = Some(ModalPresentationHandle {
            owner: owner.clone(),
            window_id: WindowId::from(1),
            presentation: successor_id,
            completion: successor_completion,
        });
        let effects = owner
            .update(cx, |state, _| {
                state.close_active(
                    predecessor_id,
                    InternalOutcome::Dismissed(ModalCloseReason::Programmatic),
                    ModalCloseReason::Programmatic,
                )
            })
            .expect("predecessor should close");

        cx.update(|cx| settle_owner(&owner, effects, cx));

        assert_eq!(
            trace.borrow().as_slice(),
            ["opened", "closing", "result", "closed"]
        );
    }

    #[gpui::test]
    fn reentrant_result_callback_observes_promoted_successor(cx: &mut TestAppContext) {
        let owner = cx.new(|cx| ModalWindowOwner::new(WindowId::from(1), cx));
        let weak = owner.downgrade();
        let callback_third = Rc::new(Cell::new(None));
        let callback_third_sink = callback_third.clone();
        let callback_owner = owner.downgrade();
        let outcomes = Rc::new(RefCell::new(Vec::new()));
        let first = PreparedModalRequest::new(
            ModalId::new("first"),
            ModalKind::Alert,
            Vec::new(),
            PreparedModalSemantics::Alert {
                accessibility_title: "First".into(),
                visible_title: "First".into(),
                message: "First".into(),
                detail: None,
                intent: AlertIntent::Informational,
                accessory: None,
                suppression: None,
                default_action: None,
                cancel_action: None,
            },
            PreparedFocusIntent::Surface,
            AnyWeakEntity::new_invalid(),
            Box::new(move |_, cx| {
                let Some(owner) = callback_owner.upgrade() else {
                    return;
                };
                let third = test_request(
                    "third",
                    AnyWeakEntity::new_invalid(),
                    Rc::new(RefCell::new(Vec::new())),
                );
                let weak = owner.downgrade();
                if let Ok((presentation, _)) =
                    owner.update(cx, |owner, _| owner.submit(third, weak))
                {
                    callback_third_sink.set(Some(presentation));
                }
            }),
        );
        let (first_id, _) = owner
            .update(cx, |owner, _| owner.submit(first, weak.clone()))
            .expect("first should open");
        let (second_id, _) = owner
            .update(cx, |owner, _| {
                owner.submit(
                    test_request("second", AnyWeakEntity::new_invalid(), outcomes),
                    weak.clone(),
                )
            })
            .expect("second should queue");
        let effects = owner
            .update(cx, |owner, _| {
                owner.close_active(
                    first_id,
                    InternalOutcome::Dismissed(ModalCloseReason::Programmatic),
                    ModalCloseReason::Programmatic,
                )
            })
            .expect("first should close");
        cx.update(|cx| settle_owner(&owner, effects, cx));

        let state = owner.read_with(cx, |owner, _| {
            (
                owner.active.as_ref().map(|active| active.id),
                owner.queue.front().map(|queued| queued.id),
            )
        });
        assert_eq!(state, (Some(second_id), callback_third.get()));
    }

    #[test]
    fn alert_logical_semantic_snapshot_retains_every_required_fact() {
        let mut owner = ModalWindowOwner::new_for_test(WindowId::from(1));
        let actions = vec![
            ModalAction::new("delete", "Delete", ModalActionRole::Affirmative, "delete")
                .with_intent(ModalActionIntent::Destructive)
                .default_action(true),
            ModalAction::new("cancel", "Cancel", ModalActionRole::Cancel, "cancel"),
        ];
        let request = PreparedModalRequest::new(
            ModalId::new("delete-alert"),
            ModalKind::Alert,
            PreparedModalRequest::erase_actions(actions),
            PreparedModalSemantics::Alert {
                accessibility_title: "Delete the file?".into(),
                visible_title: "Delete File".into(),
                message: "This cannot be undone.".into(),
                detail: Some("The original will be removed.".into()),
                intent: AlertIntent::Critical,
                accessory: Some(AlertAccessory::Icon {
                    accessibility_name: "Warning".into(),
                    image: None,
                }),
                suppression: Some(("Do not ask again".into(), false)),
                default_action: Some(0),
                cancel_action: Some(1),
            },
            PreparedFocusIntent::Action(1),
            AnyWeakEntity::new_invalid(),
            Box::new(|_, _| {}),
        );
        owner
            .submit(request, WeakEntity::new_invalid())
            .expect("alert should open");

        let snapshot = owner
            .render_snapshot()
            .expect("active alert should have a semantic snapshot")
            .semantic_snapshot;

        assert_eq!(
            snapshot,
            LogicalModalSemanticSnapshot {
                id: ModalId::new("delete-alert"),
                role: LogicalModalRole::Alert,
                modal: true,
                accessibility_title: "Delete the file?".into(),
                visible_title: "Delete File".into(),
                description: Some("This cannot be undone.".into()),
                secondary_detail: Some("The original will be removed.".into()),
                alert_intent: Some(AlertIntent::Critical),
                accessory_name: Some("Warning".into()),
                suppression_label: Some("Do not ask again".into()),
                actions: vec![
                    LogicalActionSemanticSnapshot {
                        name: "Delete".into(),
                        role: ModalActionRole::Affirmative,
                        intent: ModalActionIntent::Destructive,
                        emphasis: ModalActionEmphasis::Standard,
                        enabled: true,
                        is_default: true,
                        debug_identity: "delete".into(),
                    },
                    LogicalActionSemanticSnapshot {
                        name: "Cancel".into(),
                        role: ModalActionRole::Cancel,
                        intent: ModalActionIntent::Ordinary,
                        emphasis: ModalActionEmphasis::Standard,
                        enabled: true,
                        is_default: false,
                        debug_identity: "cancel".into(),
                    },
                ],
                default_action: Some("delete".into()),
                cancel_action: Some("cancel".into()),
                progress: None,
                focus_entry: LogicalFocusEntry::Action("cancel".into()),
                focus_contained: true,
                underlay_excluded: true,
            }
        );
    }

    #[gpui::test]
    fn dialog_logical_semantic_snapshot_retains_role_relationships_and_body_focus(
        cx: &mut TestAppContext,
    ) {
        let mut owner = ModalWindowOwner::new_for_test(WindowId::from(1));
        let body_focus = cx.new(|cx| cx.focus_handle());
        let body_focus = body_focus.read_with(cx, |focus, _| focus.clone());
        let request = PreparedModalRequest::new(
            ModalId::new("settings-dialog"),
            ModalKind::Dialog,
            PreparedModalRequest::erase_actions(vec![ModalAction::new(
                "cancel",
                "Cancel",
                ModalActionRole::Cancel,
                "cancel",
            )]),
            PreparedModalSemantics::Dialog {
                accessibility_title: "Edit workspace settings".into(),
                visible_title: "Workspace Settings".into(),
                description: Some("Changes apply to this workspace.".into()),
                default_action: None,
                cancel_action: Some(0),
            },
            PreparedFocusIntent::Body(body_focus),
            AnyWeakEntity::new_invalid(),
            Box::new(|_, _| {}),
        );
        owner
            .submit(request, WeakEntity::new_invalid())
            .expect("dialog should open");

        let snapshot = owner
            .render_snapshot()
            .expect("active dialog should have a semantic snapshot")
            .semantic_snapshot;

        assert_eq!(
            (
                snapshot.role,
                snapshot.modal,
                snapshot.accessibility_title.as_ref(),
                snapshot.visible_title.as_ref(),
                snapshot.description.as_ref().map(|value| value.as_ref()),
                snapshot.default_action,
                snapshot.cancel_action,
                snapshot.focus_entry,
                snapshot.focus_contained,
                snapshot.underlay_excluded,
            ),
            (
                LogicalModalRole::Dialog,
                true,
                "Edit workspace settings",
                "Workspace Settings",
                Some("Changes apply to this workspace."),
                None,
                Some("cancel".into()),
                LogicalFocusEntry::Body,
                true,
                true,
            )
        );
    }

    #[test]
    fn progress_logical_semantic_snapshot_tracks_value_status_and_cancellation() {
        let mut owner = ModalWindowOwner::new_for_test(WindowId::from(1));
        let weak = WeakEntity::new_invalid();
        let (presentation, _) = owner
            .submit(progress_request(AnyWeakEntity::new_invalid()), weak)
            .expect("progress should open");

        let initial_snapshot = owner
            .render_snapshot()
            .expect("active progress should have an initial semantic snapshot")
            .semantic_snapshot;

        assert_eq!(
            initial_snapshot,
            LogicalModalSemanticSnapshot {
                id: ModalId::new("progress"),
                role: LogicalModalRole::Progress,
                modal: true,
                accessibility_title: "Progress".into(),
                visible_title: "Progress".into(),
                description: Some("Working".into()),
                secondary_detail: None,
                alert_intent: None,
                accessory_name: None,
                suppression_label: None,
                actions: vec![LogicalActionSemanticSnapshot {
                    name: "Cancel".into(),
                    role: ModalActionRole::Cancel,
                    intent: ModalActionIntent::Ordinary,
                    emphasis: ModalActionEmphasis::Standard,
                    enabled: true,
                    is_default: false,
                    debug_identity: "cancel-progress".into(),
                }],
                default_action: None,
                cancel_action: Some("cancel-progress".into()),
                progress: Some(LogicalProgressSemanticSnapshot {
                    status: "Working".into(),
                    value: None,
                    indeterminate: true,
                    cancellation_available: true,
                }),
                focus_entry: LogicalFocusEntry::Action("cancel-progress".into()),
                focus_contained: true,
                underlay_excluded: true,
            }
        );

        owner
            .update_progress(
                presentation,
                0,
                ProgressDialogUpdate::new()
                    .status("Halfway")
                    .detail(Some("Two items remain"))
                    .progress(ProgressState::Determinate(
                        DeterminateProgress::new(0.5).expect("finite progress should normalize"),
                    ))
                    .cancellation_enabled(false),
            )
            .expect("progress update should succeed");

        let snapshot = owner
            .render_snapshot()
            .expect("active progress should have a semantic snapshot")
            .semantic_snapshot;

        assert_eq!(
            (
                snapshot.role,
                snapshot.modal,
                snapshot.description.as_ref().map(|value| value.as_ref()),
                snapshot
                    .secondary_detail
                    .as_ref()
                    .map(|value| value.as_ref()),
                snapshot.progress,
                snapshot.actions[0].enabled,
                snapshot.cancel_action,
                snapshot.focus_entry,
                snapshot.underlay_excluded,
            ),
            (
                LogicalModalRole::Progress,
                true,
                Some("Halfway"),
                Some("Two items remain"),
                Some(LogicalProgressSemanticSnapshot {
                    status: "Halfway".into(),
                    value: Some(0.5),
                    indeterminate: false,
                    cancellation_available: false,
                }),
                false,
                Some("cancel-progress".into()),
                LogicalFocusEntry::Surface,
                true,
            )
        );
    }
}
