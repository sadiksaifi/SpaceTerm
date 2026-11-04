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
use spaceterm_core::provisioning::{MINIMUM_GLIBC, MINIMUM_MACOS, SetupRefusal};

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

/// What the sheet says while `phase` runs on the machine named `name`.
pub(crate) fn phase_detail(phase: SetupPhase, name: &str) -> SharedString {
    match phase {
        SetupPhase::Authenticating => "Signing in\u{2026}".into(),
        SetupPhase::Discovering => format!("Checking {name}\u{2026}").into(),
        SetupPhase::Installing => "Installing SpaceTerm Core\u{2026}".into(),
        SetupPhase::Starting => "Starting SpaceTerm Core\u{2026}".into(),
        SetupPhase::Enrolling => "Pairing this device\u{2026}".into(),
        SetupPhase::Connecting => "Connecting over the network\u{2026}".into(),
    }
}

/// What the sheet says while Try Again turns Network Access on `name` back on, until pairing
/// begins.
pub(crate) fn restoring_network(name: &str) -> SharedString {
    format!("Turning Network Access on {name} back on\u{2026}").into()
}

/// How a failed attempt reads: one sentence, an optional footnote, and whether trying again
/// with the same values can help.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct SetupProblem {
    pub(crate) explanation: SharedString,
    pub(crate) footnote: Option<SharedString>,
    pub(crate) retry: bool,
}

impl SetupProblem {
    fn new(explanation: String, retry: bool) -> Self {
        Self {
            explanation: explanation.into(),
            footnote: None,
            retry,
        }
    }
}

/// The presentation of a failure to add the machine named `name`. The sheet handles a
/// cancellation itself; these words only cover a stray one.
pub(crate) fn problem(failure: &SetupFailure, name: &str) -> SetupProblem {
    let n = name;
    match failure {
        SetupFailure::Unsupported(refusal) => SetupProblem {
            explanation: refusal_sentence(*refusal, n).into(),
            footnote: matches!(refusal, SetupRefusal::UdpBlocked { .. })
                .then(|| format!("Network Access on {n} is off again.").into()),
            retry: retry_refusal(*refusal),
        },
        SetupFailure::Unreachable { address } => SetupProblem::new(
            format!(
                "SpaceTerm can\u{2019}t reach {n} at {}, so check the address and try again.",
                address.authority()
            ),
            true,
        ),
        // The kept build is changed only on that machine, after which trying again can help.
        SetupFailure::Incompatible(IncompatibilityKind::Installation) => SetupProblem::new(
            format!(
                "{n} keeps a SpaceTerm Core build that differs from the one this SpaceTerm includes, so set up with the matching SpaceTerm or manage SpaceTerm Core on {n}, then try again."
            ),
            true,
        ),
        SetupFailure::Incompatible(_) => SetupProblem::new(
            format!("SpaceTerm Core on {n} is incompatible with this SpaceTerm."),
            false,
        ),
        SetupFailure::Refused(kind) => SetupProblem::new(
            refusal_detail(*kind, n),
            !matches!(
                kind,
                RefusalKind::MissingArtifact | RefusalKind::InvalidEndpoint
            ),
        ),
        SetupFailure::Unavailable(kind) => SetupProblem::new(unavailable_detail(*kind, n), true),
        // A save SpaceTerm couldn't confirm may already be listed, so trying again could pair
        // twice.
        SetupFailure::Unconfirmed(operation) => SetupProblem::new(
            unconfirmed_detail(*operation, n),
            *operation != SetupOperation::Save,
        ),
        SetupFailure::Cancelled => {
            SetupProblem::new(format!("Setup stopped before {n} was added."), true)
        }
    }
}

/// How a setup that this computer couldn't start reads. It reached no other machine.
pub(crate) fn start_failure(name: &str) -> SetupProblem {
    SetupProblem::new(
        format!("SpaceTerm couldn\u{2019}t start setup, so nothing changed on {name}. Try again."),
        true,
    )
}

/// Whether trying again can help: a login session, linger and a firewall can each change while
/// the sheet stays open.
const fn retry_refusal(refusal: SetupRefusal) -> bool {
    matches!(
        refusal,
        SetupRefusal::NobodyLoggedIn { .. }
            | SetupRefusal::LingerDeclined
            | SetupRefusal::UdpBlocked { .. }
    )
}

/// A macOS or glibc release as people write it: "26" or "15.6", "2.31".
fn release((major, minor): (u16, u16), drop_zero: bool) -> String {
    if drop_zero && minor == 0 {
        major.to_string()
    } else {
        format!("{major}.{minor}")
    }
}

/// The one sentence that says why Add Machine refused the machine named `n`.
fn refusal_sentence(refusal: SetupRefusal, n: &str) -> String {
    let macos = release(MINIMUM_MACOS, true);
    let glibc = release(MINIMUM_GLIBC, false);
    match refusal {
        SetupRefusal::IntelMac => {
            format!("{n} is an Intel Mac, and SpaceTerm Core supports only Apple silicon.")
        }
        SetupRefusal::ArmLinux => {
            format!("{n} runs Linux on ARM, which SpaceTerm Core doesn\u{2019}t support.")
        }
        SetupRefusal::OtherSystem => {
            format!("{n} runs an operating system that SpaceTerm Core doesn\u{2019}t support.")
        }
        SetupRefusal::MacosTooOld { found: Some(found) } => format!(
            "{n} runs macOS {}, and SpaceTerm Core needs macOS {macos} or later.",
            release(found, true)
        ),
        SetupRefusal::MacosTooOld { found: None } => {
            format!("SpaceTerm Core needs macOS {macos} or later on {n}.")
        }
        SetupRefusal::GlibcTooOld { found: Some(found) } => format!(
            "{n} has glibc {}, and SpaceTerm Core needs glibc {glibc} or later.",
            release(found, false)
        ),
        SetupRefusal::GlibcTooOld { found: None } => {
            format!("{n} has no glibc, and SpaceTerm Core needs glibc {glibc} or later.")
        }
        SetupRefusal::RootAccount => format!(
            "SpaceTerm Core can\u{2019}t run as root, so sign in to {n} with a regular account."
        ),
        SetupRefusal::NoUserManager => {
            format!("{n} has no systemd user manager, which SpaceTerm Core needs.")
        }
        // Automatic login is unavailable while FileVault is on.
        SetupRefusal::NobodyLoggedIn { filevault: false } => {
            format!("{n} has nobody logged in, so turn on automatic login on {n} and try again.")
        }
        SetupRefusal::NobodyLoggedIn { filevault: true } => {
            format!("{n} needs someone logged in before SpaceTerm Core can run there.")
        }
        SetupRefusal::LingerDeclined => format!(
            "Setup stopped because {n} must let SpaceTerm Core keep running after you log out."
        ),
        SetupRefusal::UdpBlocked { port } => format!(
            "SpaceTerm can\u{2019}t reach {n} on UDP port {port}, so allow that port in {n}\u{2019}s firewall and try again."
        ),
    }
}

fn refusal_detail(kind: RefusalKind, n: &str) -> String {
    match kind {
        RefusalKind::InstallRefused => format!("{n} refused the installation."),
        RefusalKind::MissingArtifact => format!(
            "This SpaceTerm doesn\u{2019}t include SpaceTerm Core for {n}, so reinstall SpaceTerm and try again."
        ),
        RefusalKind::InvalidArtifact => {
            format!("The SpaceTerm Core package for {n} failed verification.")
        }
        RefusalKind::MachineChanged => format!(
            "SpaceTerm Core on {n} changed during setup, so try again to set up the one running there now."
        ),
        RefusalKind::InvalidEndpoint => {
            format!("SpaceTerm Core on {n} didn\u{2019}t accept that network address.")
        }
    }
}

fn unavailable_detail(kind: UnavailableKind, n: &str) -> String {
    match kind {
        UnavailableKind::SshFailed => format!(
            "SpaceTerm couldn\u{2019}t sign in to {n} over SSH, so check your SSH configuration and try again."
        ),
        UnavailableKind::HostUnreachable => format!(
            "SpaceTerm couldn\u{2019}t reach {n} over SSH, so check that it\u{2019}s online and try again."
        ),
        UnavailableKind::ServiceUnavailable => {
            format!("SpaceTerm Core on {n} didn\u{2019}t answer, so try again.")
        }
        UnavailableKind::EndpointUnreachable => {
            format!("SpaceTerm couldn\u{2019}t reach {n} at that address.")
        }
        UnavailableKind::CredentialUnavailable => "SpaceTerm couldn\u{2019}t store this device\u{2019}s key, so unlock your keychain or keyring and try again.".to_owned(),
        UnavailableKind::Storage => format!("SpaceTerm couldn\u{2019}t save {n} in Machines."),
        UnavailableKind::PortInUse => format!(
            "Another app on {n} is using that UDP port, so choose another port and try again."
        ),
    }
}

fn unconfirmed_detail(operation: SetupOperation, n: &str) -> String {
    match operation {
        SetupOperation::Install => format!(
            "SpaceTerm couldn\u{2019}t confirm the installation on {n}, so try again to check whether it finished."
        ),
        SetupOperation::Start => format!(
            "SpaceTerm couldn\u{2019}t confirm that SpaceTerm Core started on {n}, so try again to check."
        ),
        SetupOperation::Enrollment => format!(
            "SpaceTerm couldn\u{2019}t confirm that {n} paired this device, so try again to check."
        ),
        SetupOperation::Save => format!(
            "{n} paired this device, but SpaceTerm couldn\u{2019}t confirm it saved the connection, so check Machines before trying again."
        ),
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
