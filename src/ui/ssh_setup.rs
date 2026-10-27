//! SSH setup for Add Server: the composition seam, OpenSSH's prompts in the asking window, and
//! how each setup phase and outcome reads.
//!
//! SSH is temporary provisioning authority. One explicit attempt signs in once, asks the person
//! before it installs or starts the server or changes its network access, enrolls this device and
//! closes its SSH resources. The attempt succeeds only once this device is admitted over paired
//! QUIC, so every later operation uses that connection.

use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::mpsc::{RecvTimeoutError, SyncSender, sync_channel};
use std::time::Duration;

use gpui::{App, AppContext, Entity, EventEmitter, Global, SharedString, Window, WindowId};

use crate::cancellation::CancellationToken;
use crate::domain::SshDestination;
use crate::ssh::askpass::AskPassAttemptFactory;
use crate::ssh::provisioning::{
    IncompatibilityKind, RefusalKind, SetupApprovalHandler, SetupFailure, SetupOffer,
    SetupOperation, SetupPhase, SshSetupBackend, UnavailableKind,
};

/// How often a setup waiting for the person checks whether it was cancelled.
const APPROVAL_POLL: Duration = Duration::from_millis(50);

/// One offer a running setup waits on, until the person answers it or the setup ends.
pub(crate) struct PendingApproval {
    pub(crate) offer: SetupOffer,
    answer: SyncSender<bool>,
}

impl PendingApproval {
    /// Answers the offer. Dropping it unanswered declines it.
    pub(crate) fn answer(self, approved: bool) {
        let _ = self.answer.try_send(approved);
    }
}

/// Hands each offer to the window that started setup and waits on setup's own thread for the
/// person's answer. Cancellation, or a window that let go of the offer, declines it.
struct WindowApprovals(async_channel::Sender<PendingApproval>);

impl SetupApprovalHandler for WindowApprovals {
    fn approve(&self, offer: SetupOffer, cancellation: &CancellationToken) -> bool {
        let (answer, answered) = sync_channel(1);
        if self
            .0
            .send_blocking(PendingApproval { offer, answer })
            .is_err()
        {
            return false;
        }
        loop {
            if cancellation.is_cancelled() {
                return false;
            }
            match answered.recv_timeout(APPROVAL_POLL) {
                Ok(approved) => return approved && !cancellation.is_cancelled(),
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => return false,
            }
        }
    }
}

/// The approval handler one setup attempt runs with, and the offers it hands to the window.
pub(crate) fn window_approvals() -> (
    Arc<dyn SetupApprovalHandler>,
    async_channel::Receiver<PendingApproval>,
) {
    let (offers, pending) = async_channel::bounded(1);
    (Arc::new(WindowApprovals(offers)), pending)
}

/// Only an admitted AskPass request takes the setup sheet's place.
#[derive(Clone, Copy)]
pub(crate) enum SshPromptEvent {
    Requested,
    Settled { cancelled: bool },
}

#[derive(Default)]
pub(crate) struct SshPromptActivity {
    pub(crate) active: bool,
}
impl EventEmitter<SshPromptEvent> for SshPromptActivity {}

#[derive(Default)]
struct WindowPrompts(HashMap<WindowId, Entity<SshPromptActivity>>);
impl Global for WindowPrompts {}

pub(crate) fn prompt_activity(window: &Window, cx: &mut App) -> Entity<SshPromptActivity> {
    if cx.try_global::<WindowPrompts>().is_none() {
        cx.set_global(WindowPrompts::default());
        cx.on_window_closed(|cx, id| {
            cx.global_mut::<WindowPrompts>().0.remove(&id);
        })
        .detach();
    }
    let id = window.window_handle().window_id();
    if let Some(activity) = cx.global::<WindowPrompts>().0.get(&id) {
        return activity.clone();
    }
    let activity = cx.new(|_| SshPromptActivity::default());
    cx.global_mut::<WindowPrompts>()
        .0
        .insert(id, activity.clone());
    activity
}

pub(crate) fn prompt_requested(window: &Window, cx: &mut App) {
    prompt_activity(window, cx).update(cx, |activity, cx| {
        activity.active = true;
        cx.emit(SshPromptEvent::Requested);
    });
}

pub(crate) fn prompt_settled(window: WindowId, cancelled: bool, cx: &mut App) {
    let activity = cx
        .try_global::<WindowPrompts>()
        .and_then(|prompts| prompts.0.get(&window))
        .cloned();
    if let Some(activity) = activity {
        activity.update(cx, |activity, cx| {
            activity.active = false;
            cx.emit(SshPromptEvent::Settled { cancelled });
        });
    }
}

/// Lists the aliases this computer's OpenSSH configuration names, without reaching a host.
/// Reading can block on the filesystem, so callers run it off the UI thread.
pub(crate) trait SshHostSource: Send + Sync {
    fn hosts(&self) -> Vec<SshDestination>;
}

/// Creates the setup backend, presents OpenSSH's prompts in a window and lists configured hosts.
/// Creating any of them contacts no computer.
pub(crate) trait SshSetupFactory {
    /// The setup backend, or `None` when OpenSSH is unusable.
    fn backend(&self) -> Option<Arc<dyn SshSetupBackend>>;

    /// OpenSSH's prompts presented in `window`.
    fn prompts(&self, window: &Window, cx: &mut App) -> Option<Arc<dyn AskPassAttemptFactory>>;

    /// The source of the configured host list. Creating it reads nothing.
    fn hosts(&self) -> Arc<dyn SshHostSource>;
}

struct InstalledSshSetup(Rc<dyn SshSetupFactory>);
impl Global for InstalledSshSetup {}

/// Makes `factory` the desktop's SSH setup. The first Workspace Window installs it.
pub(crate) fn install(factory: Rc<dyn SshSetupFactory>, cx: &mut App) {
    cx.set_global(InstalledSshSetup(factory));
}

/// The desktop's SSH setup, or `None` in a build or test that installs none.
pub(crate) fn installed(cx: &App) -> Option<Rc<dyn SshSetupFactory>> {
    cx.try_global::<InstalledSshSetup>()
        .map(|installed| Rc::clone(&installed.0))
}

/// What the sheet says while `phase` runs.
pub(crate) const fn phase_detail(phase: SetupPhase) -> &'static str {
    match phase {
        SetupPhase::Authenticating => "Signing in\u{2026}",
        SetupPhase::Discovering => "Looking for SpaceTerm Server\u{2026}",
        SetupPhase::Installing => "Installing SpaceTerm Server\u{2026}",
        SetupPhase::Starting => "Starting SpaceTerm Server\u{2026}",
        SetupPhase::Enrolling => "Pairing this device\u{2026}",
        SetupPhase::Connecting => "Connecting over the network\u{2026}",
    }
}

/// How a failed attempt reads: its short state and what the person can do.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct SetupProblem {
    pub(crate) state: &'static str,
    pub(crate) explanation: SharedString,
    /// Whether trying again with the same values can help.
    pub(crate) retry: bool,
}

/// The presentation of a failure other than a cancellation, which the sheet handles itself.
pub(crate) fn problem(failure: &SetupFailure) -> SetupProblem {
    let fixed = |state, text: &'static str, retry| SetupProblem {
        state,
        explanation: SharedString::new_static(text),
        retry,
    };
    match failure {
        // The kept build is changed only on that computer, after which trying again can help.
        SetupFailure::Incompatible(IncompatibilityKind::Installation) => fixed(
            "Different Build",
            incompatibility_detail(IncompatibilityKind::Installation),
            true,
        ),
        SetupFailure::Incompatible(kind) => fixed("Incompatible", incompatibility_detail(*kind), false),
        SetupFailure::Refused(RefusalKind::MissingArtifact) => {
            fixed("Not Included", refusal_detail(RefusalKind::MissingArtifact), false)
        }
        SetupFailure::Refused(RefusalKind::InvalidEndpoint) => {
            fixed("Address Not Accepted", refusal_detail(RefusalKind::InvalidEndpoint), false)
        }
        SetupFailure::Refused(kind) => fixed("Refused", refusal_detail(*kind), true),
        SetupFailure::Unreachable { address } => SetupProblem {
            state: "Not Reachable",
            explanation: format!(
                "SpaceTerm Server is running, but this computer couldn\u{2019}t reach it at {}. Check the address and that any firewall allows UDP, or enter the address this computer uses to reach it.",
                address.authority()
            )
            .into(),
            retry: true,
        },
        SetupFailure::Unavailable(kind) => fixed("Unavailable", unavailable_detail(*kind), true),
        SetupFailure::Unconfirmed(operation) => {
            // A save SpaceTerm couldn't confirm may already be listed, so trying again could pair twice.
            fixed(
                "Unconfirmed",
                unconfirmed_detail(*operation),
                *operation != SetupOperation::Save,
            )
        }
        // The sheet ends or returns to its form on a cancellation; these words only cover a
        // stray one.
        SetupFailure::Cancelled => {
            fixed("Not Added", "Setup stopped before the server was added.", true)
        }
    }
}

/// How a setup that this computer couldn't start reads. It reached no other computer.
pub(crate) fn start_failure() -> SetupProblem {
    SetupProblem {
        state: "Couldn\u{2019}t Start",
        explanation: SharedString::new_static(
            "SpaceTerm couldn\u{2019}t start setup on this computer. Nothing was changed on the other computer. Try again.",
        ),
        retry: true,
    }
}

const fn incompatibility_detail(kind: IncompatibilityKind) -> &'static str {
    match kind {
        IncompatibilityKind::Protocol => "SpaceTerm and this server are incompatible.",
        IncompatibilityKind::Capability => "This server needs an update.",
        IncompatibilityKind::Platform => {
            "SpaceTerm Server is unavailable for this computer\u{2019}s operating system or processor."
        }
        IncompatibilityKind::Installation => {
            "That computer keeps a SpaceTerm Server build that differs from the one this SpaceTerm includes, so nothing was installed. Set up with the matching version of SpaceTerm, or manage the server on that computer, then try again."
        }
    }
}

const fn refusal_detail(kind: RefusalKind) -> &'static str {
    match kind {
        RefusalKind::AuthorizationRefused => {
            "This computer didn\u{2019}t allow the server to run after logout."
        }
        RefusalKind::InstallRefused => "The remote computer refused the installation.",
        RefusalKind::MissingArtifact => {
            "This SpaceTerm installation doesn\u{2019}t include a server package for this computer. Reinstall SpaceTerm, then try again."
        }
        RefusalKind::InvalidArtifact => "The server package failed verification.",
        RefusalKind::ServerChanged => {
            "The server changed during setup. Try again to set up the server now running there."
        }
        RefusalKind::InvalidEndpoint => "The server didn\u{2019}t accept that network address.",
    }
}

const fn unavailable_detail(kind: UnavailableKind) -> &'static str {
    match kind {
        UnavailableKind::SshFailed => "Check your SSH configuration, then try again.",
        UnavailableKind::HostUnreachable => {
            "Check that this computer is reachable over SSH, then try again."
        }
        UnavailableKind::ServiceUnavailable => "SpaceTerm Server did not answer. Try again.",
        UnavailableKind::EndpointUnreachable => {
            "This computer couldn\u{2019}t reach the server at that address."
        }
        UnavailableKind::CredentialUnavailable => {
            "SpaceTerm couldn\u{2019}t store this device\u{2019}s key. Unlock your keychain or keyring, then try again."
        }
        UnavailableKind::Storage => "SpaceTerm couldn\u{2019}t save the server on this computer.",
    }
}

const fn unconfirmed_detail(operation: SetupOperation) -> &'static str {
    match operation {
        SetupOperation::Install => {
            "SpaceTerm couldn\u{2019}t confirm the installation. Try again to check whether it finished."
        }
        SetupOperation::Start => {
            "SpaceTerm couldn\u{2019}t confirm that the server started. Try again to check."
        }
        SetupOperation::Enrollment => {
            "SpaceTerm couldn\u{2019}t confirm that the server paired this device. Try again to check."
        }
        SetupOperation::Save => {
            "The server paired this device, but SpaceTerm couldn\u{2019}t confirm it saved the connection. Check Servers for it before trying again."
        }
    }
}

/// A person-facing name for another computer's operating system and processor.
pub(crate) fn platform_name(os: &str, arch: &str) -> String {
    let arch = match arch.to_ascii_lowercase().as_str() {
        "aarch64" | "arm64" => "ARM",
        "x86_64" | "amd64" => "x86-64",
        _ => arch,
    };
    format!("{os} on {arch}")
}
