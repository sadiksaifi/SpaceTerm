//! The desktop's SSH server connections and how each state reads.
//!
//! One app-wide registry owns every requested SSH server connection, its continuation and its
//! retained intent. The Workspace Window presents a connected server's Workspaces, and the
//! Servers Settings Section offers Connect and reports each connection. Reading a status performs
//! no remote work, so rendering stays free of discovery.

use std::rc::Rc;
use std::sync::{Arc, Mutex};

use gpui::{App, Global, SharedString, Window, WindowId};

use crate::domain::SshDestination;
use crate::platform::app_paths::AppPaths;
use crate::ssh::askpass::{AskPassAttempt, AskPassAttemptFactory, AskPassUnavailable};
use crate::ssh::server_connection::{
    ConnectPhase, ContinuityChange, IncompatibilityKind, InteractionReason, RefusalKind,
    RemoteConnectionStatus, ServerAvailability, SshIntentStoreError, SshServerConnections,
    SshServerFacts, UnavailableKind, UnconfirmedOperation,
};
use crate::ui::sidebar_window::form::CaptionTone;

struct InstalledSshServers {
    connections: SshServerConnections,
    hosts: Option<Arc<dyn SshHostSource>>,
    /// Where SSH's prompts appear, and the factory that presents them in another window.
    prompts: Option<(Arc<PromptWindow>, Rc<dyn SshServersFactory>)>,
}

impl Global for InstalledSshServers {}

/// SSH's authentication prompts, presented in one window. The registry outlives every window, so
/// the prompts move to the window where the person asks to connect, or to an open window once
/// theirs closes. A prompt already showing stays in its window; only later attempts move.
#[derive(Default)]
struct PromptWindow(Mutex<Option<(WindowId, Arc<dyn AskPassAttemptFactory>)>>);

impl PromptWindow {
    fn window(&self) -> Option<WindowId> {
        self.0
            .lock()
            .ok()
            .and_then(|bound| bound.as_ref().map(|(window, _)| *window))
    }

    fn bind(&self, window: WindowId, prompts: Arc<dyn AskPassAttemptFactory>) {
        if let Ok(mut bound) = self.0.lock() {
            *bound = Some((window, prompts));
        }
    }
}

impl AskPassAttemptFactory for PromptWindow {
    fn start_attempt(&self, paths: &AppPaths) -> Result<AskPassAttempt, AskPassUnavailable> {
        let prompts = self
            .0
            .lock()
            .ok()
            .and_then(|bound| bound.as_ref().map(|(_, prompts)| Arc::clone(prompts)))
            .ok_or(AskPassUnavailable)?;
        prompts.start_attempt(paths)
    }
}

/// Lists the SSH hosts a person can connect to. Listing reads this computer's SSH configuration
/// only; it reaches no host.
pub(crate) trait SshHostSource: Send + Sync {
    fn hosts(&self) -> Vec<SshDestination>;
}

/// Makes one registry the desktop's for a test, with no window presenting SSH's prompts.
#[cfg(test)]
pub(crate) fn install(
    connections: SshServerConnections,
    hosts: Option<Arc<dyn SshHostSource>>,
    cx: &mut App,
) {
    cx.set_global(InstalledSshServers {
        connections,
        hosts,
        prompts: None,
    });
}

/// Creates the desktop's registry and presents SSH's prompts in a window.
pub(crate) trait SshServersFactory {
    /// The registry, which asks for authentication through `prompts`. Creating it connects to
    /// nothing.
    fn create(
        &self,
        prompts: Arc<dyn AskPassAttemptFactory>,
        cx: &mut App,
    ) -> Option<SshServerConnections>;

    /// SSH's prompts presented in `window`.
    fn prompts(&self, window: &Window, cx: &mut App) -> Option<Arc<dyn AskPassAttemptFactory>>;

    /// The SSH hosts a person can choose from. `None` offers no list.
    fn hosts(&self) -> Option<Arc<dyn SshHostSource>>;
}

/// Installs the desktop's registry from its first Workspace Window, then resumes the connections
/// that were active when SpaceTerm last quit. Resuming never asks the person for anything: a
/// connection that needs a password, host approval, or a credential provider waits until the
/// person chooses Connect. Later windows reuse the installed registry and take SSH's prompts only
/// once the window presenting them has closed.
pub(crate) fn ensure(factory: Rc<dyn SshServersFactory>, window: &Window, cx: &mut App) {
    if cx.try_global::<InstalledSshServers>().is_some() {
        let open = prompt_window(cx).is_some_and(|bound| {
            cx.windows()
                .iter()
                .any(|window| window.window_id() == bound)
        });
        if !open {
            present_prompts_in(window, cx);
        }
        return;
    }
    let prompts = Arc::new(PromptWindow::default());
    let Some(connections) = factory.create(Arc::clone(&prompts) as Arc<_>, cx) else {
        return;
    };
    cx.set_global(InstalledSshServers {
        connections: connections.clone(),
        hosts: factory.hosts(),
        prompts: Some((prompts, factory)),
    });
    present_prompts_in(window, cx);
    connections.resume_retained();
}

/// Presents SSH's prompts in `window`, where the person just asked to reach a server, so a
/// sign-in or host approval appears in front of them rather than behind another window.
pub(crate) fn present_prompts_in(window: &Window, cx: &mut App) {
    let Some((prompts, factory)) = cx
        .try_global::<InstalledSshServers>()
        .and_then(|installed| installed.prompts.clone())
    else {
        return;
    };
    let id = window.window_handle().window_id();
    if prompts.window() == Some(id) {
        return;
    }
    if let Some(presenter) = factory.prompts(window, cx) {
        prompts.bind(id, presenter);
    }
}

/// The window SSH's prompts appear in, if any.
fn prompt_window(cx: &App) -> Option<WindowId> {
    cx.try_global::<InstalledSshServers>()
        .and_then(|installed| installed.prompts.as_ref())
        .and_then(|(prompts, _)| prompts.window())
}

/// The desktop's registry, or `None` in a build or test that installs none.
pub(crate) fn installed(cx: &App) -> Option<SshServerConnections> {
    cx.try_global::<InstalledSshServers>()
        .map(|installed| installed.connections.clone())
}

/// The desktop's SSH host list, if it offers one.
pub(crate) fn host_source(cx: &App) -> Option<Arc<dyn SshHostSource>> {
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
    /// Signs in and reads the server's state once, without connecting, starting, or installing.
    CheckStatus,
    /// Forgets the saved connection. The server, its Terminal Sessions and SSH access stay.
    Remove,
    /// Ends the intent to reconnect that a failed Connect left, keeping the connection saved.
    StopReconnecting,
}

impl SshServerAction {
    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Connect => "Connect",
            Self::Install => "Install\u{2026}",
            Self::Cancel => "Cancel",
            Self::Disconnect => "Disconnect",
            Self::CheckStatus => "Check Status",
            Self::Remove => "Remove",
            Self::StopReconnecting => "Don\u{2019}t Reconnect",
        }
    }

    pub(crate) const fn selector(self) -> &'static str {
        match self {
            Self::Connect => "settings-ssh-server-connect",
            Self::Install => "settings-ssh-server-install",
            Self::Cancel => "settings-ssh-server-cancel",
            Self::Disconnect => "settings-ssh-server-disconnect",
            Self::CheckStatus => "settings-ssh-server-check",
            Self::Remove => "settings-ssh-server-remove",
            Self::StopReconnecting => "settings-ssh-server-stop-reconnecting",
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
    use SshServerAction::{Cancel, CheckStatus, Connect, Disconnect, Install};

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
            &[Connect, CheckStatus],
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
        RemoteConnectionStatus::Connected {
            protocol_major,
            facts,
        } => (
            "Connected",
            "settings-ssh-server-state-connected",
            (
                SharedString::from(format!(
                    "Its Workspaces appear in the sidebar. {} Protocol version {protocol_major}.",
                    describe_server(facts)
                )),
                CaptionTone::Guidance,
            ),
            false,
            &[Disconnect],
        ),
        RemoteConnectionStatus::Checked {
            availability,
            facts,
        } => {
            let (state, selector, detail) = checked(*availability);
            (
                state,
                selector,
                (
                    SharedString::from(format!("{detail} {}", describe_server(facts))),
                    CaptionTone::Guidance,
                ),
                false,
                &[Connect, CheckStatus],
            )
        }
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
            &[Connect, CheckStatus],
        ),
        RemoteConnectionStatus::Incompatible { kind } => (
            "Incompatible",
            "settings-ssh-server-state-incompatible",
            error(incompatibility_detail(*kind)),
            false,
            &[Connect, CheckStatus],
        ),
        RemoteConnectionStatus::Refused { kind } => (
            "Refused",
            "settings-ssh-server-state-refused",
            error(refusal_detail(*kind)),
            false,
            &[Connect, CheckStatus],
        ),
        RemoteConnectionStatus::Unavailable { kind } => (
            "Unavailable",
            "settings-ssh-server-state-unavailable",
            error(unavailable_detail(*kind)),
            false,
            &[Connect, CheckStatus],
        ),
        RemoteConnectionStatus::Unconfirmed { operation } => (
            "Unconfirmed",
            "settings-ssh-server-state-unconfirmed",
            error(unconfirmed_detail(*operation)),
            false,
            &[Connect, CheckStatus],
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

/// One sentence naming the observed server's version and computer.
pub(crate) fn describe_server(facts: &SshServerFacts) -> String {
    let platform = platform_name(&facts.platform.0, &facts.platform.1);
    match &facts.version {
        Some(version) => format!("Version {version} on {platform}."),
        None => format!("{platform}."),
    }
}

/// The state, its selector, and its explanation after Check Status reached the server.
const fn checked(availability: ServerAvailability) -> (&'static str, &'static str, &'static str) {
    match availability {
        ServerAvailability::Running => (
            "Available",
            "settings-ssh-server-state-available",
            "The SpaceTerm server for this account is running. Connect to show its Workspaces.",
        ),
        ServerAvailability::NotRunning => (
            "Not Running",
            "settings-ssh-server-state-not-running",
            "The SpaceTerm server for this account is installed but not running. Connect starts \
             it.",
        ),
        ServerAvailability::NotInstalled => (
            "No Server",
            "settings-ssh-server-state-not-installed",
            "This account has no SpaceTerm server. Connect offers to install a verified one when \
             SpaceTerm has one for that computer.",
        ),
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
    }
}

const fn unavailable_detail(kind: UnavailableKind) -> &'static str {
    match kind {
        UnavailableKind::LocalIpcUnavailable => {
            "SpaceTerm couldn\u{2019}t prepare its SSH connection on this computer, so it \
             didn\u{2019}t reach the other computer. Nothing changed there."
        }
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
pub(crate) fn platform_name(os: &str, arch: &str) -> String {
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
    use std::sync::{Arc, Mutex};

    use crate::ssh::cancellation::SshCancellationToken;
    use crate::ssh::server_connection::{
        ConnectMode, ConnectProgress, ConnectedSshServer, RetainedSshConnection, SshConnectBackend,
        SshConnectFailure, SshConnectionIntentStore, SshServerContinuity, SshServerObservation,
    };

    /// A computer that always needs the person to sign in.
    struct NeedsSignIn;
    impl SshConnectBackend for NeedsSignIn {
        fn connect(
            &self,
            _: &SshDestination,
            _: ConnectMode,
            _: Option<&SshServerContinuity>,
            _: &SshCancellationToken,
            _: &ConnectProgress<'_>,
        ) -> Result<ConnectedSshServer, SshConnectFailure> {
            Err(SshConnectFailure::NeedsPerson(
                InteractionReason::Authentication,
            ))
        }
        fn observe(
            &self,
            _: &SshDestination,
            _: Option<&SshServerContinuity>,
            _: &SshCancellationToken,
            _: &ConnectProgress<'_>,
        ) -> Result<SshServerObservation, SshConnectFailure> {
            Err(SshConnectFailure::NeedsPerson(
                InteractionReason::Authentication,
            ))
        }
    }
    struct NoIntents;
    impl SshConnectionIntentStore for NoIntents {
        fn load(&self) -> Result<Vec<RetainedSshConnection>, SshIntentStoreError> {
            Ok(Vec::new())
        }
        fn retain(&self, _: RetainedSshConnection) -> Result<(), SshIntentStoreError> {
            Ok(())
        }
        fn remove(&self, _: &SshDestination) -> Result<(), SshIntentStoreError> {
            Ok(())
        }
    }

    /// Records each window SSH's prompts were bound to, and how many registries it created.
    #[derive(Default)]
    struct RecordingFactory {
        prompt_windows: Mutex<Vec<gpui::WindowId>>,
        registries: Mutex<usize>,
    }
    /// Presents nothing; the recording factory names the window it was created for.
    struct WindowPrompts;
    impl AskPassAttemptFactory for WindowPrompts {
        fn start_attempt(&self, _: &AppPaths) -> Result<AskPassAttempt, AskPassUnavailable> {
            Err(AskPassUnavailable)
        }
    }
    impl SshServersFactory for RecordingFactory {
        fn create(
            &self,
            _: Arc<dyn AskPassAttemptFactory>,
            _: &mut App,
        ) -> Option<SshServerConnections> {
            *self.registries.lock().unwrap() += 1;
            Some(SshServerConnections::new(
                Arc::new(NeedsSignIn),
                Arc::new(NoIntents),
            ))
        }
        fn prompts(&self, window: &Window, _: &mut App) -> Option<Arc<dyn AskPassAttemptFactory>> {
            self.prompt_windows
                .lock()
                .unwrap()
                .push(window.window_handle().window_id());
            Some(Arc::new(WindowPrompts))
        }
        fn hosts(&self) -> Option<Arc<dyn SshHostSource>> {
            None
        }
    }

    /// Closing the window that presented SSH's prompts and opening another moves the prompts to
    /// the open one, so a later Connect can still ask the person to sign in. The registry and the
    /// connections it knows stay as they were.
    #[gpui::test]
    fn prompts_move_to_a_reopened_window_while_the_registry_stays(cx: &mut gpui::TestAppContext) {
        let factory = Rc::new(RecordingFactory::default());
        let first = cx.add_window(|_, _| gpui::EmptyView);
        first
            .update(cx, |_, window, cx| {
                ensure(factory.clone() as Rc<dyn SshServersFactory>, window, cx)
            })
            .unwrap();
        let destination = SshDestination::new("penguin".to_owned()).unwrap();
        let registry = cx
            .update(|cx| installed(cx))
            .expect("the first window installs the registry");
        registry.connect(destination.clone());
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !matches!(
            registry.status(&destination),
            RemoteConnectionStatus::NeedsPerson { .. }
        ) {
            assert!(std::time::Instant::now() < deadline, "the attempt settles");
            std::thread::sleep(std::time::Duration::from_millis(10));
        }

        first
            .update(cx, |_, window, _| window.remove_window())
            .unwrap();
        cx.run_until_parked();
        let second = cx.add_window(|_, _| gpui::EmptyView);
        second
            .update(cx, |_, window, cx| {
                ensure(factory.clone() as Rc<dyn SshServersFactory>, window, cx)
            })
            .unwrap();

        assert_eq!(
            factory.prompt_windows.lock().unwrap().last().copied(),
            Some(second.window_id()),
            "SSH's prompts must be presented in a window that is still open"
        );
        assert_eq!(
            *factory.registries.lock().unwrap(),
            1,
            "reopening keeps the one registry"
        );
        assert!(
            matches!(
                cx.update(|cx| installed(cx)).unwrap().status(&destination),
                RemoteConnectionStatus::NeedsPerson { .. }
            ),
            "the registry keeps what it knew about each connection"
        );
    }

    /// A Connect asked for from another window, such as Settings in front of the Workspace
    /// Window, presents its prompts in that window, and a window opening later leaves them there.
    #[gpui::test]
    fn prompts_appear_in_the_window_where_the_person_asked(cx: &mut gpui::TestAppContext) {
        let factory = Rc::new(RecordingFactory::default());
        let workspace = cx.add_window(|_, _| gpui::EmptyView);
        workspace
            .update(cx, |_, window, cx| {
                ensure(factory.clone() as Rc<dyn SshServersFactory>, window, cx)
            })
            .unwrap();
        let settings = cx.add_window(|_, _| gpui::EmptyView);

        settings
            .update(cx, |_, window, cx| present_prompts_in(window, cx))
            .unwrap();
        let reopened = cx.add_window(|_, _| gpui::EmptyView);
        reopened
            .update(cx, |_, window, cx| {
                ensure(factory.clone() as Rc<dyn SshServersFactory>, window, cx)
            })
            .unwrap();
        settings
            .update(cx, |_, window, cx| present_prompts_in(window, cx))
            .unwrap();

        assert_eq!(
            *factory.prompt_windows.lock().unwrap(),
            [workspace.window_id(), settings.window_id()],
            "prompts move to where the person asked, once, and an opening window keeps them there"
        );
    }

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
