//! The desktop's direct network connections and how each state reads.
//!
//! One app-wide registry owns every saved direct route, its continuation and the unpair requests
//! awaiting a server's confirmation. The Workspace Window presents a connected route's
//! Workspaces, and the Servers Settings Section reports each route and offers its steps. Reading
//! a status performs no network work and asks for no credential, so rendering stays quiet; only
//! a step the person chooses may ask the system for this device's key.

use gpui::{App, Global, Task};

use crate::direct_connections::{
    DirectConnections, DirectRouteStatus, PendingUnpair, RefusalReason, UnavailableReason,
    UnpairState,
};
use crate::ui::sidebar_window::form::CaptionTone;

struct InstalledDirectServers(DirectConnections);

impl Global for InstalledDirectServers {}

/// Makes `connections` the desktop's registry. The first installation wins, so a later window
/// reuses it.
///
/// Quitting the application lets go of the registry, so its background worker ends with the
/// application rather than waking work the application no longer runs. Unfinished requests are
/// retained and resume when SpaceTerm next opens.
pub(crate) fn install(connections: DirectConnections, cx: &mut App) {
    if cx.has_global::<InstalledDirectServers>() {
        return;
    }
    cx.set_global(InstalledDirectServers(connections));
    cx.on_app_quit(|cx| {
        if cx.has_global::<InstalledDirectServers>() {
            drop(cx.remove_global::<InstalledDirectServers>());
        }
        async {}
    })
    .detach();
}

/// The desktop's registry, or `None` in a build or test that installs none.
pub(crate) fn installed(cx: &App) -> Option<DirectConnections> {
    cx.try_global::<InstalledDirectServers>()
        .map(|installed| installed.0.clone())
}

/// One view's subscription to the registry's changes.
///
/// The registry notifies from its own worker threads. Dropping this closes the subscription on the
/// view's thread before the view's task goes, and the registry fences that close against any
/// notification already underway, so no worker can wake a task the view no longer runs. The
/// registry itself keeps working for the rest of the application.
pub(crate) struct DirectChanges {
    connections: DirectConnections,
    receiver: async_channel::Receiver<()>,
    _follow: Task<()>,
}

impl DirectChanges {
    /// Subscribes to `connections` and hands the subscription to the task `follow` starts.
    pub(crate) fn follow(
        connections: &DirectConnections,
        follow: impl FnOnce(async_channel::Receiver<()>) -> Task<()>,
    ) -> Self {
        let receiver = connections.subscribe();
        let task = follow(receiver.clone());
        Self {
            connections: connections.clone(),
            receiver,
            _follow: task,
        }
    }
}

impl Drop for DirectChanges {
    fn drop(&mut self) {
        self.connections.close_subscription(&self.receiver);
    }
}

/// What a person can do with one direct route from the Servers Settings Section.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum DirectServerAction {
    Connect,
    /// Connects again, letting the system ask the person before it releases this device's key.
    Resume,
    Cancel,
    Disconnect,
}

impl DirectServerAction {
    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Connect => "Connect",
            Self::Resume => "Resume",
            Self::Cancel => "Cancel",
            Self::Disconnect => "Disconnect",
        }
    }

    pub(crate) const fn selector(self) -> &'static str {
        match self {
            Self::Connect => "settings-direct-server-connect",
            Self::Resume => "settings-direct-server-resume",
            Self::Cancel => "settings-direct-server-cancel",
            Self::Disconnect => "settings-direct-server-disconnect",
        }
    }
}

/// How one direct route's state reads, derived from the state alone.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct DirectServerPresentation {
    pub(crate) state: &'static str,
    pub(crate) state_selector: &'static str,
    pub(crate) explanation: String,
    pub(crate) tone: CaptionTone,
    pub(crate) working: bool,
    pub(crate) actions: &'static [DirectServerAction],
}

/// How `status` reads for the server named `server`.
pub(crate) fn present(status: DirectRouteStatus, server: &str) -> DirectServerPresentation {
    use DirectServerAction::{Cancel, Connect, Disconnect, Resume};
    let (state, state_selector, explanation, tone, working, actions): (
        _,
        _,
        String,
        _,
        _,
        &'static [DirectServerAction],
    ) = match status {
        DirectRouteStatus::Connecting => (
            "Connecting\u{2026}",
            "settings-direct-server-state-connecting",
            format!("Connecting to \"{server}\"."),
            CaptionTone::Guidance,
            true,
            &[Cancel],
        ),
        DirectRouteStatus::Connected => (
            "Connected",
            "settings-direct-server-state-connected",
            format!("Paired with \"{server}\" and connected."),
            CaptionTone::Guidance,
            false,
            &[Disconnect],
        ),
        DirectRouteStatus::Reconnecting => (
            "Reconnecting\u{2026}",
            "settings-direct-server-state-reconnecting",
            format!("The connection to \"{server}\" was interrupted."),
            CaptionTone::Warning,
            true,
            &[Disconnect],
        ),
        DirectRouteStatus::Disconnected => (
            "Disconnected",
            "settings-direct-server-state-disconnected",
            format!("Connect to show \"{server}\"'s Workspaces."),
            CaptionTone::Guidance,
            false,
            &[Connect],
        ),
        DirectRouteStatus::NeedsInteraction => (
            "Waiting for you",
            "settings-direct-server-state-interaction",
            format!("Choose Resume to allow SpaceTerm to use this device's key for \"{server}\"."),
            CaptionTone::Warning,
            false,
            &[Resume, Disconnect],
        ),
        DirectRouteStatus::Refused(reason) => {
            let (state, selector, explanation, actions): (_, _, _, &'static [_]) = match reason {
                RefusalReason::Revoked => (
                    "Unpaired",
                    "settings-direct-server-state-revoked",
                    format!("Join with a new invitation to pair with \"{server}\" again."),
                    &[],
                ),
                RefusalReason::PinMismatch => (
                    "Different server",
                    "settings-direct-server-state-pin",
                    format!(
                        "SpaceTerm refused to connect because \"{server}\" now reaches a different server."
                    ),
                    &[Connect],
                ),
                RefusalReason::InvalidProof => (
                    "Not verified",
                    "settings-direct-server-state-proof",
                    format!("SpaceTerm couldn't verify \"{server}\"'s identity."),
                    &[Connect],
                ),
                RefusalReason::Incompatible => (
                    "Incompatible",
                    "settings-direct-server-state-incompatible",
                    format!("SpaceTerm and \"{server}\" are incompatible."),
                    &[],
                ),
            };
            (
                state,
                selector,
                explanation,
                CaptionTone::Error,
                false,
                actions,
            )
        }
        DirectRouteStatus::Unavailable(reason) => match reason {
            UnavailableReason::CredentialStore => (
                "Key unavailable",
                "settings-direct-server-state-credential",
                format!(
                    "SpaceTerm couldn't access this device's key for \"{server}\". Choose Resume to try again."
                ),
                CaptionTone::Error,
                false,
                &[Resume],
            ),
            UnavailableReason::Network => (
                "Unreachable",
                "settings-direct-server-state-unreachable",
                format!("SpaceTerm can't reach \"{server}\"."),
                CaptionTone::Warning,
                false,
                &[Connect, Disconnect],
            ),
            UnavailableReason::Storage => (
                "Unavailable",
                "settings-direct-server-state-storage",
                "Couldn't read or save direct connections.".to_owned(),
                CaptionTone::Error,
                false,
                &[],
            ),
        },
    };
    DirectServerPresentation {
        state,
        state_selector,
        explanation,
        tone,
        working,
        actions,
    }
}

/// What a person can do about one unconfirmed unpair.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum UnpairAction {
    /// Tries the original request again now, letting the system ask before it releases the key.
    Resume,
    /// Hides the notice. SpaceTerm keeps trying.
    Dismiss,
}

impl UnpairAction {
    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Resume => "Resume",
            Self::Dismiss => "Dismiss",
        }
    }
}

/// How one pending request reads, and the steps it allows. A request the server confirmed says
/// so: only the cleanup of this device's key remains, which is a different thing to wait for than
/// the server's revocation.
pub(crate) fn present_unpair(pending: &PendingUnpair) -> (String, &'static [UnpairAction]) {
    let server = &pending.server_label;
    if pending.abandoned {
        return (
            format!(
                "SpaceTerm could not finish unpairing a device from \"{server}\" before paired connections moved to QUIC. Unpair that device again on the server's machine."
            ),
            &[UnpairAction::Dismiss],
        );
    }
    let subject = if pending.enrollment_cleanup {
        format!("SpaceTerm is cancelling the unfinished pairing with \"{server}\".")
    } else if pending.server_confirmed {
        format!("\"{server}\" unpaired this device. SpaceTerm is removing its saved key.")
    } else if pending.self_unpair {
        format!("Removed. Waiting for \"{server}\" to confirm.")
    } else {
        format!("SpaceTerm is still confirming that \"{server}\" unpaired the device.")
    };
    let (next, actions): (&str, &'static [UnpairAction]) = match pending.state {
        UnpairState::Retrying => ("", &[UnpairAction::Dismiss]),
        // Only Resume continues it, so it can't be dismissed out of reach.
        UnpairState::NeedsInteraction => (
            "Choose Resume to allow SpaceTerm to use the key needed to finish this request.",
            &[UnpairAction::Resume],
        ),
        UnpairState::Unconfirmed if pending.server_confirmed => (
            "Choose Resume to try again.",
            &[UnpairAction::Resume, UnpairAction::Dismiss],
        ),
        UnpairState::Unconfirmed => (
            "Choose Resume to check again.",
            &[UnpairAction::Resume, UnpairAction::Dismiss],
        ),
    };
    let explanation = match pending.state {
        UnpairState::Retrying => subject,
        UnpairState::NeedsInteraction => next.to_owned(),
        UnpairState::Unconfirmed => format!("{subject} {next}"),
    };
    (explanation, actions)
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::rc::Rc;

    use gpui::TestAppContext;

    use super::*;

    /// A view that let go of its subscription hears nothing more, even from a notification made on
    /// a registry thread. The test scheduler refuses any wake from another thread, so a subscription
    /// left registered fails the test as it ends.
    #[gpui::test]
    fn a_released_subscription_is_never_woken_from_a_registry_thread(cx: &mut TestAppContext) {
        let (direct, route) = crate::direct_connections::tests::offline("studio");
        // Disconnecting closes the route's gate, so an attempt already underway ends without
        // notifying, and only this test's own steps notify.
        direct.disconnect(&route);
        let heard = Rc::new(Cell::new(0_usize));
        let changes = {
            let heard = Rc::clone(&heard);
            cx.update(|cx| {
                DirectChanges::follow(&direct, |changes| {
                    cx.spawn(async move |_| {
                        while changes.recv().await.is_ok() {
                            heard.set(heard.get() + 1);
                        }
                    })
                })
            })
        };
        direct.disconnect(&route);
        cx.run_until_parked();
        assert_eq!(heard.get(), 1, "a live subscription hears the registry");

        drop(changes);
        let notifier = direct.clone();
        std::thread::spawn(move || notifier.disconnect(&route))
            .join()
            .expect("the registry thread notifies");
        cx.run_until_parked();

        assert_eq!(heard.get(), 1, "a released subscription hears nothing more");
    }

    fn pending(state: UnpairState) -> PendingUnpair {
        PendingUnpair {
            key: [1; 32],
            server_label: "studio".to_owned(),
            state,
            dismissed: false,
            enrollment_cleanup: false,
            self_unpair: true,
            server_confirmed: false,
            abandoned: false,
        }
    }

    #[test]
    fn a_quiet_retry_offers_no_step_that_could_ask_for_a_credential() {
        let (_, actions) = present_unpair(&pending(UnpairState::Retrying));

        assert_eq!(actions, &[UnpairAction::Dismiss]);
    }

    #[test]
    fn an_unpair_that_needs_the_person_offers_only_resume() {
        let (_, actions) = present_unpair(&pending(UnpairState::NeedsInteraction));

        assert_eq!(actions, &[UnpairAction::Resume]);
    }

    #[test]
    fn an_abandoned_revocation_asks_to_unpair_again_and_offers_only_dismiss() {
        let mut abandoned = pending(UnpairState::Unconfirmed);
        abandoned.abandoned = true;
        let (explanation, actions) = present_unpair(&abandoned);

        assert_eq!(actions, &[UnpairAction::Dismiss]);
        assert!(
            explanation.contains("Unpair that device again on the server's machine."),
            "{explanation}"
        );
    }

    #[test]
    fn an_unconfirmed_unpair_names_the_resume_action_it_offers() {
        let (explanation, actions) = present_unpair(&pending(UnpairState::Unconfirmed));

        assert_eq!(actions, &[UnpairAction::Resume, UnpairAction::Dismiss]);
        assert!(
            explanation.ends_with("Choose Resume to check again."),
            "{explanation}"
        );
        assert!(!explanation.contains("Retry"), "{explanation}");
    }

    #[test]
    fn an_offline_self_unpair_says_removal_here_is_done_and_revocation_is_not_confirmed() {
        let (explanation, _) = present_unpair(&pending(UnpairState::Unconfirmed));

        assert!(explanation.starts_with("Removed."), "{explanation}");
        assert!(
            explanation.contains("Waiting for \"studio\" to confirm"),
            "{explanation}"
        );
    }

    #[test]
    fn a_confirmed_revocation_reports_only_the_key_cleanup_left() {
        let mut confirmed = pending(UnpairState::Retrying);
        confirmed.server_confirmed = true;

        let (explanation, _) = present_unpair(&confirmed);

        assert!(
            explanation.contains("unpaired this device"),
            "{explanation}"
        );
        assert!(
            explanation.contains("removing its saved key"),
            "{explanation}"
        );
        assert!(
            !explanation.contains("hasn\u{2019}t confirmed"),
            "a validated revocation is never called unconfirmed: {explanation}"
        );
    }

    #[test]
    fn an_unconfirmed_key_cleanup_names_the_resume_action_it_offers() {
        let mut confirmed = pending(UnpairState::Unconfirmed);
        confirmed.server_confirmed = true;

        let (explanation, actions) = present_unpair(&confirmed);

        assert_eq!(actions, &[UnpairAction::Resume, UnpairAction::Dismiss]);
        assert!(
            explanation.ends_with("Choose Resume to try again."),
            "{explanation}"
        );
        assert!(!explanation.contains("Retry"), "{explanation}");
    }

    #[test]
    fn a_route_waiting_for_the_person_offers_resume_and_never_connects_quietly() {
        let presentation = present(DirectRouteStatus::NeedsInteraction, "studio");

        assert_eq!(
            presentation.actions.first(),
            Some(&DirectServerAction::Resume)
        );
        assert!(!presentation.actions.contains(&DirectServerAction::Connect));
    }

    #[test]
    fn a_revoked_route_offers_no_reconnection() {
        let presentation = present(DirectRouteStatus::Refused(RefusalReason::Revoked), "studio");

        assert!(presentation.actions.is_empty());
    }
}
