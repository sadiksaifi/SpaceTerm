use std::time::Duration;

use gpui::{
    AppContext as _, Context, FocusHandle, InteractiveElement as _, ParentElement as _, Render,
    Styled as _, TestAppContext, Window, div, px,
};
use spaceterm_ui::{
    Alert, AlertAccessory, AlertIntent, AlertOutcome, DeterminateProgress, Dialog,
    DialogCloseDecision, DialogCompletion, DialogFocusTarget, DialogInitialFocus, DialogOutcome,
    DialogPendingCompletion, DialogSize, ModalAction, ModalActionIntent, ModalActionRole,
    ModalDesktopPolicy, ModalDismissalError, ModalId, ModalLifecycleEvent, ModalPresentationError,
    ModalPresentationHandle, ModalStaleGenerationError, ModalTerminalOutcomeError, ModalTextField,
    ModalUpdateError, ModalValidationError, ProgressCancellation, ProgressCancellationCompletion,
    ProgressDialog, ProgressDialogHandle, ProgressDialogUpdate, ProgressState, ProgressValueError,
    install_modal_policy,
};

const _: fn() = || {
    fn require_recoverable_error<T: std::error::Error + Send + Sync + 'static>() {}
    fn retain_opaque_support_types(
        _: Option<(
            ModalPresentationHandle,
            DialogCompletion,
            DialogPendingCompletion,
            ProgressCancellationCompletion,
            ProgressDialogHandle,
            ModalTextField,
        )>,
    ) {
    }

    require_recoverable_error::<ModalValidationError>();
    require_recoverable_error::<ModalPresentationError>();
    require_recoverable_error::<ModalStaleGenerationError>();
    require_recoverable_error::<ModalUpdateError>();
    require_recoverable_error::<ModalTerminalOutcomeError>();
    require_recoverable_error::<ModalDismissalError>();
    require_recoverable_error::<ProgressValueError>();
    retain_opaque_support_types(None);
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

struct PublicCustomDialogHost {
    dialog: Option<DialogCompletion>,
}

impl Render for PublicCustomDialogHost {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl gpui::IntoElement {
        spaceterm_ui::ModalLayer::new(div().size_full())
    }
}

fn install_public_dialog_themes(cx: &mut TestAppContext) {
    use spaceterm_ui::{
        ButtonMetrics, ButtonPaint, ButtonSizes, ButtonTheme, ButtonVariantStyle, ButtonVariants,
        ModalMetrics, ModalPaint, ModalTheme, ScrollbarTheme,
    };
    let color = gpui::rgba(0x606060ff);
    cx.set_global(ModalTheme::new(
        ModalPaint::new(color, color, color, color, color),
        ModalMetrics::new(px(360.0), px(480.0), px(640.0)),
    ));
    let paint = ButtonPaint::new(color, gpui::rgba(0xffffffff), color);
    let variant = ButtonVariantStyle::new(paint, paint, paint, paint);
    let metrics = ButtonMetrics::new(px(24.0));
    cx.set_global(ButtonTheme::new(
        ButtonVariants::new(
            variant, variant, variant, variant, variant, variant, variant,
        ),
        ButtonSizes::new(metrics, metrics, metrics, metrics),
        color,
    ));
    cx.set_global(ScrollbarTheme::new(color, color, color));
}

struct PublicCustomDialogBody {
    first: FocusHandle,
    second: FocusHandle,
}

impl Render for PublicCustomDialogBody {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl gpui::IntoElement {
        div()
            .flex()
            .flex_col()
            .child(DialogFocusTarget::new(
                div()
                    .debug_selector(|| "public-custom-first".to_owned())
                    .h(px(28.0))
                    .flex_shrink_0()
                    .track_focus(&self.first)
                    .child("First custom control"),
                self.first.clone(),
            ))
            .child(div().h(px(600.0)).flex_shrink_0())
            .child(DialogFocusTarget::new(
                div()
                    .debug_selector(|| "public-custom-second".to_owned())
                    .h(px(28.0))
                    .flex_shrink_0()
                    .track_focus(&self.second)
                    .child("Second custom control"),
                self.second.clone(),
            ))
    }
}

#[gpui::test]
fn public_dialog_focus_target_wraps_custom_controls_without_scroll_types(cx: &mut TestAppContext) {
    install_public_dialog_themes(cx);
    cx.update(spaceterm_ui::install_portable_modal_keybindings);
    cx.update(|cx| install_modal_policy(cx, ModalDesktopPolicy::mac_os()));
    let (caller, cx) = cx.add_window_view(|_, _| PublicCustomDialogHost { dialog: None });
    let body = cx.update(|_, cx| {
        cx.new(|cx| PublicCustomDialogBody {
            first: cx.focus_handle().tab_stop(true),
            second: cx.focus_handle().tab_stop(true),
        })
    });
    let (first, second) = body.read_with(cx, |body, _| (body.first.clone(), body.second.clone()));
    cx.update(|window, cx| {
        window.activate_window();
        caller.update(cx, |caller, cx| {
            caller.dialog = Some(
                Dialog::new(
                    ModalId::new("public-custom-dialog"),
                    "Custom controls",
                    "Custom controls",
                    vec![save_action(), cancel_action()],
                    DialogInitialFocus::Body(second.clone()),
                )
                .body(body.clone())
                .present(window, cx, |_, _, _| DialogCloseDecision::Allow, |_, _| {})
                .expect("custom dialog"),
            );
        });
    });
    cx.run_until_parked();
    let assert_visible =
        |cx: &mut gpui::VisualTestContext, selector: &'static str, focus: &FocusHandle| {
            let viewport = cx
                .debug_bounds("modal-body-viewport")
                .expect("presented dialog viewport");
            let target = cx.debug_bounds(selector).expect("custom control");
            assert!(
                cx.update(|window, _| focus.is_focused(window)),
                "{selector} should receive focus"
            );
            assert!(
                target.top() >= viewport.top() && target.bottom() <= viewport.bottom(),
                "{target:?} outside {viewport:?}"
            );
        };
    assert_visible(cx, "public-custom-second", &second);
    cx.simulate_keystrokes("shift-tab");
    cx.run_until_parked();
    assert_visible(cx, "public-custom-first", &first);
    cx.simulate_keystrokes("tab");
    cx.run_until_parked();
    assert_visible(cx, "public-custom-second", &second);
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
