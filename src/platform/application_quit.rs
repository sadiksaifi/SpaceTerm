use std::rc::Rc;

use gpui::{App, AsyncApp, PromptButton, PromptLevel, Task, Window};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ApplicationQuitDecision {
    Proceed,
    Cancel,
}

/// Why the host asked the application to quit.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ApplicationQuitRequest {
    /// The person asked to quit, such as from the Dock. Close windows when the status item
    /// remains available; otherwise confirm exiting the client.
    #[cfg_attr(
        not(target_os = "macos"),
        allow(
            dead_code,
            reason = "Linux commands invoke dismissal directly; only macOS reports Dock requests"
        )
    )]
    Dismissal,
    /// The client chose to exit, such as from its status item or an installed update.
    Exit,
    /// The operating system is logging out, restarting, or shutting down. Never prompt.
    #[cfg_attr(
        not(target_os = "macos"),
        allow(
            dead_code,
            reason = "Linux session shutdown terminates the process outside application Quit"
        )
    )]
    SessionEnd,
}

/// The host's name for the place a windowless client stays reachable.
#[cfg_attr(
    target_os = "macos",
    allow(dead_code, reason = "only Linux desktops present a status area")
)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum StatusAreaName {
    MenuBar,
    StatusArea,
}

impl StatusAreaName {
    /// The name inside a sentence, such as "menu bar".
    pub(crate) const fn phrase(self) -> &'static str {
        match self {
            Self::MenuBar => "menu bar",
            Self::StatusArea => "status area",
        }
    }
}

/// The warning for an actual client exit. Installed builds make Quit the default answer;
/// destructive Development exits focus Cancel.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct QuitPrompt {
    pub(crate) title: String,
    pub(crate) detail: String,
    pub(crate) destructive: bool,
}

/// The person's answer to the quit warning.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum QuitAnswer {
    Quit,
    Cancel,
}

impl QuitAnswer {
    pub(crate) const CANCEL_LABEL: &str = "Cancel";
}

/// Presents the quit warning through GPUI's prompt for this window.
pub(crate) fn present_window_quit(
    prompt: &QuitPrompt,
    window: &mut Window,
    cx: &mut App,
) -> Task<Option<QuitAnswer>> {
    let answer = window.prompt(
        if prompt.destructive {
            PromptLevel::Critical
        } else {
            PromptLevel::Info
        },
        &prompt.title,
        Some(&prompt.detail),
        &[
            PromptButton::ok("Quit"),
            PromptButton::cancel(QuitAnswer::CANCEL_LABEL),
        ],
        cx,
    );
    cx.spawn(async move |_| match answer.await.ok()? {
        0 => Some(QuitAnswer::Quit),
        1 => Some(QuitAnswer::Cancel),
        _ => None,
    })
}

#[cfg_attr(
    not(target_os = "macos"),
    allow(
        dead_code,
        reason = "desktops without an application delegate report only installation failures"
    )
)]
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub(crate) enum ApplicationQuitError {
    #[error("the application quit handler is already installed")]
    AlreadyInstalled,
    #[error("the application quit handler must run on the main thread")]
    OffMainThread,
    #[error("the native application delegate is unavailable")]
    DelegateUnavailable,
    #[error("the native application delegate already owns termination policy")]
    DelegateConflict,
}

#[cfg_attr(
    not(target_os = "macos"),
    allow(
        dead_code,
        reason = "only an application delegate re-enters the quit handler from native callbacks"
    )
)]
#[derive(Clone)]
pub(crate) struct ApplicationQuitHandler {
    app: AsyncApp,
    request: Rc<ApplicationQuitRequestHandler>,
}

type ApplicationQuitRequestHandler =
    dyn Fn(&mut App, ApplicationQuitRequest) -> ApplicationQuitDecision;

impl ApplicationQuitHandler {
    pub(crate) fn new(cx: &App, request: Rc<ApplicationQuitRequestHandler>) -> Self {
        Self {
            app: cx.to_async(),
            request,
        }
    }

    pub(crate) fn handle(
        &self,
        request: ApplicationQuitRequest,
        cx: &mut App,
    ) -> ApplicationQuitDecision {
        (self.request)(cx, request)
    }

    #[cfg_attr(
        not(target_os = "macos"),
        allow(
            dead_code,
            reason = "only an application delegate re-enters the quit handler from native callbacks"
        )
    )]
    pub(crate) fn handle_native(&self, request: ApplicationQuitRequest) -> ApplicationQuitDecision {
        self.app.update(|cx| self.handle(request, cx))
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum LastWindowPolicy {
    KeepRunning,
    Quit,
}

/// Connects native application termination requests to portable quit policy.
pub(crate) trait ApplicationQuitAdapter {
    fn last_window_policy(&self) -> LastWindowPolicy {
        LastWindowPolicy::KeepRunning
    }

    fn status_area_name(&self) -> StatusAreaName {
        StatusAreaName::MenuBar
    }

    fn install(&self, handler: ApplicationQuitHandler) -> Result<(), ApplicationQuitError>;

    /// Asks the quit warning in `window`. Installed builds default to Quit. A destructive
    /// Development exit focuses Cancel, and Escape always cancels.
    fn present_confirmation(
        &self,
        prompt: &QuitPrompt,
        window: &mut Window,
        cx: &mut App,
    ) -> Task<Option<QuitAnswer>> {
        present_window_quit(prompt, window, cx)
    }

    /// Asks the application to exit through the quit policy.
    fn request_quit(&self, cx: &mut App);

    fn confirm_quit(&self, cx: &mut App);
}

#[cfg(test)]
pub(crate) mod testing {
    use std::cell::{Cell, RefCell};

    use super::*;

    #[derive(Default)]
    pub(crate) struct RecordingApplicationQuitAdapter {
        last_window: Option<LastWindowPolicy>,
        handler: RefCell<Option<ApplicationQuitHandler>>,
        requests: Rc<Cell<usize>>,
        confirmations: Rc<Cell<usize>>,
    }

    impl RecordingApplicationQuitAdapter {
        pub(crate) fn with_last_window_policy(policy: LastWindowPolicy) -> Self {
            Self {
                last_window: Some(policy),
                ..Self::default()
            }
        }

        pub(crate) fn requests(&self) -> usize {
            self.requests.get()
        }

        pub(crate) fn confirmations(&self) -> usize {
            self.confirmations.get()
        }

        pub(crate) fn simulate_native_request(
            &self,
            request: ApplicationQuitRequest,
        ) -> ApplicationQuitDecision {
            self.requests.set(self.requests.get() + 1);
            self.handler
                .borrow()
                .as_ref()
                .map_or(ApplicationQuitDecision::Proceed, |handler| {
                    handler.handle_native(request)
                })
        }
    }

    impl ApplicationQuitAdapter for RecordingApplicationQuitAdapter {
        fn last_window_policy(&self) -> LastWindowPolicy {
            self.last_window.unwrap_or(LastWindowPolicy::KeepRunning)
        }

        fn install(&self, handler: ApplicationQuitHandler) -> Result<(), ApplicationQuitError> {
            let mut installed = self.handler.borrow_mut();
            if installed.is_some() {
                return Err(ApplicationQuitError::AlreadyInstalled);
            }
            *installed = Some(handler);
            Ok(())
        }

        fn request_quit(&self, cx: &mut App) {
            self.requests.set(self.requests.get() + 1);
            let handler = self.handler.borrow().clone();
            if let Some(handler) = handler {
                handler.handle(ApplicationQuitRequest::Exit, cx);
            }
        }

        fn confirm_quit(&self, _: &mut App) {
            self.confirmations.set(self.confirmations.get() + 1);
        }
    }
}
