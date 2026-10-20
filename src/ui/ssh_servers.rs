//! The desktop's SSH server connections and how each state reads.
//!
//! One app-wide registry owns every requested SSH server connection, its continuation and its
//! retained intent. The Workspace Window presents a connected server's Workspaces, and the
//! Servers Settings Section offers Connect and reports each connection. Reading a status performs
//! no remote work, so rendering stays free of discovery.

use gpui::{App, Global, SharedString, Window};

use crate::domain::SshDestination;
use crate::ssh::server_connection::{
    ConnectPhase, ContinuityChange, IncompatibilityKind, InteractionReason, RefusalKind,
    RemoteConnectionStatus, SshIntentStoreError, SshServerConnections, UnavailableKind,
    UnconfirmedOperation,
};
use crate::ui::sidebar_window::form::CaptionTone;

struct InstalledSshServers {
    connections: SshServerConnections,
    hosts: Option<std::sync::Arc<dyn SshHostSource>>,
}

impl Global for InstalledSshServers {}

/// Lists the SSH hosts a person can connect to. Listing reads this computer's SSH configuration
/// only; it reaches no host.
pub(crate) trait SshHostSource: Send + Sync {
    fn hosts(&self) -> Vec<SshDestination>;
}

/// Makes one registry the desktop's for the life of the application.
pub(crate) fn install(
    connections: SshServerConnections,
    hosts: Option<std::sync::Arc<dyn SshHostSource>>,
    cx: &mut App,
) {
    cx.set_global(InstalledSshServers { connections, hosts });
}

/// Creates the desktop's registry. A window is required because SSH asks for authentication
/// through prompts that window presents.
pub(crate) trait SshServersFactory {
    fn create(&self, window: &Window, cx: &mut App) -> Option<SshServerConnections>;

    /// The SSH hosts a person can choose from. `None` offers no list.
    fn hosts(&self) -> Option<std::sync::Arc<dyn SshHostSource>>;
}

/// Installs the desktop's registry from its first Workspace Window, then resumes the connections
/// that were active when SpaceTerm last quit. Resuming never asks the person for anything: a
/// connection that needs a password, host approval, or a credential provider waits until the
/// person chooses Connect. Later windows reuse the installed registry.
pub(crate) fn ensure(factory: &dyn SshServersFactory, window: &Window, cx: &mut App) {
    if installed(cx).is_some() {
        return;
    }
    let Some(connections) = factory.create(window, cx) else {
        return;
    };
    install(connections.clone(), factory.hosts(), cx);
    connections.resume_retained();
}

/// The desktop's registry, or `None` in a build or test that installs none.
pub(crate) fn installed(cx: &App) -> Option<SshServerConnections> {
    cx.try_global::<InstalledSshServers>()
        .map(|installed| installed.connections.clone())
}

/// The desktop's SSH host list, if it offers one.
pub(crate) fn host_source(cx: &App) -> Option<std::sync::Arc<dyn SshHostSource>> {
    cx.try_global::<InstalledSshServers>()
        .and_then(|installed| installed.hosts.clone())
}

/// What a person can do with one connection from the Servers Settings Section.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SshServerAction {
    /// Connects, or connects again after a refusal or interruption the person can retry.
    Connect,
    /// Asks the person to approve the offered verified installation, then installs it.
    Install,
    Cancel,
    Disconnect,
}

impl SshServerAction {
    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Connect => "Connect",
            Self::Install => "Install\u{2026}",
            Self::Cancel => "Cancel",
            Self::Disconnect => "Disconnect",
        }
    }

    pub(crate) const fn selector(self) -> &'static str {
        match self {
            Self::Connect => "settings-ssh-server-connect",
            Self::Install => "settings-ssh-server-install",
            Self::Cancel => "settings-ssh-server-cancel",
            Self::Disconnect => "settings-ssh-server-disconnect",
        }
    }
}

/// One connection's complete presentation, derived from its status alone.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct SshServerPresentation {
    pub(crate) state: &'static str,
    /// Names the state badge, so a test can tell each state apart by geometry.
    pub(crate) state_selector: &'static str,
    pub(crate) explanation: SharedString,
    pub(crate) tone: CaptionTone,
    /// Whether work is underway. The row shows progress in place of the badge meanwhile.
    pub(crate) working: bool,
    /// The steps the row offers, the primary one first.
    pub(crate) actions: &'static [SshServerAction],
}

/// Why SpaceTerm may not reconnect to a server on its own after it reopens, or may reconnect to
/// one the person disconnected from.
pub(crate) const fn persistence_warning(error: SshIntentStoreError) -> &'static str {
    match error {
        SshIntentStoreError::Unavailable => {
            "SpaceTerm couldn\u{2019}t read or save which SSH servers to reconnect to when it \
             reopens. A server you connect to may not reconnect on its own, and one you \
             disconnected from may reconnect. Your connections work now."
        }
        SshIntentStoreError::Malformed => {
            "SpaceTerm\u{2019}s record of which SSH servers to reconnect to is damaged, so it \
             left the record unchanged. SSH servers won\u{2019}t reconnect on their own when \
             SpaceTerm reopens. Your connections work now."
        }
    }
}

/// The presentation of one connection's status.
pub(crate) fn present(status: &RemoteConnectionStatus) -> SshServerPresentation {
    use SshServerAction::{Cancel, Connect, Disconnect, Install};

    let guidance = |text: &'static str| (SharedString::new_static(text), CaptionTone::Guidance);
    let error = |text: &'static str| (SharedString::new_static(text), CaptionTone::Error);
    let (state, state_selector, (explanation, tone), working, actions): (
        _,
        _,
        _,
        _,
        &'static [SshServerAction],
    ) = match status {
        RemoteConnectionStatus::Inactive => (
            "Not Connected",
            "settings-ssh-server-state-inactive",
            guidance(
                "SpaceTerm signs in with your SSH configuration, then uses the SpaceTerm server \
                 for that account, or offers to install one.",
            ),
            false,
            &[Connect],
        ),
        RemoteConnectionStatus::Working { phase, cancellable } => (
            phase_label(*phase),
            "settings-ssh-server-state-working",
            (
                SharedString::new_static(phase_detail(*phase)),
                CaptionTone::Guidance,
            ),
            true,
            if *cancellable { &[Cancel] } else { &[] },
        ),
        RemoteConnectionStatus::Reconnecting {
            phase,
            attempt,
            next_retry,
        } => (
            "Reconnecting",
            "settings-ssh-server-state-reconnecting",
            (
                SharedString::from(format!(
                    "The connection was interrupted. {} Terminal screens stay frozen and typing \
                     is paused until it reconnects.",
                    match next_retry {
                        // Between attempts, the row says it waits rather than implying progress.
                        Some(_) if *attempt > 1 => {
                            "SpaceTerm keeps trying and waits a little longer each time.".to_owned()
                        }
                        Some(_) => "SpaceTerm tries again shortly.".to_owned(),
                        None => phase_detail(*phase).to_owned(),
                    }
                )),
                CaptionTone::Guidance,
            ),
            true,
            &[Disconnect],
        ),
        RemoteConnectionStatus::Cancelling => (
            "Cancelling",
            "settings-ssh-server-state-cancelling",
            guidance(
                "SpaceTerm is stopping the work it started. A step the server already accepted \
                 may still take effect.",
            ),
            true,
            &[],
        ),
        RemoteConnectionStatus::NeedsPerson { reason } => (
            "Waiting for You",
            "settings-ssh-server-state-needs-person",
            guidance(interaction_detail(*reason)),
            false,
            &[Connect, Disconnect],
        ),
        RemoteConnectionStatus::InstallationOffered {
            version, platform, ..
        } => (
            "Not Installed",
            "settings-ssh-server-state-installation-offered",
            (
                SharedString::from(format!(
                    "This account has no SpaceTerm server. SpaceTerm can install verified version \
                     {version} for {}.",
                    platform_name(&platform.0, &platform.1)
                )),
                CaptionTone::Guidance,
            ),
            false,
            &[Install, Disconnect],
        ),
        RemoteConnectionStatus::Connected { protocol_major, .. } => (
            "Connected",
            "settings-ssh-server-state-connected",
            (
                SharedString::from(format!(
                    "Its Workspaces appear in the sidebar. Protocol version {protocol_major}."
                )),
                CaptionTone::Guidance,
            ),
            false,
            &[Disconnect],
        ),
        RemoteConnectionStatus::Paused { reason } => (
            "Paused",
            "settings-ssh-server-state-paused",
            error(continuity_detail(*reason)),
            false,
            &[Disconnect],
        ),
        RemoteConnectionStatus::Stopped => (
            "Stopped",
            "settings-ssh-server-state-stopped",
            guidance(
                "The SpaceTerm server for this account was stopped. It stays stopped until it\u{2019}s \
                 started on that computer; connecting doesn\u{2019}t start it.",
            ),
            false,
            &[Connect],
        ),
        RemoteConnectionStatus::Incompatible { kind } => (
            "Incompatible",
            "settings-ssh-server-state-incompatible",
            error(incompatibility_detail(*kind)),
            false,
            &[Connect],
        ),
        RemoteConnectionStatus::Refused { kind } => (
            "Refused",
            "settings-ssh-server-state-refused",
            error(refusal_detail(*kind)),
            false,
            &[Connect],
        ),
        RemoteConnectionStatus::Unavailable { kind } => (
            "Unavailable",
            "settings-ssh-server-state-unavailable",
            error(unavailable_detail(*kind)),
            false,
            &[Connect],
        ),
        RemoteConnectionStatus::Unconfirmed { operation } => (
            "Unconfirmed",
            "settings-ssh-server-state-unconfirmed",
            error(unconfirmed_detail(*operation)),
            false,
            &[Connect],
        ),
    };
    SshServerPresentation {
        state,
        state_selector,
        explanation,
        tone,
        working,
        actions,
    }
}

const fn phase_label(phase: ConnectPhase) -> &'static str {
    match phase {
        ConnectPhase::Authenticating => "Signing In",
        ConnectPhase::Discovering => "Checking",
        ConnectPhase::Installing => "Installing",
        ConnectPhase::Starting => "Starting",
        ConnectPhase::Connecting => "Connecting",
    }
}

const fn phase_detail(phase: ConnectPhase) -> &'static str {
    match phase {
        ConnectPhase::Authenticating => {
            "Signing in with your SSH configuration. Answer any request from SSH to continue."
        }
        ConnectPhase::Discovering => "Looking for the SpaceTerm server for this account\u{2026}",
        ConnectPhase::Installing => "Installing the verified SpaceTerm server\u{2026}",
        ConnectPhase::Starting => "Starting the SpaceTerm server for this account\u{2026}",
        ConnectPhase::Connecting => "Connecting to the SpaceTerm server\u{2026}",
    }
}

const fn interaction_detail(reason: InteractionReason) -> &'static str {
    match reason {
        InteractionReason::Authentication => {
            "SSH needs you to sign in again. Choose Connect to answer it; SpaceTerm doesn\u{2019}t \
             ask in the background."
        }
        InteractionReason::HostVerification => {
            "SSH needs you to confirm this computer\u{2019}s identity. Choose Connect to review \
             it; SpaceTerm doesn\u{2019}t ask in the background."
        }
        InteractionReason::CredentialProvider => {
            "Your SSH key or credential provider needs your approval. Choose Connect to approve \
             it; SpaceTerm doesn\u{2019}t ask in the background."
        }
    }
}

const fn continuity_detail(change: ContinuityChange) -> &'static str {
    match change {
        ContinuityChange::HostChanged => {
            "This SSH destination now reaches a different computer, so SpaceTerm stopped \
             reconnecting. Nothing was sent to it."
        }
        ContinuityChange::AccountChanged => {
            "This SSH destination now signs in to a different account, so SpaceTerm stopped \
             reconnecting. Nothing was sent to it."
        }
        ContinuityChange::ServerChanged => {
            "The SpaceTerm server for this account changed, so SpaceTerm stopped reconnecting. \
             Its Workspaces may differ from the ones you used."
        }
        ContinuityChange::BuildChanged => {
            "The SpaceTerm server for this account is now a different version, so SpaceTerm \
             stopped reconnecting."
        }
    }
}

const fn incompatibility_detail(kind: IncompatibilityKind) -> &'static str {
    match kind {
        IncompatibilityKind::Protocol => {
            "SpaceTerm and the server for this account have no protocol version in common, so \
             SpaceTerm didn\u{2019}t connect. The server and its Terminal Sessions keep running, \
             and nothing was updated."
        }
        IncompatibilityKind::Capability => {
            "The server for this account doesn\u{2019}t support a feature this SpaceTerm \
             requires, so SpaceTerm didn\u{2019}t connect. The server and its Terminal Sessions \
             keep running, and nothing was updated."
        }
        IncompatibilityKind::Platform => {
            "SpaceTerm has no verified server for this computer\u{2019}s operating system or \
             processor. Nothing was installed."
        }
    }
}

const fn refusal_detail(kind: RefusalKind) -> &'static str {
    match kind {
        RefusalKind::AuthorizationRefused => {
            "The remote computer didn\u{2019}t allow the SpaceTerm server to keep running after \
             logout, so SpaceTerm didn\u{2019}t install it. Connect to try again."
        }
        RefusalKind::InstallRefused => {
            "The remote computer refused the installation. Nothing was installed."
        }
        RefusalKind::InvalidArtifact => {
            "The server package didn\u{2019}t pass verification, so SpaceTerm didn\u{2019}t \
             install it."
        }
        RefusalKind::IntentPersistence => {
            "SpaceTerm couldn\u{2019}t save this connection, so it won\u{2019}t reconnect on its \
             own after SpaceTerm reopens."
        }
    }
}

const fn unavailable_detail(kind: UnavailableKind) -> &'static str {
    match kind {
        UnavailableKind::Network => "The network is unavailable. Connect again when it returns.",
        UnavailableKind::HostUnreachable => {
            "SpaceTerm couldn\u{2019}t reach this computer. Check that it\u{2019}s on and \
             reachable, then connect again."
        }
        UnavailableKind::SshFailed => {
            "SSH couldn\u{2019}t connect with this destination. Check your SSH configuration, \
             then connect again."
        }
        UnavailableKind::ServiceUnavailable => {
            "The SpaceTerm server for this account didn\u{2019}t answer. Connect again to retry."
        }
        UnavailableKind::CandidateUnavailable => {
            "This SpaceTerm has no verified server package to install. Nothing was installed."
        }
    }
}

const fn unconfirmed_detail(operation: UnconfirmedOperation) -> &'static str {
    match operation {
        UnconfirmedOperation::Install => {
            "The connection ended before SpaceTerm could confirm the installation. It may have \
             finished. Connect to find out; nothing is installed twice."
        }
        UnconfirmedOperation::Connect => {
            "The connection ended before SpaceTerm could confirm it. Connect to try again."
        }
    }
}

/// A person-facing name for the other computer's operating system and processor. The backend
/// reports the system's display name, which this presents unchanged.
fn platform_name(os: &str, arch: &str) -> String {
    let arch = match arch.to_ascii_lowercase().as_str() {
        "aarch64" | "arm64" => "ARM",
        "x86_64" | "amd64" => "x86-64",
        _ => arch,
    };
    format!("{os} on {arch}")
}

/// What installing the offered server will do on the other computer, which the person approves
/// before anything is installed. `os` names that computer's system as the backend reports it; it
/// never describes this computer.
pub(crate) fn installation_consequence(os: &str) -> &'static str {
    match os.to_ascii_lowercase().as_str() {
        "linux" => {
            "SpaceTerm installs the server in your account on that computer and runs it as a \
             systemd user service. To keep it running after you log out, systemd needs lingering \
             turned on for your account; if it isn\u{2019}t already, SpaceTerm asks for your sudo \
             password on that computer. Your SSH settings and keys aren\u{2019}t changed."
        }
        "macos" | "darwin" => {
            "SpaceTerm installs the server in your account on that computer and runs it as a \
             login item (LaunchAgent), so it runs while you\u{2019}re logged in there. No \
             administrator password is needed. Your SSH settings and keys aren\u{2019}t changed."
        }
        _ => {
            "SpaceTerm installs the server in this account. Existing SSH settings and keys \
             aren\u{2019}t changed."
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The person can tell apart waiting for them, unavailable, stopped, refused, unconfirmed
    /// and incompatible, and no state the person didn't cause offers a step that installs or
    /// starts anything.
    #[test]
    fn each_outcome_reads_distinctly_and_only_an_offer_installs() {
        let outcomes = [
            RemoteConnectionStatus::NeedsPerson {
                reason: InteractionReason::Authentication,
            },
            RemoteConnectionStatus::Unavailable {
                kind: UnavailableKind::HostUnreachable,
            },
            RemoteConnectionStatus::Stopped,
            RemoteConnectionStatus::Refused {
                kind: RefusalKind::AuthorizationRefused,
            },
            RemoteConnectionStatus::Unconfirmed {
                operation: UnconfirmedOperation::Install,
            },
            RemoteConnectionStatus::Incompatible {
                kind: IncompatibilityKind::Protocol,
            },
            RemoteConnectionStatus::Paused {
                reason: ContinuityChange::HostChanged,
            },
        ];
        let presented: Vec<_> = outcomes.iter().map(present).collect();
        for (index, presentation) in presented.iter().enumerate() {
            assert!(
                presented[..index]
                    .iter()
                    .all(|earlier| earlier.state != presentation.state
                        && earlier.state_selector != presentation.state_selector),
                "{} must read differently from every other outcome",
                presentation.state
            );
            assert!(!presentation.working);
            assert!(!presentation.actions.contains(&SshServerAction::Install));
        }

        let offered = present(&RemoteConnectionStatus::InstallationOffered {
            offer_id: 1,
            version: "1.0.0".to_owned(),
            platform: ("Example OS".to_owned(), "x86_64".to_owned()),
        });
        assert_eq!(offered.actions.first(), Some(&SshServerAction::Install));
    }

    /// Work in progress offers Cancel, never a second Connect that would start competing work.
    #[test]
    fn work_in_progress_offers_only_cancel() {
        let working = present(&RemoteConnectionStatus::Working {
            phase: ConnectPhase::Installing,
            cancellable: true,
        });
        assert!(working.working);
        assert_eq!(working.actions, &[SshServerAction::Cancel]);
        let settling = present(&RemoteConnectionStatus::Cancelling);
        assert!(settling.actions.is_empty());
    }
}
