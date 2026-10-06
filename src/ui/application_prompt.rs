//! Bridges GPUI prompt responses to SpaceTerm's window-owned Modal presentation.
use gpui::{
    App, AppContext, Context, Entity, EventEmitter, FocusHandle, Focusable, InteractiveElement,
    IntoElement, PromptButton, PromptHandle, PromptLevel, PromptResponse, Render,
    RenderablePromptHandle, Window, div,
};
use spaceterm_ui::{
    Alert, AlertIntent, AlertOutcome, ModalAction, ModalActionEmphasis, ModalActionRole, ModalId,
};

pub(crate) fn install(cx: &mut App) {
    cx.set_prompt_builder(build);
}

fn build(
    level: PromptLevel,
    message: &str,
    detail: Option<&str>,
    buttons: &[PromptButton],
    handle: PromptHandle,
    window: &mut Window,
    cx: &mut App,
) -> RenderablePromptHandle {
    let cancelled = buttons
        .iter()
        .position(PromptButton::is_cancel)
        .unwrap_or(usize::MAX);
    let default = buttons
        .iter()
        .position(|button| matches!(button, PromptButton::Ok(_)));
    let actions = buttons
        .iter()
        .enumerate()
        .map(|(index, button)| {
            let role = match button {
                PromptButton::Ok(_) => ModalActionRole::Affirmative,
                PromptButton::Cancel(_) => ModalActionRole::Cancel,
                PromptButton::Other(_) => ModalActionRole::Auxiliary,
            };
            let mut action = ModalAction::new(
                index,
                button.label().clone(),
                role,
                format!("application-prompt-{index}"),
            );
            if Some(index) == default {
                action = action
                    .with_emphasis(ModalActionEmphasis::Prominent)
                    .default_action(true);
            }
            action
        })
        .collect();
    let (title, body) = match detail.filter(|detail| !detail.trim().is_empty()) {
        Some(detail) => (message, detail),
        None => (
            crate::application_identity::ApplicationIdentity::current().display_name(),
            message,
        ),
    };
    let alert = Alert::new(
        ModalId::new("application-prompt"),
        message.to_owned(),
        title.to_owned(),
        body.to_owned(),
        actions,
    )
    .intent(match level {
        PromptLevel::Info => AlertIntent::Informational,
        PromptLevel::Warning => AlertIntent::Warning,
        PromptLevel::Critical => AlertIntent::Critical,
    });
    let bridge = cx.new(|cx| PromptBridge {
        focus: cx.focus_handle(),
    });
    let rendered = handle.with_view(bridge.clone(), window, cx);
    // GPUI first retains its response subscription and original focus. The Modal then owns
    // focus, keyboard routing, button order, and the same cleanup as every other app dialog.
    window.defer(cx, move |window, cx| {
        present(bridge, alert, cancelled, window, cx)
    });
    rendered
}

fn present(
    bridge: Entity<PromptBridge>,
    alert: Alert<usize>,
    cancelled: usize,
    window: &Window,
    cx: &mut App,
) {
    let weak = bridge.downgrade();
    let result = bridge.update(cx, |_, cx| {
        alert.present(window, cx, move |outcome, cx| {
            let answer = match outcome {
                AlertOutcome::Activated { action_id, .. } => action_id,
                AlertOutcome::Dismissed { .. } => cancelled,
            };
            let _ = weak.update(cx, |_, cx| cx.emit(PromptResponse(answer)));
        })
    });
    if result.is_err() {
        bridge.update(cx, |_, cx| cx.emit(PromptResponse(cancelled)));
    }
}

struct PromptBridge {
    focus: FocusHandle,
}
impl EventEmitter<PromptResponse> for PromptBridge {}
impl Focusable for PromptBridge {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}
impl Render for PromptBridge {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        // The existing root ModalLayer renders the dialog. This view only owns GPUI's response.
        div().track_focus(&self.focus)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{ParentElement, Styled, TestAppContext};
    use spaceterm_ui::ModalLayer;

    struct PromptWindow {
        focus: FocusHandle,
    }
    impl Render for PromptWindow {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            ModalLayer::new(
                div()
                    .size_full()
                    .track_focus(&self.focus)
                    .child("Window content"),
            )
        }
    }

    #[gpui::test]
    fn application_prompt_uses_modal_buttons_and_restores_focus(cx: &mut TestAppContext) {
        cx.update(crate::ui::init).unwrap();
        cx.update(install);
        let (root, cx) = cx.add_window_view(|window, cx| {
            window.activate_window();
            let focus = cx.focus_handle();
            focus.focus(window, cx);
            PromptWindow { focus }
        });
        cx.run_until_parked();
        for (key, answer) in [("escape", 1), ("enter", 0)] {
            let mut result = cx.update(|window, cx| {
                window.prompt(
                    PromptLevel::Critical,
                    "Quit SpaceTerm?",
                    Some("Running commands will stop."),
                    &[
                        PromptButton::ok("Quit SpaceTerm"),
                        PromptButton::cancel("Cancel"),
                    ],
                    cx,
                )
            });
            cx.run_until_parked();
            assert!(
                cx.debug_bounds("modal-action-application-prompt-0")
                    .is_some()
            );
            assert!(
                cx.debug_bounds("modal-action-application-prompt-1")
                    .is_some()
            );
            assert!(cx.update(|window, cx| spaceterm_ui::window_modal_is_open(window, cx)));
            cx.simulate_keystrokes(key);
            cx.simulate_event(gpui::KeyUpEvent {
                keystroke: gpui::Keystroke::parse(key).unwrap(),
            });
            cx.run_until_parked();
            assert_eq!(result.try_recv().unwrap(), Some(answer));
            assert!(!cx.update(|window, cx| spaceterm_ui::window_modal_is_open(window, cx)));
            assert!(cx.update(|window, cx| root.read(cx).focus.is_focused(window)));
            assert!(!cx.update(|window, _| window.has_active_prompt()));
        }
        let mut acknowledgement = cx.update(|window, cx| {
            window.prompt(
                PromptLevel::Info,
                "Operation complete",
                None,
                &[PromptButton::ok("OK")],
                cx,
            )
        });
        cx.run_until_parked();
        assert!(
            cx.debug_bounds("modal-action-application-prompt-0")
                .is_some()
        );
        cx.simulate_keystrokes("enter");
        cx.simulate_event(gpui::KeyUpEvent {
            keystroke: gpui::Keystroke::parse("enter").unwrap(),
        });
        cx.run_until_parked();
        assert_eq!(acknowledgement.try_recv().unwrap(), Some(0));
        assert!(cx.update(|window, cx| root.read(cx).focus.is_focused(window)));
        assert!(!cx.update(|window, _| window.has_active_prompt()));
    }

    #[gpui::test]
    fn application_prompt_publishes_an_alert_dialog_that_answers_through_its_buttons(
        cx: &mut TestAppContext,
    ) {
        use gpui::accesskit::Action;
        use spaceterm_ui::a11y_testing::{A11yTree, perform};

        cx.update(crate::ui::init).unwrap();
        cx.update(install);
        let (root, cx) = cx.add_window_view(|window, cx| {
            window.activate_window();
            let focus = cx.focus_handle();
            focus.focus(window, cx);
            PromptWindow { focus }
        });
        cx.run_until_parked();
        let mut result = cx.update(|window, cx| {
            window.prompt(
                PromptLevel::Critical,
                "Quit SpaceTerm?",
                Some("Running commands will stop."),
                &[
                    PromptButton::ok("Quit SpaceTerm"),
                    PromptButton::cancel("Cancel"),
                ],
                cx,
            )
        });
        cx.run_until_parked();

        let tree = A11yTree::read(cx);
        let alert = tree.node("Quit SpaceTerm?");
        assert_eq!(alert["aria"]["role"], "AlertDialog");
        assert_eq!(alert["aria"]["modal"], true);
        assert_eq!(alert["aria"]["description"], "Running commands will stop.");
        assert_eq!(tree.node("Quit SpaceTerm")["aria"]["role"], "Button");

        perform(cx, tree.node("Quit SpaceTerm"), Action::Click);
        cx.run_until_parked();
        assert_eq!(result.try_recv().unwrap(), Some(0));
        assert!(A11yTree::read(cx).find("Quit SpaceTerm?").is_none());
        assert!(cx.update(|window, cx| root.read(cx).focus.is_focused(window)));
    }
}
