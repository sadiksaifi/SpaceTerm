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
fn repeated_settled_presentations_do_not_retain_caller_release_callbacks(cx: &mut TestAppContext) {
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
        duplicate.is_err() && outcomes.borrow().as_slice() == [ProgressDialogOutcome::Completed]
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
fn predecessor_lifecycle_can_dismiss_queued_successor_without_opening_it(cx: &mut TestAppContext) {
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
        if let Ok((presentation, effects)) = owner.update(cx, |owner, _| owner.submit(third, weak))
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
        let replacement = traced_request("c", AnyWeakEntity::new_invalid(), callback_trace.clone());
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
fn reentrant_dialog_lifecycle_close_skips_stale_action_handler(cx: &mut TestAppContext) {
    let owner = cx.new(|cx| ModalWindowOwner::new(WindowId::from(1), cx));
    let weak = owner.downgrade();
    let handle = Rc::new(RefCell::new(None));
    let lifecycle_handle = handle.clone();
    let handler_calls = Rc::new(Cell::new(0));
    let handler_calls_sink = handler_calls.clone();
    let request = dialog_request(AnyWeakEntity::new_invalid())
        .with_dialog_action(Rc::new(move |_, _, _, _, _| {
            handler_calls_sink.set(handler_calls_sink.get() + 1);
            DialogCloseDecision::Pending
        }))
        .with_lifecycle(Some(Rc::new(move |event, cx| {
            if matches!(event, ModalLifecycleEvent::ActionRequested(_)) {
                let handle = lifecycle_handle
                    .borrow()
                    .clone()
                    .expect("Dialog handle should be retained");
                dismiss_retained_handle_for_test(&handle, cx);
            }
        })));
    let completion = request.completion.clone();
    let (presentation, startup) = owner
        .update(cx, |state, _| state.submit(request, weak.clone()))
        .expect("Dialog should open");
    *handle.borrow_mut() = Some(ModalPresentationHandle {
        owner: owner.clone(),
        window_id: WindowId::from(1),
        presentation,
        completion,
    });
    cx.update(|cx| defer_owner_effects(&owner, startup, cx));
    cx.run_until_parked();

    let effects = owner
        .update(cx, |state, _| {
            state.request_action(presentation, 0, ModalActivationSource::Return, weak.clone())
        })
        .expect("Dialog action should enter lifecycle delivery");
    cx.update(|cx| settle_owner(&owner, effects, cx));
    cx.run_until_parked();

    assert_eq!(handler_calls.get(), 0);
}

#[gpui::test]
fn reentrant_progress_lifecycle_close_skips_stale_cancel_handler(cx: &mut TestAppContext) {
    let owner = cx.new(|cx| ModalWindowOwner::new(WindowId::from(1), cx));
    let weak = owner.downgrade();
    let handle = Rc::new(RefCell::new(None));
    let lifecycle_handle = handle.clone();
    let handler_calls = Rc::new(Cell::new(0));
    let handler_calls_sink = handler_calls.clone();
    let request = progress_request(AnyWeakEntity::new_invalid())
        .with_progress_cancel(Rc::new(move |_, _, _| {
            handler_calls_sink.set(handler_calls_sink.get() + 1);
            ProgressCancelDecision::Pending
        }))
        .with_lifecycle(Some(Rc::new(move |event, cx| {
            if matches!(event, ModalLifecycleEvent::ActionRequested(_)) {
                let handle = lifecycle_handle
                    .borrow()
                    .clone()
                    .expect("ProgressDialog handle should be retained");
                dismiss_retained_handle_for_test(&handle, cx);
            }
        })));
    let completion = request.completion.clone();
    let (presentation, startup) = owner
        .update(cx, |state, _| state.submit(request, weak.clone()))
        .expect("ProgressDialog should open");
    *handle.borrow_mut() = Some(ModalPresentationHandle {
        owner: owner.clone(),
        window_id: WindowId::from(1),
        presentation,
        completion,
    });
    cx.update(|cx| defer_owner_effects(&owner, startup, cx));
    cx.run_until_parked();

    let effects = owner
        .update(cx, |state, _| {
            state.request_action(presentation, 0, ModalActivationSource::Escape, weak.clone())
        })
        .expect("ProgressDialog cancellation should enter lifecycle delivery");
    cx.update(|cx| settle_owner(&owner, effects, cx));
    cx.run_until_parked();

    assert_eq!(handler_calls.get(), 0);
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
            if let Ok((presentation, _)) = owner.update(cx, |owner, _| owner.submit(third, weak)) {
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
