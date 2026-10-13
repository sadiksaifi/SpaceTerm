//! Foreground account-service consent. Discovery never invokes this surface.

use gpui::{
    App, AppContext as _, Bounds, Context, IntoElement, Render, Styled as _, Task, TitlebarOptions,
    Window, WindowBounds, WindowOptions, div, px, size,
};
use spaceterm_client::{
    failure::{ClientFailure, ClientFailureClass},
    ports::ClientFuture,
};
use spaceterm_ui::{
    Alert, AlertOutcome, ModalAction, ModalActionEmphasis, ModalActionRole, ModalId, ModalLayer,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SetupKind {
    Install,
    Enroll,
}

pub(crate) trait ForegroundSetup: Send + Sync {
    fn kind(&self) -> SetupKind;
    fn execute(&self) -> ClientFuture<'_, ()>;
}

fn explanation(kind: SetupKind) -> &'static str {
    match kind {
        SetupKind::Install if cfg!(target_os = "linux") => {
            "SpaceTerm Server runs as a service for your account. It owns your terminals and keeps running after SpaceTerm quits or crashes. It starts at boot and keeps running after logout. Setup requires administrator authorization to enable lingering for your account. No SpaceTerm code runs as root. Setup also pairs this desktop with the server. SpaceTerm has not installed or started anything."
        }
        SetupKind::Install => {
            "SpaceTerm Server runs as a LaunchAgent in your login session. It owns your terminals and keeps running after SpaceTerm quits or crashes. It starts at login and stops at logout. Setup needs no administrator password. Setup also pairs this desktop with the server. SpaceTerm has not installed or started anything."
        }
        SetupKind::Enroll => {
            "This account already has a SpaceTerm Server. Pair this desktop with that server to use its terminals. Pairing keeps the existing server identity, data and version. It does not start a stopped server."
        }
    }
}

fn setup_alert(kind: SetupKind) -> Alert<bool> {
    let (title, action) = match kind {
        SetupKind::Install => ("Set up This Computer", "Set Up Server"),
        SetupKind::Enroll => ("Pair This Computer", "Pair This Desktop"),
    };
    Alert::new(
        ModalId::new("local-server-setup"),
        title,
        title,
        explanation(kind),
        vec![
            ModalAction::new(
                true,
                action,
                ModalActionRole::Affirmative,
                "local-server-setup-confirm",
            )
            .with_emphasis(ModalActionEmphasis::Prominent),
            ModalAction::new(
                false,
                "Not Now",
                ModalActionRole::Cancel,
                "local-server-setup-cancel",
            ),
        ],
    )
}

pub(crate) struct SetupWindow;
impl Render for SetupWindow {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        ModalLayer::new(div().size_full())
    }
}

/// This prompt precedes registry recovery, so a successful enrollment enters the
/// original application runtime rather than requiring a second client owner.
pub(crate) fn confirm(kind: SetupKind, cx: &mut App) -> Task<bool> {
    show(setup_alert(kind), cx)
}

pub(crate) fn notify_failure(failure: ClientFailure, cx: &mut App) -> Task<bool> {
    let reason = match failure.class() {
        ClientFailureClass::AuthorizationDenied => "Administrator authorization was denied.",
        ClientFailureClass::StorageUnavailable => "Secure storage is unavailable.",
        ClientFailureClass::RecoveryRequired => "The original setup request still needs recovery.",
        ClientFailureClass::PinMismatch | ClientFailureClass::StaleAuthority => {
            "The registered server changed during setup."
        }
        _ => "The signed server download or local connection is unavailable.",
    };
    show(
        Alert::new(
            ModalId::new("local-server-setup-failed"),
            "Server setup did not complete",
            "Server Setup Did Not Complete",
            format!(
                "{reason} Service installation may have completed. Existing server data and pairing identities have not been replaced. Continue to use your other saved servers."
            ),
            vec![ModalAction::new(
                true,
                "Continue",
                ModalActionRole::Affirmative,
                "local-server-setup-failed-continue",
            )],
        ),
        cx,
    )
}

fn show(alert: Alert<bool>, cx: &mut App) -> Task<bool> {
    let bounds = Bounds::centered(None, size(px(640.0), px(360.0)), cx);
    let opened = cx.open_window(
        cx.global::<crate::platform::window_chrome::WindowChrome>()
            .options(
                crate::platform::window_chrome::WindowRole::Launch,
                WindowOptions {
                    app_id: crate::app::window_application_id(),
                    window_background: crate::ui::appearance_runtime::window_background(cx),
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    titlebar: Some(TitlebarOptions {
                        title: Some("SpaceTerm".into()),
                        ..TitlebarOptions::default()
                    }),
                    is_resizable: false,
                    is_minimizable: false,
                    ..WindowOptions::default()
                },
                cx,
            ),
        |window, cx| {
            window.activate_window();
            cx.new(|_| SetupWindow)
        },
    );
    let Ok(window) = opened else {
        return Task::ready(false);
    };
    let (send, receive) = tokio::sync::oneshot::channel();
    cx.defer(move |cx| {
        let _ = window.update(cx, |_, window, cx| {
            let _ = alert.present(window, cx, move |outcome, _| {
                let accepted = matches!(
                    outcome,
                    AlertOutcome::Activated {
                        action_id: true,
                        ..
                    }
                );
                let _ = send.send(accepted);
            });
        });
    });
    cx.activate(true);
    cx.spawn(async move |cx| {
        let accepted = receive.await.unwrap_or(false);
        let _ = window.update(cx, |_, window, _| window.remove_window());
        accepted
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use spaceterm_ui::a11y_testing::{A11yTree, perform};

    #[gpui::test]
    async fn desktop_first_run_explains_account_service_and_requires_foreground_setup(
        cx: &mut gpui::TestAppContext,
    ) {
        cx.update(crate::ui::init).unwrap();
        let consent = cx.update(|cx| confirm(SetupKind::Install, cx));
        cx.run_until_parked();
        let handle = cx.windows()[0];
        let mut window = gpui::VisualTestContext::from_window(handle, cx);
        let tree = A11yTree::read(&mut window);
        assert!(tree.find("Set up This Computer").is_some());
        assert!(tree.find_text(explanation(SetupKind::Install)).is_some());
        assert_eq!(tree.node("Set Up Server")["aria"]["role"], "Button");
        perform(
            &mut window,
            tree.node("Not Now"),
            gpui::accesskit::Action::Click,
        );
        assert!(!consent.await);
        assert!(cx.windows().is_empty());

        let consent = cx.update(|cx| confirm(SetupKind::Install, cx));
        cx.run_until_parked();
        let mut window = gpui::VisualTestContext::from_window(cx.windows()[0], cx);
        let tree = A11yTree::read(&mut window);
        perform(
            &mut window,
            tree.node("Set Up Server"),
            gpui::accesskit::Action::Click,
        );
        assert!(consent.await);
    }

    #[test]
    fn setup_explains_host_lifetime_and_validates_the_existing_dialog_policy() {
        let text = explanation(SetupKind::Install);
        assert!(text.contains("keeps running after SpaceTerm quits or crashes"));
        if cfg!(target_os = "linux") {
            assert!(text.contains("administrator authorization"));
            assert!(text.contains("lingering"));
            assert!(text.contains("starts at boot"));
        } else {
            assert!(text.contains("starts at login and stops at logout"));
            assert!(text.contains("no administrator password"));
        }
        for kind in [SetupKind::Install, SetupKind::Enroll] {
            assert_eq!(
                setup_alert(kind).validate(&spaceterm_ui::ModalDesktopPolicy::mac_os()),
                Ok(())
            );
        }
    }
}
