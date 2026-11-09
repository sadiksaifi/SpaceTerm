//! SpaceTerm's answer to each native quit request (ADR 0049), apart from the windows, warnings
//! and Settings it then acts on. The macOS updater test compiles this file by itself and answers
//! the quit requests Sparkle sends with these same functions, so it uses nothing outside this
//! file.

/// Why the host asked the application to quit.
#[repr(u8)]
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
    Dismissal = 0,
    /// The client chose to exit, such as from its status item or an installed update.
    Exit = 1,
    /// The operating system is logging out, restarting, or shutting down. Never prompt.
    #[cfg_attr(
        not(target_os = "macos"),
        allow(
            dead_code,
            reason = "Linux session shutdown terminates the process outside application Quit"
        )
    )]
    SessionEnd = 2,
}

/// Where the update relaunch the person approved stands when a quit request arrives.
#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RelaunchPhase {
    /// No relaunch is approved.
    None = 0,
    /// The person approved a relaunch that has not begun to quit.
    Approved = 1,
    /// Settings are saving for the relaunch, or SpaceTerm already quits for it.
    Quitting = 2,
    /// Settings are saved and the updater holds the relaunch. Its quit request quits SpaceTerm.
    Released = 3,
}

/// What SpaceTerm does with one quit request.
#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum QuitStep {
    /// Terminate inside the request, with no warning: the session is ending.
    Proceed = 0,
    /// Save Settings, then hand the relaunch to the updater. Nothing closes and nothing asks.
    BeginRelaunch = 1,
    /// Nothing more: the relaunch already quits SpaceTerm.
    Wait = 2,
    /// Quit for the relaunch after the request returns, with no warning.
    QuitForRelaunch = 3,
    /// Close the windows, or warn about quitting when nothing would stay reachable.
    Dismiss = 4,
    /// Warn about quitting.
    Exit = 5,
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

/// While an approved update relaunch holds, no quit request is a dismissal or an exit: the
/// relaunch quits once, after Settings are saved and the updater holds the relaunch. A relaunch
/// that failed or was cancelled leaves quitting to ADR 0049 again.
pub(crate) fn quit_step(request: ApplicationQuitRequest, relaunch: RelaunchPhase) -> QuitStep {
    match (request, relaunch) {
        (ApplicationQuitRequest::SessionEnd, _) => QuitStep::Proceed,
        (_, RelaunchPhase::Approved) => QuitStep::BeginRelaunch,
        (_, RelaunchPhase::Quitting) => QuitStep::Wait,
        (_, RelaunchPhase::Released) => QuitStep::QuitForRelaunch,
        (ApplicationQuitRequest::Dismissal, RelaunchPhase::None) => QuitStep::Dismiss,
        (ApplicationQuitRequest::Exit, RelaunchPhase::None) => QuitStep::Exit,
    }
}

/// [`quit_request`] for the macOS updater test, which has no Rust of its own. `in_event` is
/// false outside any Apple Event.
#[unsafe(no_mangle)]
pub extern "C" fn spt_quit_request(
    event_class: u32,
    event_id: u32,
    has_reason: bool,
    in_event: bool,
) -> u8 {
    quit_request(in_event.then_some((event_class, event_id, has_reason))) as u8
}

/// [`quit_step`] for the macOS updater test. An unknown value reads as no relaunch and an exit.
#[unsafe(no_mangle)]
pub extern "C" fn spt_quit_step(request: u8, relaunch: u8) -> u8 {
    let request = match request {
        0 => ApplicationQuitRequest::Dismissal,
        2 => ApplicationQuitRequest::SessionEnd,
        _ => ApplicationQuitRequest::Exit,
    };
    let relaunch = match relaunch {
        1 => RelaunchPhase::Approved,
        2 => RelaunchPhase::Quitting,
        3 => RelaunchPhase::Released,
        _ => RelaunchPhase::None,
    };
    quit_step(request, relaunch) as u8
}
