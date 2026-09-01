use gpui::prelude::*;
use gpui::{App, Context, Entity, Render, Window, div, px};
use spaceterm_ui::{
    Alert, AlertIntent, AlertOutcome, Dialog, DialogCloseDecision, DialogCompletion,
    DialogInitialFocus, DialogOutcome, DialogSize, ModalAction, ModalActionRole, ModalId,
    ModalLifecycleEvent, ModalPresentationHandle, TextInput, TextInputContentMode,
    TextInputEscapeBehavior, TextInputReturnBehavior, TextInputVariant,
};

use crate::platform::ssh_askpass::{
    AskPassCompletion, AskPassConfirmationPresentation, AskPassPresentationError, AskPassRequest,
    AskPassResponseError, AskPassResult, AskPassSecret, AskPassSecretPresentation,
};
use crate::theme::{ACTIVE_THEME, Color};

const CONFIRMATION_MODAL_ID: &str = "ssh-askpass-confirmation";
const SECRET_MODAL_ID: &str = "ssh-askpass-secret";
const MAX_SECRET_BYTES: usize = 16 * 1024;
const SECRET_INPUT_HEIGHT: f32 = 28.0;
const REQUIRED_SECRET_MESSAGE: &str = "Enter a response to continue.";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum AskPassAction {
    Submit,
    Cancel,
}

enum AskPassModalHandle {
    Confirmation(ModalPresentationHandle),
    Secret(DialogCompletion),
}

impl AskPassModalHandle {
    fn dismiss(&self, window: &Window, cx: &mut App) {
        match self {
            Self::Confirmation(handle) => {
                let _ = handle.dismiss(window, cx);
            }
            Self::Secret(handle) => {
                let _ = handle.dismiss(window, cx);
            }
        }
    }
}

struct ActiveAskPassPresentation {
    owner: u64,
    generation: u64,
    handle: AskPassModalHandle,
    body: Option<Entity<AskPassSecretBody>>,
    result: Option<AskPassResult>,
    completion: Option<AskPassCompletion>,
}

impl Drop for ActiveAskPassPresentation {
    fn drop(&mut self) {
        if let Some(completion) = self.completion.take() {
            completion(AskPassResult::Cancelled);
        }
    }
}

struct AskPassSettlement {
    body: Option<Entity<AskPassSecretBody>>,
    result: AskPassResult,
    completion: AskPassCompletion,
}

impl AskPassSettlement {
    fn deliver(self, cx: &mut App) {
        if let Some(body) = self.body {
            body.update(cx, |body, cx| body.clear(cx));
        }
        (self.completion)(self.result);
    }
}

/// Application-owned presenter for exactly one active or queued OpenSSH AskPass request.
#[derive(Default)]
pub(crate) struct GpuiAskPassPresenter {
    next_generation: u64,
    active: Option<ActiveAskPassPresentation>,
}

impl GpuiAskPassPresenter {
    pub(crate) fn present(
        &mut self,
        owner: u64,
        request: AskPassRequest,
        completion: AskPassCompletion,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<(), AskPassPresentationError> {
        if self.active.is_some() {
            return Err(AskPassPresentationError::Busy);
        }
        self.next_generation = self.next_generation.wrapping_add(1);
        let generation = self.next_generation;
        if let Some(presentation) = request.secret_presentation() {
            self.present_secret(owner, generation, presentation, completion, window, cx)
        } else if let Some(presentation) = request.confirmation_presentation() {
            self.present_confirmation(owner, generation, presentation, completion, window, cx)
        } else {
            Err(AskPassPresentationError::ApplicationPresentationUnavailable)
        }
    }

    pub(crate) fn cancel_owner(&self, owner: u64, window: &Window, cx: &mut App) {
        if let Some(active) = &self.active
            && active.owner == owner
        {
            active.handle.dismiss(window, cx);
        }
    }

    fn present_confirmation(
        &mut self,
        owner: u64,
        generation: u64,
        presentation: AskPassConfirmationPresentation,
        completion: AskPassCompletion,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<(), AskPassPresentationError> {
        let first_contact = presentation.is_first_contact();
        let presenter = cx.weak_entity();
        let result_presenter = presenter.clone();
        let lifecycle_presenter = presenter;
        let alert = Alert::new(
            ModalId::new(CONFIRMATION_MODAL_ID),
            presentation.title(),
            presentation.title(),
            presentation.message(),
            vec![
                ModalAction::new(
                    AskPassAction::Submit,
                    presentation.affirmative(),
                    ModalActionRole::Affirmative,
                    "ssh-askpass-confirm",
                )
                .default_action(true),
                ModalAction::new(
                    AskPassAction::Cancel,
                    presentation.negative(),
                    ModalActionRole::Cancel,
                    "ssh-askpass-reject",
                ),
            ],
        )
        .intent(AlertIntent::Warning)
        .detail(presentation.detail().to_owned())
        .present_with_lifecycle(
            window,
            cx,
            move |outcome, cx| {
                let result = map_confirmation_outcome(first_contact, outcome);
                let _ = result_presenter.update(cx, |presenter, _| {
                    presenter.stage_result(generation, result);
                });
            },
            move |event, cx| {
                settle_after_close(&lifecycle_presenter, generation, event, cx);
            },
        )
        .map_err(|_| AskPassPresentationError::ApplicationPresentationUnavailable)?;
        self.active = Some(ActiveAskPassPresentation {
            owner,
            generation,
            handle: AskPassModalHandle::Confirmation(alert),
            body: None,
            result: None,
            completion: Some(completion),
        });
        Ok(())
    }

    fn present_secret(
        &mut self,
        owner: u64,
        generation: u64,
        presentation: AskPassSecretPresentation,
        completion: AskPassCompletion,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<(), AskPassPresentationError> {
        let body = cx.new(|cx| AskPassSecretBody::new(presentation, window, cx));
        let initial_focus = body.read(cx).focus_handle(cx);
        let presenter = cx.weak_entity();
        let action_presenter = presenter.clone();
        let action_body = body.clone();
        let result_presenter = presenter.clone();
        let lifecycle_presenter = presenter;
        let dialog = Dialog::new(
            ModalId::new(SECRET_MODAL_ID),
            "SSH authentication response",
            body.read(cx).title(),
            vec![
                ModalAction::new(
                    AskPassAction::Submit,
                    body.read(cx).affirmative(),
                    ModalActionRole::Affirmative,
                    "ssh-askpass-submit",
                )
                .default_action(true),
                ModalAction::new(
                    AskPassAction::Cancel,
                    body.read(cx).negative(),
                    ModalActionRole::Cancel,
                    "ssh-askpass-cancel",
                ),
            ],
            DialogInitialFocus::Body(initial_focus),
        )
        .size(DialogSize::Regular)
        .body(body.clone())
        .present_with_lifecycle(
            window,
            cx,
            move |request, _, cx| match request.action_id() {
                AskPassAction::Cancel => DialogCloseDecision::Allow,
                AskPassAction::Submit => {
                    let submission = action_body.update(cx, |body, cx| body.take_submission(cx));
                    match submission {
                        Ok(secret) => {
                            let _ = action_presenter.update(cx, |presenter, _| {
                                presenter.stage_result(generation, AskPassResult::Secret(secret));
                            });
                            DialogCloseDecision::Allow
                        }
                        Err(SecretSubmissionError::Required) => DialogCloseDecision::Deny {
                            first_invalid: Some(action_body.read(cx).focus_handle(cx)),
                        },
                        Err(SecretSubmissionError::Response(error)) => {
                            let _ = action_presenter.update(cx, |presenter, _| {
                                presenter.stage_result(generation, AskPassResult::Failed(error));
                            });
                            DialogCloseDecision::Allow
                        }
                    }
                }
            },
            move |outcome, cx| {
                if !matches!(
                    outcome,
                    DialogOutcome::Completed {
                        action_id: AskPassAction::Submit,
                        ..
                    }
                ) {
                    let _ = result_presenter.update(cx, |presenter, _| {
                        presenter.stage_result(generation, AskPassResult::Cancelled);
                    });
                }
            },
            move |event, cx| {
                settle_after_close(&lifecycle_presenter, generation, event, cx);
            },
        )
        .map_err(|_| AskPassPresentationError::ApplicationPresentationUnavailable)?;
        self.active = Some(ActiveAskPassPresentation {
            owner,
            generation,
            handle: AskPassModalHandle::Secret(dialog),
            body: Some(body),
            result: None,
            completion: Some(completion),
        });
        Ok(())
    }

    fn stage_result(&mut self, generation: u64, result: AskPassResult) {
        let Some(active) = self.active.as_mut() else {
            return;
        };
        if active.generation == generation && active.result.is_none() {
            active.result = Some(result);
        }
    }

    fn settle(&mut self, generation: u64) -> Option<AskPassSettlement> {
        if self
            .active
            .as_ref()
            .is_none_or(|active| active.generation != generation)
        {
            return None;
        }
        let mut active = self.active.take()?;
        Some(AskPassSettlement {
            body: active.body.take(),
            result: active.result.take().unwrap_or(AskPassResult::Cancelled),
            completion: active.completion.take()?,
        })
    }
}

fn settle_after_close(
    presenter: &gpui::WeakEntity<GpuiAskPassPresenter>,
    generation: u64,
    event: &ModalLifecycleEvent,
    cx: &mut App,
) {
    if !matches!(event, ModalLifecycleEvent::Closed(_, _)) {
        return;
    }
    let settlement = presenter
        .update(cx, |presenter, _| presenter.settle(generation))
        .ok()
        .flatten();
    if let Some(settlement) = settlement {
        settlement.deliver(cx);
    }
}

fn map_confirmation_outcome(
    first_contact: bool,
    outcome: AlertOutcome<AskPassAction>,
) -> AskPassResult {
    match outcome {
        AlertOutcome::Activated {
            action_id: AskPassAction::Submit,
            ..
        } => AskPassResult::Confirmation(true),
        AlertOutcome::Activated {
            action_id: AskPassAction::Cancel,
            ..
        } if !first_contact => AskPassResult::Confirmation(false),
        AlertOutcome::Activated { .. } | AlertOutcome::Dismissed { .. } => AskPassResult::Cancelled,
    }
}

enum SecretSubmissionError {
    Required,
    Response(AskPassResponseError),
}

struct AskPassSecretBody {
    title: &'static str,
    detail: String,
    affirmative: &'static str,
    negative: &'static str,
    field_label: &'static str,
    requires_nonempty: bool,
    required_error: bool,
    input: Entity<TextInput>,
}

impl AskPassSecretBody {
    fn new(
        presentation: AskPassSecretPresentation,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let input = cx.new(|cx| {
            TextInput::new(
                "ssh-askpass-secret-input",
                presentation.field_label(),
                "",
                window,
                cx,
            )
            .variant(TextInputVariant::Bare)
            .content_mode(TextInputContentMode::Obscured)
            .input_length_limit(Some(MAX_SECRET_BYTES))
            .return_behavior(TextInputReturnBehavior::Propagate)
            .escape_behavior(TextInputEscapeBehavior::Propagate)
            .debug_selector("ssh-askpass-secret-input")
        });
        Self {
            title: presentation.title(),
            detail: presentation.detail().to_owned(),
            affirmative: presentation.affirmative(),
            negative: presentation.negative(),
            field_label: presentation.field_label(),
            requires_nonempty: presentation.requires_nonempty(),
            required_error: false,
            input,
        }
    }

    const fn title(&self) -> &'static str {
        self.title
    }

    const fn affirmative(&self) -> &'static str {
        self.affirmative
    }

    const fn negative(&self) -> &'static str {
        self.negative
    }

    fn focus_handle(&self, cx: &App) -> gpui::FocusHandle {
        self.input.read(cx).focus_handle()
    }

    fn take_submission(
        &mut self,
        cx: &mut Context<Self>,
    ) -> Result<AskPassSecret, SecretSubmissionError> {
        if self.requires_nonempty && self.input.read(cx).value().is_empty() {
            self.required_error = true;
            cx.notify();
            return Err(SecretSubmissionError::Required);
        }
        self.required_error = false;
        let value = self.input.update(cx, |input, cx| input.take_value(cx));
        AskPassSecret::new(value.into_bytes()).map_err(SecretSubmissionError::Response)
    }

    fn clear(&mut self, cx: &mut Context<Self>) {
        self.input.update(cx, |input, cx| {
            input.clear(cx);
        });
        self.required_error = false;
    }
}

impl Render for AskPassSecretBody {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let input_focus = self.input.read(cx).focus_handle();
        div()
            .flex()
            .flex_col()
            .gap(px(12.0))
            .child(
                div()
                    .text_size(px(12.0))
                    .text_color(gpui_color(ACTIVE_THEME.text_muted))
                    .whitespace_normal()
                    .child(self.detail.clone()),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(5.0))
                    .child(
                        div()
                            .text_size(px(12.0))
                            .text_color(gpui_color(ACTIVE_THEME.text_muted))
                            .child(self.field_label),
                    )
                    .child(
                        div()
                            .id("ssh-askpass-secret-input-frame")
                            .debug_selector(|| "ssh-askpass-secret-input-frame".to_owned())
                            .h(px(SECRET_INPUT_HEIGHT))
                            .w_full()
                            .min_w_0()
                            .flex_shrink_0()
                            .flex()
                            .items_center()
                            .overflow_hidden()
                            .px(px(8.0))
                            .rounded(px(4.0))
                            .border(px(1.0))
                            .border_color(gpui_color(ACTIVE_THEME.border))
                            .bg(gpui_color(ACTIVE_THEME.element_background))
                            .text_size(px(13.0))
                            .text_color(gpui_color(ACTIVE_THEME.text))
                            .on_click(move |_, window, cx| {
                                input_focus.focus(window);
                                cx.stop_propagation();
                            })
                            .child(self.input.clone()),
                    )
                    .when(self.required_error, |field| {
                        field.child(
                            div()
                                .debug_selector(|| "ssh-askpass-required-error".to_owned())
                                .text_size(px(11.0))
                                .text_color(gpui_color(ACTIVE_THEME.error))
                                .child(REQUIRED_SECRET_MESSAGE),
                        )
                    }),
            )
    }
}

fn gpui_color(color: Color) -> gpui::Rgba {
    gpui::rgba(color.rgba_hex())
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::rc::Rc;

    use gpui::{TestAppContext, VisualTestContext};
    use spaceterm_ui::{ModalLayer, ModalPresentationHandle};

    use super::*;
    use crate::platform::ssh_askpass::AskPassPromptKind;

    #[derive(Debug, Eq, PartialEq)]
    enum ObservedResult {
        Secret(Vec<u8>),
        Confirmation(bool),
        Cancelled,
        Failed,
    }

    impl From<AskPassResult> for ObservedResult {
        fn from(value: AskPassResult) -> Self {
            match value {
                AskPassResult::Secret(secret) => Self::Secret(secret.as_bytes().to_vec()),
                AskPassResult::Confirmation(confirmed) => Self::Confirmation(confirmed),
                AskPassResult::Cancelled => Self::Cancelled,
                AskPassResult::Failed(_) => Self::Failed,
            }
        }
    }

    struct AskPassHarness {
        presenter: Entity<GpuiAskPassPresenter>,
        blocker: Option<ModalPresentationHandle>,
    }

    impl AskPassHarness {
        fn block_modal_queue(&mut self, window: &Window, cx: &mut Context<Self>) {
            self.blocker = Some(
                Alert::new(
                    ModalId::new("ssh-askpass-test-blocker"),
                    "Blocking modal",
                    "Blocking Modal",
                    "Keep the next presentation queued.",
                    vec![ModalAction::new(
                        AskPassAction::Cancel,
                        "Close",
                        ModalActionRole::Cancel,
                        "ssh-askpass-test-blocker-close",
                    )],
                )
                .present(window, cx, |_, _| {})
                .expect("test blocker should present"),
            );
        }
    }

    impl Render for AskPassHarness {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            ModalLayer::new(div().size_full())
        }
    }

    type AskPassWindow<'a> = (
        Entity<AskPassHarness>,
        Entity<GpuiAskPassPresenter>,
        Rc<RefCell<Vec<ObservedResult>>>,
        &'a mut VisualTestContext,
    );

    fn askpass_window(cx: &mut TestAppContext) -> AskPassWindow<'_> {
        cx.update(crate::ui::init)
            .expect("UI initialization should succeed");
        let results = Rc::new(RefCell::new(Vec::new()));
        let (harness, cx) = cx.add_window_view(|_, cx| {
            let presenter = cx.new(|_| GpuiAskPassPresenter::default());
            AskPassHarness {
                presenter,
                blocker: None,
            }
        });
        let presenter = harness.read_with(cx, |harness, _| harness.presenter.clone());
        cx.update(|window, _| window.activate_window());
        (harness, presenter, results, cx)
    }

    fn request(prompt: &str, kind: AskPassPromptKind) -> AskPassRequest {
        AskPassRequest::new(prompt.to_owned(), kind).expect("test prompt should be valid")
    }

    fn present(
        owner: u64,
        request: AskPassRequest,
        presenter: &Entity<GpuiAskPassPresenter>,
        results: &Rc<RefCell<Vec<ObservedResult>>>,
        cx: &mut VisualTestContext,
    ) {
        let captured = Rc::clone(results);
        cx.update(|window, cx| {
            presenter
                .update(cx, |presenter, cx| {
                    presenter.present(
                        owner,
                        request,
                        Box::new(move |result| captured.borrow_mut().push(result.into())),
                        window,
                        cx,
                    )
                })
                .expect("AskPass presentation should open");
        });
        cx.run_until_parked();
    }

    fn input_focus(
        presenter: &Entity<GpuiAskPassPresenter>,
        cx: &VisualTestContext,
    ) -> gpui::FocusHandle {
        presenter.read_with(cx, |presenter, cx| {
            presenter
                .active
                .as_ref()
                .and_then(|active| active.body.as_ref())
                .expect("secret body should exist")
                .read(cx)
                .focus_handle(cx)
        })
    }

    #[gpui::test]
    fn password_focuses_obscured_input_and_return_denies_empty_then_submits_once(
        cx: &mut TestAppContext,
    ) {
        let (_, presenter, results, cx) = askpass_window(cx);
        present(
            1,
            request("root@example.test's password:", AskPassPromptKind::Secret),
            &presenter,
            &results,
            cx,
        );

        let focus = input_focus(&presenter, cx);
        assert!(cx.update(|window, _| focus.is_focused(window)));
        presenter.update(cx, |presenter, _| {
            presenter.stage_result(
                0,
                AskPassResult::Failed(AskPassResponseError::SecretTooLong),
            );
        });
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();
        assert!(cx.debug_bounds("ssh-askpass-required-error").is_some());
        assert!(results.borrow().is_empty());

        cx.simulate_input("correct horse");
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();
        assert_eq!(
            results.borrow().as_slice(),
            [ObservedResult::Secret(b"correct horse".to_vec())]
        );
        assert!(presenter.read_with(cx, |presenter, _| presenter.active.is_none()));
    }

    #[gpui::test]
    fn secret_input_uses_compact_single_line_frame(cx: &mut TestAppContext) {
        let (_, presenter, results, cx) = askpass_window(cx);
        present(
            1,
            request("root@example.test's password:", AskPassPromptKind::Secret),
            &presenter,
            &results,
            cx,
        );

        let bounds = cx
            .debug_bounds("ssh-askpass-secret-input-frame")
            .expect("secret input frame should render");
        assert_eq!(bounds.size, gpui::size(px(446.0), px(SECRET_INPUT_HEIGHT)));
    }

    #[gpui::test]
    fn generic_secret_accepts_empty_return(cx: &mut TestAppContext) {
        let (_, presenter, results, cx) = askpass_window(cx);
        present(
            2,
            request("Verification response:", AskPassPromptKind::Secret),
            &presenter,
            &results,
            cx,
        );

        cx.simulate_keystrokes("enter");
        cx.run_until_parked();
        assert_eq!(
            results.borrow().as_slice(),
            [ObservedResult::Secret(Vec::new())]
        );
    }

    #[gpui::test]
    fn escape_and_owner_cancellation_settle_once_and_ignore_stale_owner(cx: &mut TestAppContext) {
        let (_, presenter, results, cx) = askpass_window(cx);
        present(
            10,
            request("first password:", AskPassPromptKind::Secret),
            &presenter,
            &results,
            cx,
        );
        cx.simulate_keystrokes("escape");
        cx.run_until_parked();
        assert_eq!(results.borrow().as_slice(), [ObservedResult::Cancelled]);

        present(
            11,
            request("second password:", AskPassPromptKind::Secret),
            &presenter,
            &results,
            cx,
        );
        cx.update(|window, cx| {
            presenter.update(cx, |presenter, cx| presenter.cancel_owner(10, window, cx));
        });
        cx.run_until_parked();
        assert_eq!(results.borrow().len(), 1);
        assert_eq!(
            presenter.read_with(cx, |presenter, _| {
                presenter.active.as_ref().map(|active| active.owner)
            }),
            Some(11)
        );

        cx.update(|window, cx| {
            presenter.update(cx, |presenter, cx| {
                presenter.cancel_owner(11, window, cx);
                presenter.cancel_owner(11, window, cx);
            });
        });
        cx.run_until_parked();
        assert_eq!(
            results.borrow().as_slice(),
            [ObservedResult::Cancelled, ObservedResult::Cancelled]
        );
    }

    #[gpui::test]
    fn queued_secret_can_be_cancelled_before_it_becomes_active(cx: &mut TestAppContext) {
        let (harness, presenter, results, cx) = askpass_window(cx);
        cx.update(|window, cx| {
            harness.update(cx, |harness, cx| harness.block_modal_queue(window, cx));
        });
        present(
            20,
            request("queued password:", AskPassPromptKind::Secret),
            &presenter,
            &results,
            cx,
        );
        assert!(cx.debug_bounds("ssh-askpass-secret-input").is_none());

        cx.update(|window, cx| {
            presenter.update(cx, |presenter, cx| presenter.cancel_owner(20, window, cx));
        });
        cx.run_until_parked();
        assert_eq!(results.borrow().as_slice(), [ObservedResult::Cancelled]);
        assert!(presenter.read_with(cx, |presenter, _| presenter.active.is_none()));
    }

    #[test]
    fn confirmation_rejection_maps_first_contact_to_cancel_and_generic_to_false() {
        let rejected = AlertOutcome::Activated {
            action_id: AskPassAction::Cancel,
            source: spaceterm_ui::ModalActivationSource::Pointer,
            suppression_selected: None,
        };
        assert!(matches!(
            map_confirmation_outcome(true, rejected),
            AskPassResult::Cancelled
        ));
        let rejected = AlertOutcome::Activated {
            action_id: AskPassAction::Cancel,
            source: spaceterm_ui::ModalActivationSource::Pointer,
            suppression_selected: None,
        };
        assert!(matches!(
            map_confirmation_outcome(false, rejected),
            AskPassResult::Confirmation(false)
        ));
    }
}
