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
            format!("SpaceTerm is connecting to \u{201C}{server}\u{201D} over the network."),
            CaptionTone::Guidance,
            true,
            &[Cancel],
        ),
        DirectRouteStatus::Connected => (
            "Connected",
            "settings-direct-server-state-connected",
            format!(
                "This device is paired with \u{201C}{server}\u{201D} and connected over the \
                 network."
            ),
            CaptionTone::Guidance,
            false,
            &[Disconnect],
        ),
        DirectRouteStatus::Reconnecting => (
            "Reconnecting\u{2026}",
            "settings-direct-server-state-reconnecting",
            format!(
                "The connection to \u{201C}{server}\u{201D} was interrupted. Terminal screens \
                 stay as they were until SpaceTerm reconnects."
            ),
            CaptionTone::Warning,
            true,
            &[Disconnect],
        ),
        DirectRouteStatus::Disconnected => (
            "Disconnected",
            "settings-direct-server-state-disconnected",
            format!(
                "This device is still paired with \u{201C}{server}\u{201D}. Connect to present \
                 its Workspaces."
            ),
            CaptionTone::Guidance,
            false,
            &[Connect],
        ),
        DirectRouteStatus::NeedsInteraction => (
            "Waiting for you",
            "settings-direct-server-state-interaction",
            format!(
                "SpaceTerm needs your permission to use this device\u{2019}s key for \
                 \u{201C}{server}\u{201D}. It doesn\u{2019}t ask while it works in the \
                 background, so choose Resume to continue."
            ),
            CaptionTone::Warning,
            false,
            &[Resume, Disconnect],
        ),
        DirectRouteStatus::Refused(reason) => {
            let (state, selector, explanation, actions): (_, _, _, &'static [_]) = match reason {
                RefusalReason::Revoked => (
                    "Unpaired",
                    "settings-direct-server-state-revoked",
                    format!(
                        "\u{201C}{server}\u{201D} unpaired this device, so it can\u{2019}t \
                         connect. Join with a new invitation to pair it again."
                    ),
                    &[],
                ),
                RefusalReason::PinMismatch => (
                    "Different server",
                    "settings-direct-server-state-pin",
                    format!(
                        "The computer answering for \u{201C}{server}\u{201D} isn\u{2019}t the \
                         server this device paired with, so SpaceTerm didn\u{2019}t connect \
                         and sent it nothing."
                    ),
                    &[Connect],
                ),
                RefusalReason::InvalidProof => (
                    "Not verified",
                    "settings-direct-server-state-proof",
                    format!(
                        "\u{201C}{server}\u{201D} couldn\u{2019}t prove it\u{2019}s the server \
                         this device paired with, so SpaceTerm didn\u{2019}t connect."
                    ),
                    &[Connect],
                ),
                RefusalReason::Incompatible => (
                    "Incompatible",
                    "settings-direct-server-state-incompatible",
                    format!(
                        "This SpaceTerm and the server on \u{201C}{server}\u{201D} have no \
                         protocol version in common. Update the older one to connect."
                    ),
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
                    "SpaceTerm can\u{2019}t read this device\u{2019}s key for \
                     \u{201C}{server}\u{201D} from the system\u{2019}s credential storage. \
                     Other servers aren\u{2019}t affected."
                ),
                CaptionTone::Error,
                false,
                &[Resume],
            ),
            UnavailableReason::Network => (
                "Unreachable",
                "settings-direct-server-state-unreachable",
                format!(
                    "SpaceTerm can\u{2019}t reach \u{201C}{server}\u{201D} over the network \
                     and keeps trying quietly."
                ),
                CaptionTone::Warning,
                false,
                &[Connect, Disconnect],
            ),
            UnavailableReason::Storage => (
                "Unavailable",
                "settings-direct-server-state-storage",
                "SpaceTerm couldn\u{2019}t read or save its direct connections on this \
                 computer."
                    .to_owned(),
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
    let subject = if pending.enrollment_cleanup {
        format!(
            "A pairing with \u{201C}{server}\u{201D} that didn\u{2019}t finish left a request \
             on that server, which SpaceTerm is withdrawing."
        )
    } else if pending.server_confirmed {
        format!(
            "\u{201C}{server}\u{201D} confirmed the unpair. SpaceTerm still has to remove this \
             device\u{2019}s key for it from the system\u{2019}s credential storage."
        )
    } else if pending.self_unpair {
        format!(
            "This device\u{2019}s connection to \u{201C}{server}\u{201D} is removed here, but \
             the server hasn\u{2019}t confirmed that it revoked this device yet."
        )
    } else {
        format!("\u{201C}{server}\u{201D} hasn\u{2019}t confirmed the unpair you asked for yet.")
    };
    let (next, actions): (&str, &'static [UnpairAction]) = match pending.state {
        UnpairState::Retrying => ("SpaceTerm keeps trying quietly.", &[UnpairAction::Dismiss]),
        // Only Resume continues it, so it can't be dismissed out of reach.
        UnpairState::NeedsInteraction => (
            "To continue, SpaceTerm needs your permission to use the key the request was made \
             with. Choose Resume.",
            &[UnpairAction::Resume],
        ),
        UnpairState::Unconfirmed if pending.server_confirmed => (
            "Choose Retry to try again.",
            &[UnpairAction::Resume, UnpairAction::Dismiss],
        ),
        UnpairState::Unconfirmed => (
            "If that computer is gone for good, this stays unconfirmed. Choose Retry to try \
             again.",
            &[UnpairAction::Resume, UnpairAction::Dismiss],
        ),
    };
    (format!("{subject} {next}"), actions)
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
    fn an_unconfirmed_unpair_still_retrying_quietly_offers_retry_and_dismiss() {
        let (_, actions) = present_unpair(&pending(UnpairState::Unconfirmed));

        assert_eq!(actions, &[UnpairAction::Resume, UnpairAction::Dismiss]);
    }

    #[test]
    fn an_offline_self_unpair_says_removal_here_is_done_and_revocation_is_not_confirmed() {
        let (explanation, _) = present_unpair(&pending(UnpairState::Unconfirmed));

        assert!(explanation.contains("is removed here"), "{explanation}");
        assert!(
            explanation.contains("hasn\u{2019}t confirmed"),
            "{explanation}"
        );
    }

    #[test]
    fn a_confirmed_revocation_reports_only_the_key_cleanup_left() {
        let mut confirmed = pending(UnpairState::NeedsInteraction);
        confirmed.server_confirmed = true;

        let (explanation, _) = present_unpair(&confirmed);

        assert!(
            explanation.contains("confirmed the unpair"),
            "{explanation}"
        );
        assert!(
            !explanation.contains("hasn\u{2019}t confirmed"),
            "a validated revocation is never called unconfirmed: {explanation}"
        );
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
