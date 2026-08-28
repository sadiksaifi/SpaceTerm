use std::time::Duration;

use gpui::{
    Context, FocusHandle, InteractiveElement as _, ParentElement as _, Render, TestAppContext,
    Window, div, px, rgba,
};
use spaceterm_ui::{
    Alert, AlertAccessory, AlertIntent, AlertOutcome, DeterminateProgress, Dialog,
    DialogCloseDecision, DialogCompletion, DialogFocusTarget, DialogInitialFocus, DialogOutcome,
    DialogSize, ModalAction, ModalActionEmphasis, ModalActionIntent, ModalActionRole,
    ModalActivationSource, ModalCloseReason, ModalDesktopPolicy, ModalDismissalError, ModalId,
    ModalLayer, ModalLifecycleEvent, ModalMetrics, ModalPaint, ModalPresentationError,
    ModalPresentationHandle, ModalPresentationId, ModalStaleGenerationError,
    ModalTerminalOutcomeError, ModalTextField, ModalTheme, ModalUpdateError, ModalValidationError,
    ProgressCancellation, ProgressCancellationCompletion, ProgressDialog, ProgressDialogHandle,
    ProgressDialogOutcome, ProgressDialogUpdate, ProgressState, TextDirection,
    install_modal_policy,
};

#[derive(Default)]
struct ReentrantCallbackCaller {
    opened_updates: usize,
    dismissal_updates: usize,
    completion_updates: usize,
    alert: Option<ModalPresentationHandle>,
    dialog: Option<DialogCompletion>,
}

impl ReentrantCallbackCaller {
    fn present_alert(&mut self, window: &Window, cx: &mut Context<Self>) {
        let opened_caller = cx.weak_entity();
        let dismissal_caller = cx.weak_entity();
        self.alert = Some(
            Alert::new(
                ModalId::new("reentrant-callback-alert"),
                "Reentrant callback Alert",
                "Reentrant Callback",
                "Callbacks may update their presenting entity.",
                vec![ModalAction::new(
                    (),
                    "OK",
                    ModalActionRole::Affirmative,
                    "reentrant-callback-alert-ok",
                )],
            )
            .present_with_lifecycle(
                window,
                cx,
                move |outcome, cx| {
                    if matches!(outcome, AlertOutcome::Dismissed { .. }) {
                        let _ =
                            dismissal_caller.update(cx, |caller, _| caller.dismissal_updates += 1);
                    }
                },
                move |event, cx| {
                    if matches!(event, ModalLifecycleEvent::Opened(_)) {
                        let _ = opened_caller.update(cx, |caller, _| caller.opened_updates += 1);
                    }
                },
            )
            .expect("Alert should present"),
        );
    }

    fn dismiss_alert(&mut self, window: &Window, cx: &mut Context<Self>) {
        self.alert
            .as_ref()
            .expect("Alert handle should be retained")
            .dismiss(window, cx)
            .expect("Alert should dismiss");
    }

    fn present_dialog(&mut self, window: &Window, cx: &mut Context<Self>) {
        let completion_caller = cx.weak_entity();
        self.dialog = Some(
            Dialog::new(
                ModalId::new("reentrant-callback-dialog"),
                "Reentrant callback Dialog",
                "Reentrant Callback",
                vec![save_action(), cancel_action()],
                DialogInitialFocus::Action(Decision::Save),
            )
            .present(
                window,
                cx,
                |_, _, _| DialogCloseDecision::Pending,
                move |outcome, cx| {
                    if outcome == DialogOutcome::ProgrammaticallyCompleted {
                        let _ = completion_caller
                            .update(cx, |caller, _| caller.completion_updates += 1);
                    }
                },
            )
            .expect("Dialog should present"),
        );
    }

    fn complete_dialog(&mut self, window: &Window, cx: &mut Context<Self>) {
        self.dialog
            .as_ref()
            .expect("Dialog completion should be retained")
            .complete(window, None, cx)
            .expect("Dialog should complete");
    }
}

impl Render for ReentrantCallbackCaller {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl gpui::IntoElement {
        div()
    }
}

struct PublicCustomDialogBody {
    focus: FocusHandle,
}

impl Render for PublicCustomDialogBody {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl gpui::IntoElement {
        DialogFocusTarget::new(
            div()
                .track_focus(&self.focus)
                .child("Caller-owned custom control"),
            self.focus.clone(),
        )
    }
}

#[gpui::test]
fn public_dialog_focus_target_wraps_custom_controls_without_scroll_types(cx: &mut TestAppContext) {
    let (body, cx) = cx.add_window_view(|_, cx| PublicCustomDialogBody {
        focus: cx.focus_handle().tab_stop(true),
    });
    let focus = body.read_with(cx, |body, _| body.focus.clone());

    cx.update(|window, _| {
        window.activate_window();
        focus.focus(window);
    });
    cx.run_until_parked();

    assert!(cx.update(|window, _| focus.is_focused(window)));
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum Decision {
    Save,
    Cancel,
}

fn save_action() -> ModalAction<Decision> {
    ModalAction::new(Decision::Save, "Save", ModalActionRole::Affirmative, "save")
        .default_action(true)
}

fn cancel_action() -> ModalAction<Decision> {
    ModalAction::new(
        Decision::Cancel,
        "Cancel",
        ModalActionRole::Cancel,
        "cancel",
    )
}

#[gpui::test]
fn opened_callback_updates_presenting_entity_after_present_returns(cx: &mut TestAppContext) {
    cx.update(|cx| install_modal_policy(cx, ModalDesktopPolicy::mac_os()));
    let (caller, cx) = cx.add_window_view(|_, _| ReentrantCallbackCaller::default());

    cx.update(|window, cx| {
        caller.update(cx, |caller, cx| {
            caller.present_alert(window, cx);
            assert_eq!(caller.opened_updates, 0);
        });
    });
    cx.update(|_, _| {});
    cx.run_until_parked();

    assert_eq!(caller.read_with(cx, |caller, _| caller.opened_updates), 1);
}

#[gpui::test]
fn dismissal_result_updates_entity_after_dismiss_returns(cx: &mut TestAppContext) {
    cx.update(|cx| install_modal_policy(cx, ModalDesktopPolicy::mac_os()));
    let (caller, cx) = cx.add_window_view(|_, _| ReentrantCallbackCaller::default());
    cx.update(|window, cx| {
        caller.update(cx, |caller, cx| caller.present_alert(window, cx));
    });
    cx.update(|_, _| {});
    cx.run_until_parked();

    cx.update(|window, cx| {
        caller.update(cx, |caller, cx| {
            caller.dismiss_alert(window, cx);
            assert_eq!(caller.dismissal_updates, 0);
        });
    });
    cx.update(|_, _| {});
    cx.run_until_parked();

    assert_eq!(
        caller.read_with(cx, |caller, _| caller.dismissal_updates),
        1
    );
}

#[gpui::test]
fn completion_result_updates_entity_after_complete_returns(cx: &mut TestAppContext) {
    cx.update(|cx| install_modal_policy(cx, ModalDesktopPolicy::mac_os()));
    let (caller, cx) = cx.add_window_view(|_, _| ReentrantCallbackCaller::default());
    cx.update(|window, cx| {
        caller.update(cx, |caller, cx| caller.present_dialog(window, cx));
    });
    cx.update(|_, _| {});
    cx.run_until_parked();

    cx.update(|window, cx| {
        caller.update(cx, |caller, cx| {
            caller.complete_dialog(window, cx);
            assert_eq!(caller.completion_updates, 0);
        });
    });
    cx.update(|_, _| {});
    cx.run_until_parked();

    assert_eq!(
        caller.read_with(cx, |caller, _| caller.completion_updates),
        1
    );
}

#[test]
fn public_modal_facades_construct_and_validate_without_internal_types() {
    let policy = ModalDesktopPolicy::mac_os();
    let alert = Alert::new(
        ModalId::new("replace-alert"),
        "Replace the file?",
        "Replace File",
        "A file with this name already exists.",
        vec![save_action(), cancel_action()],
    )
    .intent(AlertIntent::Warning)
    .accessory(AlertAccessory::icon("Replacement warning", None));
    let dialog = Dialog::new(
        ModalId::new("save-dialog"),
        "Save workspace settings",
        "Save Settings",
        vec![save_action(), cancel_action()],
        DialogInitialFocus::Action(Decision::Cancel),
    )
    .size(DialogSize::Regular);
    let progress = ProgressDialog::new(
        ModalId::new("save-progress"),
        "Saving workspace settings",
        "Saving Settings",
        "Writing configuration",
        ProgressState::Determinate(
            DeterminateProgress::new(0.5).expect("finite progress should normalize"),
        ),
        ProgressCancellation::Cancellable(cancel_action()),
    );
    let update = ProgressDialogUpdate::new()
        .status("Finalizing")
        .progress(ProgressState::Indeterminate);
    let programmatic = ProgressDialog::<Decision>::new(
        ModalId::new("bounded-progress"),
        "Completing required migration",
        "Completing Migration",
        "Applying changes",
        ProgressState::Indeterminate,
        ProgressCancellation::programmatic_only(Duration::from_secs(30)),
    );

    assert!(
        alert.validate(&policy).is_ok()
            && dialog.validate(&policy).is_ok()
            && progress.validate(&policy).is_ok()
            && update.validate().is_ok()
            && programmatic.validate(&policy).is_ok()
    );
}

#[test]
fn public_dialog_validation_rejects_affirmative_only_actions() {
    let dialog = Dialog::new(
        ModalId::new("affirmative-only-dialog"),
        "Save workspace settings",
        "Save Settings",
        vec![save_action()],
        DialogInitialFocus::Action(Decision::Save),
    );

    assert_eq!(
        dialog.validate(&ModalDesktopPolicy::mac_os()),
        Err(ModalValidationError::MissingSafeDismissal)
    );
}

#[test]
fn public_dialog_validation_rejects_disabled_cancel() {
    let dialog = Dialog::new(
        ModalId::new("disabled-cancel-dialog"),
        "Save workspace settings",
        "Save Settings",
        vec![save_action(), cancel_action().enabled(false)],
        DialogInitialFocus::Action(Decision::Save),
    );

    assert_eq!(
        dialog.validate(&ModalDesktopPolicy::mac_os()),
        Err(ModalValidationError::MissingSafeDismissal)
    );
}

#[test]
fn public_dialog_validation_rejects_destructive_cancel() {
    let dialog = Dialog::new(
        ModalId::new("destructive-cancel-dialog"),
        "Save workspace settings",
        "Save Settings",
        vec![
            save_action(),
            cancel_action().with_intent(ModalActionIntent::Destructive),
        ],
        DialogInitialFocus::Action(Decision::Save),
    );

    assert_eq!(
        dialog.validate(&ModalDesktopPolicy::mac_os()),
        Err(ModalValidationError::DestructiveCancelAction { index: 1 })
    );
}

#[test]
fn public_alert_validation_accepts_sole_acknowledgement() {
    let alert = Alert::new(
        ModalId::new("acknowledgement-alert"),
        "Settings saved",
        "Settings Saved",
        "Workspace settings were saved.",
        vec![ModalAction::new(
            Decision::Save,
            "OK",
            ModalActionRole::Affirmative,
            "okay",
        )],
    );

    assert_eq!(alert.validate(&ModalDesktopPolicy::mac_os()), Ok(()));
}

#[test]
fn public_progress_validation_accepts_installed_policy_deadline_boundary() {
    let progress = ProgressDialog::<Decision>::new(
        ModalId::new("deadline-boundary-progress"),
        "Completing required migration",
        "Completing Migration",
        "Applying changes",
        ProgressState::Indeterminate,
        ProgressCancellation::programmatic_only(Duration::from_secs(30 * 60)),
    );

    assert_eq!(progress.validate(&ModalDesktopPolicy::mac_os()), Ok(()));
}

#[test]
fn public_progress_validation_rejects_deadline_above_installed_policy_boundary() {
    let deadline = Duration::from_secs(30 * 60) + Duration::from_millis(1);
    let progress = ProgressDialog::<Decision>::new(
        ModalId::new("deadline-overflow-progress"),
        "Completing required migration",
        "Completing Migration",
        "Applying changes",
        ProgressState::Indeterminate,
        ProgressCancellation::programmatic_only(deadline),
    );

    assert_eq!(
        progress.validate(&ModalDesktopPolicy::mac_os()),
        Err(ModalValidationError::InvalidProgrammaticOnlyDeadline {
            deadline,
            maximum: Duration::from_secs(30 * 60),
        })
    );
}

#[test]
fn action_emphasis_remains_independent_from_default_key_designation() {
    let default_standard = save_action();
    let prominent_non_default = ModalAction::new(
        Decision::Save,
        "Save",
        ModalActionRole::Affirmative,
        "prominent-save",
    )
    .with_emphasis(ModalActionEmphasis::Prominent);

    assert_eq!(
        (
            default_standard.is_default(),
            default_standard.emphasis(),
            prominent_non_default.is_default(),
            prominent_non_default.emphasis(),
        ),
        (
            true,
            ModalActionEmphasis::Standard,
            false,
            ModalActionEmphasis::Prominent,
        )
    );
}

#[test]
fn public_modal_support_contract_is_exported_without_private_machinery() {
    fn assert_error<T: std::error::Error + Send + Sync + 'static>() {}
    fn retain_opaque_types(
        _: Option<(
            ModalPresentationHandle,
            DialogCompletion,
            spaceterm_ui::DialogPendingCompletion,
            ProgressCancellationCompletion,
            ProgressDialogHandle,
        )>,
    ) {
    }

    assert_error::<ModalValidationError>();
    assert_error::<ModalPresentationError>();
    assert_error::<ModalStaleGenerationError>();
    assert_error::<ModalUpdateError>();
    assert_error::<ModalTerminalOutcomeError>();
    assert_error::<ModalDismissalError>();
    retain_opaque_types(None);

    let color = rgba(0x223344ff);
    let theme = ModalTheme::new(
        ModalPaint::new(
            color, color, color, color, color, color, color, color, color, color, color, color,
            color, color, color,
        ),
        ModalMetrics::new(px(360.0), px(480.0), px(640.0)),
    );
    let _layer = ModalLayer::new(div());
    let semantic_types = (
        TextDirection::LeftToRight,
        ModalActionEmphasis::Standard,
        ModalActionIntent::Ordinary,
        ModalActivationSource::Programmatic,
        ModalCloseReason::Programmatic,
        ModalTextField::VisibleTitle,
        ProgressDialogOutcome::Completed,
    );
    let identity_and_events: Option<(
        ModalPresentationId,
        ModalLifecycleEvent,
        ModalStaleGenerationError,
    )> = None;

    assert!(
        !theme.surface_animation_enabled()
            && semantic_types.0 == TextDirection::LeftToRight
            && identity_and_events.is_none()
    );
}
