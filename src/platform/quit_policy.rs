//! SpaceTerm's answer to each native quit request (ADR 0049) and the sequence of an approved
//! update relaunch's quit, apart from the windows, warnings, Settings and updater it acts on. The
//! macOS updater test compiles this file into its fixture and answers the quit requests Sparkle
//! sends with it, so it uses nothing outside this file.

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
    /// A termination signal: the person or the system ends the process, such as `brew
    /// uninstall`. Never prompt; save and quit.
    Terminate,
}

/// What SpaceTerm does next for a quit request, or for a step of the relaunch quit it began.
#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum QuitAction {
    /// Terminate inside the request, with no warning: the session is ending.
    Proceed = 0,
    /// Nothing more for now.
    Nothing = 1,
    /// Save Settings for the relaunch, then report with [`RelaunchQuit::saved`].
    SaveSettings = 2,
    /// Hand the relaunch to the updater, then report with [`RelaunchQuit::released`].
    Release = 3,
    /// Quit once for the relaunch, after the current request returns, with no warning.
    Quit = 4,
    /// Settings couldn't be saved: fail the relaunch now and say why.
    FailUnsaved = 5,
    /// Close the windows, or warn about quitting when nothing would stay reachable.
    Dismiss = 6,
    /// Warn about quitting.
    Exit = 7,
    /// Save Settings and quit once, with no warning.
    Terminate = 8,
    /// Settings couldn't be saved for a relaunch that a termination signal also asked to end:
    /// fail the relaunch, then quit as [`QuitAction::Terminate`] does.
    FailUnsavedAndTerminate = 9,
}

/// The quit of an approved update relaunch, from the first quit request through the Settings
/// save and the updater to the one quit. `A` names one approval; each call passes the approval
/// the update service still holds. While one holds, no quit request is a dismissal or an exit,
/// and a relaunch that failed or was cancelled leaves quitting to ADR 0049 again.
#[derive(Debug)]
pub(crate) struct RelaunchQuit<A> {
    /// The approval this quit began for, and how far it got.
    begun: Option<(A, RelaunchStage)>,
    /// A termination signal arrived during the relaunch quit, so the process ends even when the
    /// relaunch fails before the updater holds it.
    terminate: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RelaunchStage {
    /// Settings are saving, then the updater receives the relaunch.
    Saving,
    /// The updater holds the relaunch. Its quit request quits SpaceTerm.
    Released,
    /// SpaceTerm quits for the relaunch.
    Quitting,
}

impl<A> Default for RelaunchQuit<A> {
    fn default() -> Self {
        Self {
            begun: None,
            terminate: false,
        }
    }
}

impl<A: Copy + Eq> RelaunchQuit<A> {
    /// Whether a relaunch quit is under way, so no other quit flow may act.
    pub(crate) fn active(&self) -> bool {
        self.begun.is_some()
    }

    pub(crate) fn request(
        &mut self,
        request: ApplicationQuitRequest,
        approval: Option<A>,
    ) -> QuitAction {
        let owed = self.settle(approval);
        if request == ApplicationQuitRequest::SessionEnd {
            return QuitAction::Proceed;
        }
        if owed {
            return QuitAction::Terminate;
        }
        let Some(approval) = approval else {
            return match request {
                ApplicationQuitRequest::Dismissal => QuitAction::Dismiss,
                ApplicationQuitRequest::Terminate => QuitAction::Terminate,
                _ => QuitAction::Exit,
            };
        };
        self.terminate |= request == ApplicationQuitRequest::Terminate;
        match &mut self.begun {
            Some((begun, stage)) if *begun == approval => match stage {
                RelaunchStage::Released => {
                    *stage = RelaunchStage::Quitting;
                    QuitAction::Quit
                }
                RelaunchStage::Saving | RelaunchStage::Quitting => QuitAction::Nothing,
            },
            _ => {
                self.begun = Some((approval, RelaunchStage::Saving));
                QuitAction::SaveSettings
            }
        }
    }

    /// Settings finished saving for the relaunch, or couldn't be saved.
    pub(crate) fn saved(&mut self, saved: bool, approval: Option<A>) -> QuitAction {
        if self.settle(approval) {
            return QuitAction::Terminate;
        }
        if !self.saving(approval) {
            return QuitAction::Nothing;
        }
        if saved {
            QuitAction::Release
        } else {
            self.begun = None;
            if std::mem::take(&mut self.terminate) {
                QuitAction::FailUnsavedAndTerminate
            } else {
                QuitAction::FailUnsaved
            }
        }
    }

    /// The updater received the relaunch. `updater_quits` when it asks to quit by itself once it
    /// holds the relaunch; otherwise SpaceTerm quits now.
    pub(crate) fn released(&mut self, updater_quits: bool, approval: Option<A>) -> QuitAction {
        if self.settle(approval) {
            return QuitAction::Terminate;
        }
        if !self.saving(approval) {
            return QuitAction::Nothing;
        }
        // A termination signal doesn't wait for the updater's own quit request.
        let (stage, action) = if updater_quits && !self.terminate {
            (RelaunchStage::Released, QuitAction::Nothing)
        } else {
            (RelaunchStage::Quitting, QuitAction::Quit)
        };
        if let Some((_, current)) = &mut self.begun {
            *current = stage;
        }
        action
    }

    fn saving(&self, approval: Option<A>) -> bool {
        matches!(
            (self.begun, approval),
            (Some((begun, RelaunchStage::Saving)), Some(approval)) if begun == approval
        )
    }

    /// A relaunch the update service no longer holds leaves quitting to ADR 0049. Returns
    /// whether a termination signal that arrived during it is still owed its quit.
    fn settle(&mut self, approval: Option<A>) -> bool {
        if approval.is_none() && self.begun.take().is_some() {
            return std::mem::take(&mut self.terminate);
        }
        false
    }
}

/// `kCoreEventClass`, `kAEQuitApplication` from the Apple Event Manager.
const CORE_EVENT_CLASS: u32 = u32::from_be_bytes(*b"aevt");
const QUIT_APPLICATION_EVENT: u32 = u32::from_be_bytes(*b"quit");

/// Reads a macOS termination request. A Quit Apple Event without a reason comes from the Dock's
/// Quit, and from Sparkle when an approved update relaunches the client. One with a reason means
/// the session is logging out, restarting, or shutting down. A termination outside any Apple
/// Event comes from the client itself, such as Command-Q or a confirmed quit.
#[cfg_attr(
    not(target_os = "macos"),
    allow(dead_code, reason = "only macOS reports Apple Events")
)]
pub(crate) fn quit_request(event: Option<(u32, u32, bool)>) -> ApplicationQuitRequest {
    match event {
        Some((CORE_EVENT_CLASS, QUIT_APPLICATION_EVENT, false)) => {
            ApplicationQuitRequest::Dismissal
        }
        Some((CORE_EVENT_CLASS, QUIT_APPLICATION_EVENT, true)) => {
            ApplicationQuitRequest::SessionEnd
        }
        _ => ApplicationQuitRequest::Exit,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One approved relaunch quits once: it saves Settings, hands the relaunch to the updater,
    /// and quits on the updater's request. A failed save fails it, and quitting follows ADR 0049
    /// again once the update service no longer holds the relaunch.
    #[test]
    fn a_relaunch_saves_releases_and_quits_once() {
        use ApplicationQuitRequest::{Dismissal, Exit, SessionEnd, Terminate};
        let mut quit = RelaunchQuit::default();
        assert_eq!(quit.request(Dismissal, None), QuitAction::Dismiss);
        assert_eq!(quit.request(Terminate, None), QuitAction::Terminate);
        assert_eq!(quit.request(Exit, Some(1)), QuitAction::SaveSettings);
        assert_eq!(quit.request(Dismissal, Some(1)), QuitAction::Nothing);
        assert_eq!(quit.saved(true, Some(1)), QuitAction::Release);
        assert_eq!(quit.released(true, Some(1)), QuitAction::Nothing);
        assert_eq!(quit.request(Dismissal, Some(1)), QuitAction::Quit);
        assert_eq!(
            quit.request(Exit, Some(1)),
            QuitAction::Nothing,
            "it quits once"
        );
        assert_eq!(quit.request(SessionEnd, Some(1)), QuitAction::Proceed);

        // An updater that leaves the quit to SpaceTerm quits as soon as it holds the relaunch.
        assert_eq!(quit.request(Exit, Some(2)), QuitAction::SaveSettings);
        assert_eq!(quit.saved(true, Some(2)), QuitAction::Release);
        assert_eq!(quit.released(false, Some(2)), QuitAction::Quit);

        assert_eq!(quit.request(Exit, Some(3)), QuitAction::SaveSettings);
        assert_eq!(quit.saved(false, Some(3)), QuitAction::FailUnsaved);
        assert!(!quit.active());
        assert_eq!(quit.request(Exit, None), QuitAction::Exit);
        // A relaunch that failed while Settings saved goes no further.
        assert_eq!(quit.request(Exit, Some(4)), QuitAction::SaveSettings);
        assert_eq!(quit.saved(true, None), QuitAction::Nothing);
        assert!(!quit.active());
    }

    /// Termination signals that arrive while a relaunch saves Settings still end the process
    /// when the relaunch fails before the updater holds it, once however many arrived.
    #[test]
    fn a_termination_signal_during_a_relaunch_save_is_kept() {
        use ApplicationQuitRequest::{Exit, Terminate};
        let mut quit = RelaunchQuit::default();
        assert_eq!(quit.request(Exit, Some(1)), QuitAction::SaveSettings);
        assert_eq!(quit.request(Terminate, Some(1)), QuitAction::Nothing);
        assert_eq!(quit.request(Terminate, Some(1)), QuitAction::Nothing);
        assert_eq!(
            quit.saved(false, Some(1)),
            QuitAction::FailUnsavedAndTerminate
        );
        assert_eq!(
            quit.request(Exit, None),
            QuitAction::Exit,
            "the quit is owed once"
        );

        // The relaunch fails some other way while Settings save.
        assert_eq!(quit.request(Terminate, Some(2)), QuitAction::SaveSettings);
        assert_eq!(quit.saved(true, None), QuitAction::Terminate);
        assert_eq!(quit.request(Exit, None), QuitAction::Exit);

        // A saved relaunch quits for the relaunch without waiting for the updater.
        assert_eq!(quit.request(Exit, Some(3)), QuitAction::SaveSettings);
        assert_eq!(quit.request(Terminate, Some(3)), QuitAction::Nothing);
        assert_eq!(quit.saved(true, Some(3)), QuitAction::Release);
        assert_eq!(quit.released(true, Some(3)), QuitAction::Quit);
    }
}
