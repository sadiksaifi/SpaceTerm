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

/// The warning for an actual client exit, or the notice that it can't be checked.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct QuitPrompt {
    pub(crate) title: String,
    pub(crate) detail: String,
    pub(crate) buttons: QuitButtons,
}

/// The answers a quit warning offers, in button order.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum QuitButtons {
    /// An installed build's Quit is the default answer.
    Quit,
    /// A Development quit ends Terminal Sessions, so focus rests on the safe Cancel and Return
    /// quits nothing.
    DestructiveQuit,
    /// The client can't check what quitting ends, so it offers only Cancel.
    CancelOnly,
}

impl QuitButtons {
    pub(crate) const fn answers(self) -> &'static [QuitAnswer] {
        match self {
            Self::Quit | Self::DestructiveQuit => &[QuitAnswer::Quit, QuitAnswer::Cancel],
            Self::CancelOnly => &[QuitAnswer::Cancel],
        }
    }
}

/// The person's answer to the quit warning.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum QuitAnswer {
    Quit,
    Cancel,
}

impl QuitAnswer {
    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Quit => "Quit",
            Self::Cancel => "Cancel",
        }
    }
}

/// Presents the quit warning through GPUI's prompt for this window.
pub(crate) fn present_window_quit(
    prompt: &QuitPrompt,
    window: &mut Window,
    cx: &mut App,
) -> Task<Option<QuitAnswer>> {
    let answers = prompt.buttons.answers();
    let buttons = answers
        .iter()
        .map(|answer| match answer {
            QuitAnswer::Quit => PromptButton::ok(answer.label()),
            QuitAnswer::Cancel => PromptButton::cancel(answer.label()),
        })
        .collect::<Vec<_>>();
    let answer = window.prompt(
        match prompt.buttons {
            QuitButtons::Quit => PromptLevel::Info,
            QuitButtons::DestructiveQuit => PromptLevel::Critical,
            QuitButtons::CancelOnly => PromptLevel::Warning,
        },
        &prompt.title,
        Some(&prompt.detail),
        &buttons,
        cx,
    );
    cx.spawn(async move |_| answers.get(answer.await.ok()?).copied())
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

    /// Takes the keyboard from the app in front before a warning appears on one of the client's
    /// windows, so the warning receives its keys. The menu bar item can ask while another app is
    /// in front. Hosts whose windows take the keyboard as they come forward need nothing.
    fn take_keyboard(&self, _cx: &mut App) {}

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

    /// Asks the quit warning by itself while the client has no window, with the same answers
    /// and keys as [`Self::present_confirmation`]. Any answer but Quit hides the client,
    /// returning focus to the app the person was using. Returns `None` when this host shows the
    /// warning only in a window.
    fn present_windowless_confirmation(
        &self,
        _prompt: &QuitPrompt,
        _cx: &mut App,
    ) -> Option<Task<Option<QuitAnswer>>> {
        None
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
        /// Warnings asked by themselves, oldest first, each with the channel that answers it.
        /// `None` for a host that shows the warning only in a window.
        windowless: Option<RefCell<Vec<WindowlessPrompt>>>,
        /// Each step of asking on a window, oldest first.
        on_window: RefCell<Vec<AskingOnWindow>>,
    }

    type WindowlessPrompt = (QuitPrompt, async_channel::Sender<QuitAnswer>);

    /// A step the coordinator takes to ask on one of the client's windows.
    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    pub(crate) enum AskingOnWindow {
        TookKeyboard,
        Presented,
    }

    impl RecordingApplicationQuitAdapter {
        pub(crate) fn with_last_window_policy(policy: LastWindowPolicy) -> Self {
            Self {
                last_window: Some(policy),
                ..Self::default()
            }
        }

        /// A host that asks the warning by itself while the client has no window.
        pub(crate) fn presenting_windowless(policy: LastWindowPolicy) -> Self {
            Self {
                last_window: Some(policy),
                windowless: Some(RefCell::default()),
                ..Self::default()
            }
        }

        /// The warnings asked by themselves that remain unanswered.
        pub(crate) fn windowless_prompts(&self) -> Vec<QuitPrompt> {
            self.windowless.as_ref().map_or_else(Vec::new, |prompts| {
                prompts
                    .borrow()
                    .iter()
                    .map(|(prompt, _)| prompt.clone())
                    .collect()
            })
        }

        /// Answers the oldest warning asked by itself. `None` ends it without an answer.
        pub(crate) fn answer_windowless(&self, answer: Option<QuitAnswer>) {
            let (_, answered) = self
                .windowless
                .as_ref()
                .expect("this host asks only in a window")
                .borrow_mut()
                .remove(0);
            if let Some(answer) = answer {
                answered.try_send(answer).unwrap();
            }
        }

        /// The steps taken to ask on a window since the last call, oldest first.
        pub(crate) fn take_asking_on_window(&self) -> Vec<AskingOnWindow> {
            self.on_window.take()
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

        fn take_keyboard(&self, _: &mut App) {
            self.on_window
                .borrow_mut()
                .push(AskingOnWindow::TookKeyboard);
        }

        fn present_confirmation(
            &self,
            prompt: &QuitPrompt,
            window: &mut Window,
            cx: &mut App,
        ) -> Task<Option<QuitAnswer>> {
            self.on_window.borrow_mut().push(AskingOnWindow::Presented);
            present_window_quit(prompt, window, cx)
        }

        fn present_windowless_confirmation(
            &self,
            prompt: &QuitPrompt,
            cx: &mut App,
        ) -> Option<Task<Option<QuitAnswer>>> {
            let (answered, answer) = async_channel::bounded(1);
            self.windowless
                .as_ref()?
                .borrow_mut()
                .push((prompt.clone(), answered));
            Some(cx.spawn(async move |_| answer.recv().await.ok()))
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
