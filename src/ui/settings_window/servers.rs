//! The Servers section: the SpaceTerm Server for this account, this device's pairing with it, its
//! authenticated status, and the service that runs it while the person is logged in.
//! [`crate::local_server`] owns setup, pairing, authentication, and the service.
//!
//! Every build offers the section for its own identity's server. Opening Settings asks the server
//! nothing. Presenting the section checks the saved pairing and reads the service, and only the
//! person's explicit Set Up, Install, Start, Stop, Restart, or Uninstall changes either.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use gpui::prelude::*;
use gpui::{AnyElement, Context, ElementId, SharedString, Task, Window, accesskit, div};
use spaceterm_ui::{
    Alert, AlertIntent, AlertOutcome, Button, ModalAction, ModalActionEmphasis, ModalActionIntent,
    ModalActionRole, ModalId, ProgressRing, ProgressSize, ProgressState,
};

use super::SettingsWindow;
use crate::application_identity::ApplicationIdentity;
use crate::local_server::service::{Service, ServiceStatus};
use crate::local_server::{AuthenticatedStatus, LocalServer, ServerError};
use crate::ui::appearance::ChromeAppearance;
use crate::ui::sidebar_window::form::{CaptionTone, action_button, badge};

pub(super) const CONTROL_SELECTOR: &str = "settings-local-server-control";
pub(super) const PROGRESS_SELECTOR: &str = "settings-local-server-progress";
pub(super) const CHECK_SELECTOR: &str = "settings-local-server-check";
pub(super) const SET_UP_SELECTOR: &str = "settings-local-server-set-up";
pub(super) const START_SELECTOR: &str = "settings-local-server-start";
pub(super) const OPEN_DEVELOPMENT_PANE_SELECTOR: &str =
    "settings-local-server-open-development-pane";
pub(super) const SHARED_WORKSPACES_SELECTOR: &str = "settings-local-server-shared-workspaces";

/// The blocking local server operations the section runs on the background executor.
pub(super) trait LocalServerControl: Send + Sync {
    /// Authenticates with the saved pairing. Never creates an identity or starts a server.
    fn refresh(&self) -> Result<ServerConnection, ServerError>;
    /// Sets up and pairs when needed, starts the server when it is not running, and authenticates.
    fn setup_and_start(&self) -> Result<ServerConnection, ServerError>;
}

impl LocalServerControl for LocalServer {
    fn refresh(&self) -> Result<ServerConnection, ServerError> {
        LocalServer::refresh(self).map(ServerConnection::from)
    }

    fn setup_and_start(&self) -> Result<ServerConnection, ServerError> {
        LocalServer::setup_and_start(self).map(ServerConnection::from)
    }
}

/// What the section presents about an authenticated connection.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct ServerConnection {
    pub(super) protocol_major: u16,
    /// Whether the server said it runs the Development Pane's Terminal Session. A server that
    /// predates terminal operations authenticates without saying so.
    pub(super) development_pane: bool,
}

impl From<AuthenticatedStatus> for ServerConnection {
    fn from(status: AuthenticatedStatus) -> Self {
        Self {
            protocol_major: status.protocol_major,
            development_pane: status.development_pane,
        }
    }
}

/// The blocking service operations the section runs on the background executor. Each returns the
/// service's status after the operation, so the row never presents a state it assumed.
pub(super) trait ServerServiceControl: Send + Sync {
    /// Reads the service. Never installs, starts, or stops anything.
    fn status(&self) -> Result<ServiceStatus, ServerError>;
    /// Installs the service from a verified retained server installation, keeping an approved
    /// runtime the service already uses.
    fn install(&self, installation: &Path) -> Result<ServiceStatus, ServerError>;
    fn start(&self) -> Result<ServiceStatus, ServerError>;
    /// Stops the server, ends its Terminal Sessions, and keeps it stopped until an explicit Start.
    fn stop(&self) -> Result<ServiceStatus, ServerError>;
    fn restart(&self) -> Result<ServiceStatus, ServerError>;
    /// Stops and removes the service. Workspaces, pairings, and installations are kept.
    fn uninstall(&self) -> Result<ServiceStatus, ServerError>;
}

impl ServerServiceControl for Service {
    fn status(&self) -> Result<ServiceStatus, ServerError> {
        Service::status(self)
    }

    fn install(&self, installation: &Path) -> Result<ServiceStatus, ServerError> {
        Service::install(self, installation)
    }

    fn start(&self) -> Result<ServiceStatus, ServerError> {
        Service::start(self)
    }

    fn stop(&self) -> Result<ServiceStatus, ServerError> {
        Service::stop(self)
    }

    fn restart(&self) -> Result<ServiceStatus, ServerError> {
        Service::restart(self)
    }

    fn uninstall(&self) -> Result<ServiceStatus, ServerError> {
        Service::uninstall(self)
    }
}

/// The local server a Settings window presents, and the service that runs it where the platform
/// has one.
#[derive(Clone)]
pub(super) struct OfferedServer {
    pub(super) connection: Result<Arc<dyn LocalServerControl>, ServerError>,
    /// `None` omits the service row, which keeps the connection row's own Set Up and Start.
    pub(super) service: Option<Result<Arc<dyn ServerServiceControl>, ServerError>>,
}

/// The local server a Settings window presents. Every build offers one. `None` hides the Servers
/// section.
pub(super) type OfferedLocalServer = Option<OfferedServer>;

/// Resolves the running build's local server and its service. Resolution reads paths only: it
/// sets up, installs, starts, and reads credentials for nothing.
pub(super) fn offered_local_server() -> OfferedLocalServer {
    let identity = ApplicationIdentity::current().directory_name();
    let service = match Service::for_identity(identity) {
        // A platform without a service layer keeps the connection row's own workflow.
        Err(ServerError::Unsupported) => None,
        resolved => {
            Some(resolved.map(|service| Arc::new(service) as Arc<dyn ServerServiceControl>))
        }
    };
    Some(OfferedServer {
        connection: LocalServer::for_identity(identity)
            .map(|server| Arc::new(server) as Arc<dyn LocalServerControl>),
        service,
    })
}

/// The step a person can take from the row.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum LocalServerAction {
    /// Authenticates with the saved pairing.
    Check,
    /// Sets up the server's identity and this device's pairing, then starts the server.
    SetUp,
    /// Starts the paired server.
    Start,
}

impl LocalServerAction {
    /// The button label. Checking reads as the Updates section's check does.
    pub(super) const fn label(self) -> &'static str {
        match self {
            Self::Check => "Check Now",
            Self::SetUp => "Set Up Server",
            Self::Start => "Start Server",
        }
    }

    const fn selector(self) -> &'static str {
        match self {
            Self::Check => CHECK_SELECTOR,
            Self::SetUp => SET_UP_SELECTOR,
            Self::Start => START_SELECTOR,
        }
    }
}

/// What SpaceTerm last learned about the local server.
#[derive(Clone, Copy, Debug)]
pub(super) enum LocalServerStatus {
    /// Nothing has been asked yet.
    NotChecked,
    /// One action awaits its result.
    Working(LocalServerAction),
    Connected(ServerConnection),
    Failed(ServerError),
}

/// The row's complete presentation, derived from status alone so every state reads one way.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct LocalServerPresentation {
    pub(super) state: &'static str,
    /// Names the state badge, so a test can tell each state apart by geometry.
    pub(super) state_selector: &'static str,
    pub(super) explanation: SharedString,
    pub(super) tone: CaptionTone,
    /// The step the row offers, if any.
    pub(super) action: Option<LocalServerAction>,
    /// Whether an action is running. The row shows progress and withholds its action meanwhile.
    pub(super) working: bool,
    /// Whether the row offers Open Development Pane, which needs an authenticated server that
    /// runs it.
    pub(super) development_pane: bool,
    /// Whether the row offers Use Shared Workspaces, which presents this server's Workspaces in
    /// the Workspace Window beside the Workspaces the desktop owns.
    pub(super) shared_workspaces: bool,
}

/// Which of the server's two terminal surfaces a device may open now.
///
/// One device holds one authenticated connection per Pane. Two surfaces presenting the same Pane
/// would rebind its lease and fence each other into reconnecting, so only one is offered at a
/// time. Shared Workspaces are the ordinary route; the Development Pane window stays as a single
/// Pane for diagnosis.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct ServerSurfaces {
    pub(super) shared_workspaces: bool,
    pub(super) development_pane: bool,
}

impl ServerSurfaces {
    const SHARED_HOLDS: &'static str = "A Workspace Window is presenting this server's Workspaces. One device holds one \
         connection per Pane.";
    const DEVELOPMENT_PANE_HOLDS: &'static str = "The Development Pane window is presenting a Pane this server owns. Close it to present \
         this server's Workspaces instead.";

    /// What each surface offers, given which one is open.
    pub(super) fn resolve(shared_adopted: bool, development_pane_open: bool) -> Self {
        Self {
            shared_workspaces: !development_pane_open,
            development_pane: !shared_adopted,
        }
    }

    pub(super) const fn shared_workspaces_reason(self) -> Option<&'static str> {
        if self.shared_workspaces {
            None
        } else {
            Some(Self::DEVELOPMENT_PANE_HOLDS)
        }
    }

    pub(super) const fn development_pane_reason(self) -> Option<&'static str> {
        if self.development_pane {
            None
        } else {
            Some(Self::SHARED_HOLDS)
        }
    }
}

/// Owns the offered server, the last status, and the pending operation.
pub(super) struct LocalServerRow {
    control: Result<Arc<dyn LocalServerControl>, ServerError>,
    status: LocalServerStatus,
    /// Returns the background result to GPUI. Dropping the window drops it, and a late result then
    /// has nowhere to land.
    _operation: Option<Task<()>>,
}

impl LocalServerRow {
    pub(super) fn new(control: Result<Arc<dyn LocalServerControl>, ServerError>) -> Self {
        Self {
            control,
            status: LocalServerStatus::NotChecked,
            _operation: None,
        }
    }

    #[cfg(test)]
    pub(super) fn status(&self) -> LocalServerStatus {
        self.status
    }

    pub(super) fn presentation(&self) -> LocalServerPresentation {
        let status = match &self.control {
            Err(error) => LocalServerStatus::Failed(*error),
            Ok(_) => self.status,
        };
        let mut presentation = present(status);
        // A server that could not be resolved offers no step: checking again resolves nothing.
        if self.control.is_err() {
            presentation.action = None;
        }
        // Opening never sets up or starts the server, so it waits for a successful check. A
        // server that does not run the Development Pane is offered no terminal operation.
        presentation.development_pane = self.control.is_ok()
            && matches!(
                status,
                LocalServerStatus::Connected(ServerConnection {
                    development_pane: true,
                    ..
                })
            );
        // Presenting the server's Workspaces reads its inventory, so it waits for the same
        // successful check. The Workspace Window keeps the Workspaces it already owns.
        presentation.shared_workspaces = presentation.development_pane;
        presentation
    }
}

/// The presentation of one status.
pub(super) fn present(status: LocalServerStatus) -> LocalServerPresentation {
    use LocalServerAction as Action;

    let guidance =
        |explanation: &'static str| (SharedString::new_static(explanation), CaptionTone::Guidance);
    let (state, state_selector, (explanation, tone), action, working) = match status {
        LocalServerStatus::NotChecked => (
            "Not Checked",
            "settings-local-server-state-not-checked",
            guidance("SpaceTerm checks the server when you open this section."),
            Some(Action::Check),
            false,
        ),
        LocalServerStatus::Working(action) => (
            match action {
                Action::Check => "Checking",
                Action::SetUp => "Setting Up",
                Action::Start => "Starting",
            },
            "settings-local-server-state-working",
            guidance(match action {
                Action::Check => "Connecting to the server…",
                Action::SetUp => {
                    "Setting up the server and pairing this device. The system may ask you to \
                     allow access to its secure storage."
                }
                Action::Start => "Starting the server…",
            }),
            Some(action),
            true,
        ),
        LocalServerStatus::Connected(connection) => (
            "Connected",
            "settings-local-server-state-connected",
            (
                SharedString::from(format!(
                    "This device is paired and authenticated. Protocol version {}.",
                    connection.protocol_major
                )),
                CaptionTone::Guidance,
            ),
            Some(Action::Check),
            false,
        ),
        LocalServerStatus::Failed(error) => {
            let (state, state_selector, action) = failure(error);
            let explanation = match error {
                ServerError::NotPaired => guidance(
                    "Set Up creates a server for your account, pairs this device with it, saves \
                     the device's credential in the system's secure storage, and starts the \
                     server.",
                ),
                ServerError::NotRunning => {
                    guidance("This device is paired. Start the server to connect.")
                }
                // A deliberate Stop is a state the person chose, not a fault.
                ServerError::Stopped => guidance(STOPPED_GUIDANCE),
                ServerError::NotInstalled => {
                    guidance("Install the server's service below to start it.")
                }
                _ => (SharedString::from(error.to_string()), CaptionTone::Error),
            };
            (state, state_selector, explanation, action, false)
        }
    };
    LocalServerPresentation {
        state,
        state_selector,
        explanation,
        tone,
        action,
        working,
        development_pane: false,
        shared_workspaces: false,
    }
}

/// The badge, its selector, and the step that can recover from one failure.
fn failure(error: ServerError) -> (&'static str, &'static str, Option<LocalServerAction>) {
    use LocalServerAction as Action;

    match error {
        ServerError::NotPaired => (
            "Not Set Up",
            "settings-local-server-state-not-paired",
            Some(Action::SetUp),
        ),
        ServerError::NotRunning => (
            "Not Running",
            "settings-local-server-state-not-running",
            Some(Action::Start),
        ),
        ServerError::StartupFailed => (
            "Not Started",
            "settings-local-server-state-startup-failed",
            Some(Action::Start),
        ),
        // Only the service's explicit Start or Install reverses these, so the connection row offers
        // no step of its own. Setting up again would leave a stopped server stopped.
        ServerError::Stopped => ("Stopped", "settings-local-server-state-stopped", None),
        ServerError::NotInstalled => (
            "Not Installed",
            "settings-local-server-state-not-installed",
            None,
        ),
        // Unreadable or unsafe storage and a refused or incompatible server are never repaired by
        // setting up again, so the row offers only another check.
        ServerError::CredentialUnavailable
        | ServerError::StorageUnavailable
        | ServerError::ServiceUnavailable => (
            "Unavailable",
            "settings-local-server-state-unavailable",
            Some(Action::Check),
        ),
        ServerError::UnsafeStorage => (
            "Unsafe Storage",
            "settings-local-server-state-unsafe-storage",
            Some(Action::Check),
        ),
        ServerError::AuthenticationRefused => (
            "Refused",
            "settings-local-server-state-refused",
            Some(Action::Check),
        ),
        ServerError::IncompatibleProtocol => (
            "Incompatible",
            "settings-local-server-state-incompatible",
            Some(Action::Check),
        ),
        ServerError::Protocol => (
            "Protocol Error",
            "settings-local-server-state-protocol-error",
            Some(Action::Check),
        ),
        // Terminal Session refusals reach this row only through a check, which another check
        // answers afresh.
        ServerError::Controlled => (
            "In Use",
            "settings-local-server-state-controlled",
            Some(Action::Check),
        ),
        ServerError::SessionUnavailable | ServerError::StaleControl => (
            "Unavailable",
            "settings-local-server-state-session-unavailable",
            Some(Action::Check),
        ),
        ServerError::DirectoryUnavailable => (
            "Directory Unavailable",
            "settings-local-server-state-directory-unavailable",
            Some(Action::Check),
        ),
        ServerError::Unsupported => (
            "Unsupported",
            "settings-local-server-state-unsupported",
            Some(Action::Check),
        ),
        ServerError::RuntimeUnavailable => (
            "Runtime Unavailable",
            "settings-local-server-state-runtime-unavailable",
            Some(Action::Check),
        ),
        ServerError::AuthorizationRequired => (
            "Authorization Required",
            "settings-local-server-state-authorization-required",
            Some(Action::Check),
        ),
        ServerError::CleanupUnresolved => (
            "Cleanup Unresolved",
            "settings-local-server-state-cleanup-unresolved",
            Some(Action::Check),
        ),
        // The server is holding shared edit outcomes for clients that have not acknowledged
        // them. Another check reports whether they have since.
        ServerError::Capacity => (
            "At Capacity",
            "settings-local-server-state-capacity",
            Some(Action::Check),
        ),
    }
}

/// The connection row's guidance once the person has stopped the server.
const STOPPED_GUIDANCE: &str =
    "You stopped the server. It stays stopped, even after you log in again, until you start it.";

pub(super) const SERVICE_CONTROL_SELECTOR: &str = "settings-server-service-control";
pub(super) const SERVICE_PROGRESS_SELECTOR: &str = "settings-server-service-progress";
pub(super) const SERVICE_CHECK_SELECTOR: &str = "settings-server-service-check";
pub(super) const INSTALL_SELECTOR: &str = "settings-server-service-install";
pub(super) const START_SERVICE_SELECTOR: &str = "settings-server-service-start";
pub(super) const STOP_SELECTOR: &str = "settings-server-service-stop";
pub(super) const RESTART_SELECTOR: &str = "settings-server-service-restart";
pub(super) const UNINSTALL_SELECTOR: &str = "settings-server-service-uninstall";
pub(super) const STOP_CONFIRM_SELECTOR: &str = "settings-server-stop-confirm";
pub(super) const STOP_CANCEL_SELECTOR: &str = "settings-server-stop-cancel";
pub(super) const RESTART_CONFIRM_SELECTOR: &str = "settings-server-restart-confirm";
pub(super) const RESTART_CANCEL_SELECTOR: &str = "settings-server-restart-cancel";
pub(super) const UNINSTALL_CONFIRM_SELECTOR: &str = "settings-server-uninstall-confirm";
pub(super) const UNINSTALL_CANCEL_SELECTOR: &str = "settings-server-uninstall-cancel";

/// The step a person can take from the service row.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ServiceAction {
    /// Reads the service again.
    Check,
    /// Asks for a verified server installation, then installs and starts the service.
    Install,
    Start,
    Stop,
    Restart,
    Uninstall,
}

impl ServiceAction {
    /// The button label. Install asks for an installation first, so it ends with an ellipsis.
    /// Start and Stop use the names the person knows the actions by.
    pub(super) const fn label(self) -> &'static str {
        match self {
            Self::Check => "Check Now",
            Self::Install => "Install\u{2026}",
            Self::Start => "Start Server",
            Self::Stop => "Stop Server",
            Self::Restart => "Restart Server",
            Self::Uninstall => "Uninstall",
        }
    }

    const fn selector(self) -> &'static str {
        match self {
            Self::Check => SERVICE_CHECK_SELECTOR,
            Self::Install => INSTALL_SELECTOR,
            Self::Start => START_SERVICE_SELECTOR,
            Self::Stop => STOP_SELECTOR,
            Self::Restart => RESTART_SELECTOR,
            Self::Uninstall => UNINSTALL_SELECTOR,
        }
    }

    /// The element identity of the slot the button occupies. Install, Start, Stop, and Check share
    /// the leading slot, so keyboard focus stays on it when one replaces another.
    const fn slot(self) -> &'static str {
        match self {
            Self::Check | Self::Install | Self::Start | Self::Stop => {
                "settings-server-service-primary"
            }
            Self::Restart => "settings-server-service-secondary",
            Self::Uninstall => "settings-server-service-removal",
        }
    }

    /// What the button does beyond its name, for assistive technology.
    const fn consequence(self) -> Option<&'static str> {
        match self {
            Self::Stop => Some("Ends the server's Terminal Sessions. SpaceTerm asks first."),
            Self::Restart => Some("Ends the server's Terminal Sessions. SpaceTerm asks first."),
            Self::Uninstall => Some("Removes the service. SpaceTerm asks first."),
            Self::Install => Some("Asks for a verified server installation."),
            Self::Check | Self::Start => None,
        }
    }

    /// Whether the action changes the service. A check only reads it.
    const fn changes_service(self) -> bool {
        !matches!(self, Self::Check)
    }

    const fn progress(self) -> (&'static str, &'static str) {
        match self {
            Self::Check => ("Checking", "Reading the service\u{2026}"),
            Self::Install => ("Installing", "Installing and starting the server\u{2026}"),
            Self::Start => ("Starting", "Starting the server\u{2026}"),
            Self::Stop => (
                "Stopping",
                "Stopping the server and ending its Terminal Sessions\u{2026}",
            ),
            Self::Restart => (
                "Restarting",
                "Restarting the server. Its Terminal Sessions end\u{2026}",
            ),
            Self::Uninstall => ("Uninstalling", "Stopping and removing the service\u{2026}"),
        }
    }
}

/// The alert that asks before Stop, Restart, or Uninstall.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct ServiceConfirmation {
    id: &'static str,
    pub(super) title: &'static str,
    pub(super) message: &'static str,
    pub(super) detail: &'static str,
    pub(super) confirm: &'static str,
    confirm_selector: &'static str,
    cancel_selector: &'static str,
}

impl ServiceConfirmation {
    /// The confirmation for one action, or `None` for an action that needs none. Uninstalling a
    /// server that is not running ends no Terminal Session, and the message says so.
    pub(super) const fn for_action(action: ServiceAction, running: bool) -> Option<Self> {
        Some(match action {
            ServiceAction::Stop => Self {
                id: "settings-server-stop",
                title: "Stop Server",
                message: "Stop the server and end its Terminal Sessions?",
                detail: "Programs running in them stop. Workspaces are kept. The server stays \
                         stopped, even after you log in again, until you choose Start Server.",
                confirm: "Stop Server",
                confirm_selector: STOP_CONFIRM_SELECTOR,
                cancel_selector: STOP_CANCEL_SELECTOR,
            },
            ServiceAction::Restart => Self {
                id: "settings-server-restart",
                title: "Restart Server",
                message: "Restart the server and end its Terminal Sessions?",
                detail: "Programs running in them stop. Workspaces are kept, and each Pane starts \
                         a new shell when you next use it.",
                confirm: "Restart Server",
                confirm_selector: RESTART_CONFIRM_SELECTOR,
                cancel_selector: RESTART_CANCEL_SELECTOR,
            },
            ServiceAction::Uninstall => Self {
                id: "settings-server-uninstall",
                title: "Uninstall Server",
                message: if running {
                    "Uninstall the server and end its Terminal Sessions?"
                } else {
                    "Uninstall the server?"
                },
                detail: "It no longer starts when you log in. Workspaces, paired devices, and \
                         verified server installations are kept, so you can install it again.",
                confirm: "Uninstall",
                confirm_selector: UNINSTALL_CONFIRM_SELECTOR,
                cancel_selector: UNINSTALL_CANCEL_SELECTOR,
            },
            ServiceAction::Check | ServiceAction::Install | ServiceAction::Start => return None,
        })
    }
}

/// Where the service stands, read from its status.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ServicePhase {
    NotInstalled,
    Running,
    /// The person stopped it. It stays stopped until an explicit Start.
    Stopped,
    /// Installed and not stopped by the person, yet not running, such as after a crash.
    NotRunning,
}

impl ServicePhase {
    /// A running process wins over a recorded Stop, since the process is what ends Sessions.
    pub(super) fn of(status: ServiceStatus) -> Self {
        if !status.installed {
            Self::NotInstalled
        } else if status.running {
            Self::Running
        } else if status.stopped {
            Self::Stopped
        } else {
            Self::NotRunning
        }
    }

    /// The steps offered from this phase, in the order the row shows them.
    pub(super) fn actions(self) -> &'static [ServiceAction] {
        use ServiceAction as Action;
        match self {
            Self::NotInstalled => &[Action::Install],
            Self::Running => &[Action::Stop, Action::Restart, Action::Uninstall],
            Self::Stopped | Self::NotRunning => &[Action::Start, Action::Uninstall],
        }
    }
}

/// The service row's complete presentation, derived from its state alone.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct ServicePresentation {
    pub(super) state: &'static str,
    /// Names the state badge, so a test can tell each state apart by geometry.
    pub(super) state_selector: &'static str,
    pub(super) explanation: SharedString,
    pub(super) tone: CaptionTone,
    /// The steps the row offers, in order.
    pub(super) actions: Vec<ServiceAction>,
    /// The action awaiting its result. The row shows progress and withholds its steps meanwhile.
    pub(super) working: Option<ServiceAction>,
}

/// What the service row last learned and what it is doing.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(super) struct ServiceState {
    /// The last status read. `None` until the first read succeeds, or after a read failed.
    pub(super) status: Option<ServiceStatus>,
    /// Why the last read or operation failed, until the next action.
    pub(super) failure: Option<ServerError>,
    pub(super) working: Option<ServiceAction>,
}

/// The presentation of one service state. A failed operation keeps the badge and steps of the
/// status read after it, so the row says what is true now and why the operation did not finish.
pub(super) fn present_service(state: ServiceState) -> ServicePresentation {
    let guidance = |text: &'static str| (SharedString::new_static(text), CaptionTone::Guidance);
    let phase = state.status.map(ServicePhase::of);
    let (badge, state_selector, (explanation, tone)) = match (phase, state.failure) {
        (None, None) => (
            "Not Checked",
            "settings-server-service-state-not-checked",
            guidance("SpaceTerm reads the service when you open this section."),
        ),
        (None, Some(error)) => (
            "Unknown",
            "settings-server-service-state-unknown",
            (SharedString::from(error.to_string()), CaptionTone::Error),
        ),
        (Some(phase), failure) => {
            let (badge, selector, caption) = match phase {
                ServicePhase::NotInstalled => (
                    "Not Installed",
                    "settings-server-service-state-not-installed",
                    guidance(
                        "Installing runs the server for your account whenever you\u{2019}re \
                         logged in, even after SpaceTerm quits. No administrator password is \
                         needed.",
                    ),
                ),
                ServicePhase::Running => (
                    "Running",
                    "settings-server-service-state-running",
                    guidance(
                        "The server starts when you log in and keeps running after SpaceTerm \
                         quits.",
                    ),
                ),
                ServicePhase::Stopped => (
                    "Stopped",
                    "settings-server-service-state-stopped",
                    guidance(
                        "You stopped the server. It stays stopped, even after you log in again \
                         or open SpaceTerm, until you start it.",
                    ),
                ),
                ServicePhase::NotRunning => (
                    "Not Running",
                    "settings-server-service-state-not-running",
                    (
                        SharedString::new_static(
                            "The server is installed but not running. Start it to connect.",
                        ),
                        CaptionTone::Warning,
                    ),
                ),
            };
            let caption = failure.map_or(caption, |error| {
                (SharedString::from(error.to_string()), CaptionTone::Error)
            });
            (badge, selector, caption)
        }
    };
    let actions = phase.map_or_else(
        || vec![ServiceAction::Check],
        |phase| phase.actions().to_vec(),
    );
    let (label, explanation, tone) = match state.working {
        Some(action) => {
            let (label, explanation) = action.progress();
            (
                label,
                SharedString::new_static(explanation),
                CaptionTone::Guidance,
            )
        }
        None => (badge, explanation, tone),
    };
    ServicePresentation {
        state: label,
        state_selector,
        explanation,
        tone,
        actions,
        working: state.working,
    }
}

/// Owns the offered service, its last state, and the pending operation.
pub(super) struct ServerServiceRow {
    control: Result<Arc<dyn ServerServiceControl>, ServerError>,
    state: ServiceState,
    /// Whether the installation chooser is open, so a second Install waits for it.
    choosing: bool,
    /// Returns the background result to GPUI. Dropping the window drops it.
    _operation: Option<Task<()>>,
}

impl ServerServiceRow {
    pub(super) fn new(control: Result<Arc<dyn ServerServiceControl>, ServerError>) -> Self {
        Self {
            control,
            state: ServiceState::default(),
            choosing: false,
            _operation: None,
        }
    }

    pub(super) fn presentation(&self) -> ServicePresentation {
        match &self.control {
            // A service that could not be resolved offers no step: reading again resolves nothing.
            Err(error) => {
                let mut presentation = present_service(ServiceState {
                    failure: Some(*error),
                    ..ServiceState::default()
                });
                presentation.actions.clear();
                presentation
            }
            Ok(_) => present_service(self.state),
        }
    }

    /// Whether an operation that changes the service is running.
    fn changing(&self) -> bool {
        self.choosing
            || self
                .state
                .working
                .is_some_and(ServiceAction::changes_service)
    }
}

impl SettingsWindow {
    /// Checks the saved pairing as the section comes into view. A running action keeps its place.
    pub(super) fn check_local_server(&mut self, cx: &mut Context<Self>) {
        self.run_local_server(LocalServerAction::Check, cx);
    }

    /// Runs one action on the background executor. Only one runs at a time, and none while the
    /// service is changing: its result checks the connection afresh.
    pub(super) fn run_local_server(&mut self, action: LocalServerAction, cx: &mut Context<Self>) {
        if self
            .server_service
            .as_ref()
            .is_some_and(ServerServiceRow::changing)
        {
            return;
        }
        let Some(row) = &mut self.local_server else {
            return;
        };
        if matches!(row.status, LocalServerStatus::Working(_)) {
            return;
        }
        let Ok(control) = &row.control else {
            return;
        };
        let control = Arc::clone(control);
        row.status = LocalServerStatus::Working(action);
        let result = cx.background_spawn(async move {
            match action {
                LocalServerAction::Check => control.refresh(),
                LocalServerAction::SetUp | LocalServerAction::Start => control.setup_and_start(),
            }
        });
        row._operation = Some(cx.spawn(async move |settings, cx| {
            let result = result.await;
            let _ = settings.update(cx, |settings, cx| {
                if let Some(row) = &mut settings.local_server {
                    row.status = match result {
                        Ok(connection) => LocalServerStatus::Connected(connection),
                        Err(error) => LocalServerStatus::Failed(error),
                    };
                }
                // Setting up or starting can start the server, which the service row then reads.
                if action != LocalServerAction::Check {
                    settings.check_server_service(cx);
                }
                cx.notify();
            });
        }));
        cx.notify();
    }

    pub(super) fn local_server_presentation(&self) -> Option<LocalServerPresentation> {
        self.local_server.as_ref().map(LocalServerRow::presentation)
    }

    pub(super) fn server_service_presentation(&self) -> Option<ServicePresentation> {
        self.server_service
            .as_ref()
            .map(ServerServiceRow::presentation)
    }

    /// Whether the service row can take `action` now. One service action runs at a time, and a
    /// change waits for the connection row to finish.
    fn server_service_ready(&self, action: ServiceAction) -> bool {
        let connection_working = self.local_server_working();
        self.server_service.as_ref().is_some_and(|row| {
            row.control.is_ok()
                && !row.choosing
                && row.state.working.is_none()
                && !(action.changes_service() && connection_working)
        })
    }

    /// Whether the connection row has an action running.
    fn local_server_working(&self) -> bool {
        self.local_server
            .as_ref()
            .is_some_and(|row| matches!(row.status, LocalServerStatus::Working(_)))
    }

    /// Reads the service as the section comes into view. A running action keeps its place.
    pub(super) fn check_server_service(&mut self, cx: &mut Context<Self>) {
        self.run_server_service(ServiceAction::Check, cx);
    }

    /// Runs one action from the service row. Install asks for an installation first; Stop,
    /// Restart, and Uninstall ask for confirmation first, so they need a window.
    pub(super) fn request_server_service(
        &mut self,
        action: ServiceAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.server_service_ready(action) {
            return;
        }
        match action {
            ServiceAction::Check | ServiceAction::Start => self.run_server_service(action, cx),
            ServiceAction::Install => self.choose_service_installation(cx),
            ServiceAction::Stop | ServiceAction::Restart | ServiceAction::Uninstall => {
                self.confirm_server_service(action, window, cx);
            }
        }
    }

    /// Runs an action that needs no installation. Stop, Restart, and Uninstall run only after the
    /// person confirms.
    pub(super) fn run_server_service(&mut self, action: ServiceAction, cx: &mut Context<Self>) {
        debug_assert_ne!(action, ServiceAction::Install);
        self.run_service_operation(action, None, cx);
    }

    /// Installs from the installation the person chose.
    pub(super) fn install_server_service(&mut self, installation: PathBuf, cx: &mut Context<Self>) {
        self.run_service_operation(ServiceAction::Install, Some(installation), cx);
    }

    /// Runs one service operation on the background executor, then reads the service again when
    /// the operation failed, so the row presents the status that is true afterwards. Only one runs
    /// at a time, and a change waits for the connection row to finish.
    fn run_service_operation(
        &mut self,
        action: ServiceAction,
        installation: Option<PathBuf>,
        cx: &mut Context<Self>,
    ) {
        if !self.server_service_ready(action) {
            return;
        }
        let Some(row) = &mut self.server_service else {
            return;
        };
        let Ok(control) = &row.control else {
            return;
        };
        let control = Arc::clone(control);
        row.state.working = Some(action);
        row.state.failure = None;
        let result = cx.background_spawn(async move {
            let outcome = match (action, installation) {
                (ServiceAction::Install, Some(installation)) => control.install(&installation),
                (ServiceAction::Install, None) => Err(ServerError::RuntimeUnavailable),
                (ServiceAction::Check, _) => control.status(),
                (ServiceAction::Start, _) => control.start(),
                (ServiceAction::Stop, _) => control.stop(),
                (ServiceAction::Restart, _) => control.restart(),
                (ServiceAction::Uninstall, _) => control.uninstall(),
            };
            match outcome {
                Ok(status) => (Some(status), None),
                Err(error) if action == ServiceAction::Check => (None, Some(error)),
                Err(error) => (control.status().ok(), Some(error)),
            }
        });
        row._operation = Some(cx.spawn(async move |settings, cx| {
            let (status, failure) = result.await;
            let _ = settings.update(cx, |settings, cx| {
                if let Some(row) = &mut settings.server_service {
                    row.state = ServiceState {
                        status,
                        failure,
                        working: None,
                    };
                }
                // A change starts, stops, or replaces the server the connection row authenticates
                // with, so the connection is checked afresh.
                if action.changes_service() {
                    settings.check_local_server(cx);
                }
                cx.notify();
            });
        }));
        cx.notify();
    }

    /// Asks the system chooser for a verified server installation, then installs from it.
    /// Cancelling changes nothing.
    fn choose_service_installation(&mut self, cx: &mut Context<Self>) {
        if !self.server_service_ready(ServiceAction::Install) {
            return;
        }
        let Some(row) = &mut self.server_service else {
            return;
        };
        row.choosing = true;
        let selection = cx.prompt_for_paths(gpui::PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Install".into()),
        });
        cx.spawn(async move |settings, cx| {
            let chosen = match selection.await {
                Ok(Ok(Some(paths))) => paths.into_iter().next(),
                _ => None,
            };
            let _ = settings.update(cx, |settings, cx| {
                if let Some(row) = &mut settings.server_service {
                    row.choosing = false;
                }
                if let Some(installation) = chosen {
                    settings.install_server_service(installation, cx);
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    /// Asks before an action that ends the server's Terminal Sessions or removes the service.
    /// Cancel is the default, so Return and Escape both leave the server as it is.
    fn confirm_server_service(
        &mut self,
        action: ServiceAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let running = self
            .server_service
            .as_ref()
            .and_then(|row| row.state.status)
            .is_some_and(|status| ServicePhase::of(status) == ServicePhase::Running);
        let Some(confirmation) = ServiceConfirmation::for_action(action, running) else {
            return;
        };
        let owner = cx.weak_entity();
        let result = Alert::new(
            ModalId::new(confirmation.id),
            confirmation.title,
            confirmation.title,
            confirmation.message,
            vec![
                ModalAction::new(
                    true,
                    confirmation.confirm,
                    ModalActionRole::Affirmative,
                    confirmation.confirm_selector,
                )
                .with_intent(ModalActionIntent::Destructive)
                .with_emphasis(ModalActionEmphasis::Prominent),
                ModalAction::new(
                    false,
                    "Cancel",
                    ModalActionRole::Cancel,
                    confirmation.cancel_selector,
                ),
            ],
        )
        .intent(AlertIntent::Critical)
        .detail(confirmation.detail)
        .present(window, cx, move |outcome, cx| {
            if !matches!(
                outcome,
                AlertOutcome::Activated {
                    action_id: true,
                    ..
                }
            ) {
                return;
            }
            let _ = owner.update(cx, |settings, cx| settings.run_server_service(action, cx));
        });
        if result.is_err() {
            eprintln!("failed to present the SpaceTerm server service confirmation");
        }
    }

    /// The service's state badge, or progress while an action runs, and its steps.
    pub(super) fn render_server_service(
        &mut self,
        appearance: &ChromeAppearance,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(presentation) = self.server_service_presentation() else {
            return div().into_any_element();
        };
        let state_selector = presentation.state_selector;
        let state = if presentation.working.is_some() {
            ProgressRing::new(
                SERVICE_PROGRESS_SELECTOR,
                presentation.state,
                ProgressState::Indeterminate,
            )
            .size(ProgressSize::Compact)
            .debug_selector(SERVICE_PROGRESS_SELECTOR)
            .into_any_element()
        } else {
            div()
                .debug_selector(move || state_selector.to_owned())
                .child(badge(state_selector, presentation.state, appearance))
                .into_any_element()
        };
        // Assistive technology announces the state when it changes, as a status.
        let state = div()
            .id("settings-server-service-state")
            .role(accesskit::Role::Status)
            .aria_label(SharedString::new_static(presentation.state))
            .child(state);
        // The steps stay enabled while an action runs. A disabled button gives up keyboard focus,
        // and the leading slot keeps it as Stop gives way to Start. A step taken meanwhile is
        // ignored, and the status says why.
        let owner = cx.weak_entity();
        let actions = presentation.actions.iter().map(|&action| {
            let owner = owner.clone();
            Button::new(ElementId::from(action.slot()), action.label())
                .variant(spaceterm_ui::ButtonVariant::Outline)
                .size(spaceterm_ui::ButtonSize::Regular)
                .tab_stop(true)
                .debug_selector(action.selector())
                .when_some(action.consequence(), |button, consequence| {
                    button.accessibility_description(consequence)
                })
                .on_activate(move |_, window, cx| {
                    let _ = owner.update(cx, |settings, cx| {
                        settings.request_server_service(action, window, cx);
                    });
                })
        });
        div()
            .debug_selector(|| SERVICE_CONTROL_SELECTOR.to_owned())
            .flex()
            .flex_row()
            .items_center()
            .gap(appearance.spacing(8.0))
            .child(state)
            .children(actions)
            .into_any_element()
    }

    /// The state badge, or progress while an action runs, and the step beside it.
    pub(super) fn render_local_server(
        &mut self,
        appearance: &ChromeAppearance,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(presentation) = self.local_server_presentation() else {
            return div().into_any_element();
        };
        let state_selector = presentation.state_selector;
        let state = if presentation.working {
            ProgressRing::new(
                PROGRESS_SELECTOR,
                presentation.state,
                ProgressState::Indeterminate,
            )
            .size(ProgressSize::Compact)
            .debug_selector(PROGRESS_SELECTOR)
            .into_any_element()
        } else {
            div()
                .debug_selector(move || state_selector.to_owned())
                .child(badge(state_selector, presentation.state, appearance))
                .into_any_element()
        };
        // A service change checks the connection when it finishes, so the step waits for it.
        let service_changing = self
            .server_service
            .as_ref()
            .is_some_and(ServerServiceRow::changing);
        let owner = cx.weak_entity();
        let action = presentation.action.map(|action| {
            action_button(
                action.selector(),
                action.label(),
                !presentation.working && !service_changing,
                move |_, cx| {
                    let _ = owner.update(cx, |settings, cx| settings.run_local_server(action, cx));
                },
            )
        });
        let surfaces = ServerSurfaces::resolve(
            crate::ui::shared_workspaces::adopted(cx),
            crate::ui::development_pane::is_open(cx),
        );
        let development_pane = presentation.development_pane.then(|| {
            action_button(
                OPEN_DEVELOPMENT_PANE_SELECTOR,
                "Open Development Pane",
                surfaces.development_pane,
                |_, cx| crate::ui::development_pane::open_or_activate(cx),
            )
            .when_some(surfaces.development_pane_reason(), |button, reason| {
                button.accessibility_description(reason)
            })
        });
        // Presenting the server's Workspaces adds them to the Workspace Window's sidebar. The
        // Workspaces, Tabs, Panes, and Terminal Sessions the desktop owns are left untouched.
        let shared_workspaces = presentation.shared_workspaces.then(|| {
            action_button(
                SHARED_WORKSPACES_SELECTOR,
                "Use Shared Workspaces",
                surfaces.shared_workspaces,
                |_, cx| crate::ui::shared_workspaces::adopt(cx),
            )
            .when_some(surfaces.shared_workspaces_reason(), |button, reason| {
                button.accessibility_description(reason)
            })
        });
        div()
            .debug_selector(|| CONTROL_SELECTOR.to_owned())
            .flex()
            .flex_row()
            .items_center()
            .gap(appearance.spacing(8.0))
            .child(state)
            .children(action)
            .children(development_pane)
            .children(shared_workspaces)
            .into_any_element()
    }
}

/// A local server whose answers a test sets and whose operations it counts.
#[cfg(test)]
pub(super) mod testing {
    use std::sync::Arc;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use std::path::{Path, PathBuf};

    use super::{LocalServerControl, ServerConnection, ServerServiceControl};
    use crate::local_server::ServerError;
    use crate::local_server::service::ServiceStatus;

    /// A server whose answers the test sets and whose operations it counts.
    pub(in crate::ui::settings_window) struct ScriptedServer {
        refresh: Mutex<Result<ServerConnection, ServerError>>,
        setup: Mutex<Result<ServerConnection, ServerError>>,
        refreshes: AtomicUsize,
        setups: AtomicUsize,
    }

    impl ScriptedServer {
        pub(in crate::ui::settings_window) fn new(
            refresh: Result<ServerConnection, ServerError>,
        ) -> Arc<Self> {
            Arc::new(Self {
                refresh: Mutex::new(refresh),
                setup: Mutex::new(Ok(connected(2))),
                refreshes: AtomicUsize::new(0),
                setups: AtomicUsize::new(0),
            })
        }

        pub(in crate::ui::settings_window) fn answer_refresh(
            &self,
            result: Result<ServerConnection, ServerError>,
        ) {
            *self.refresh.lock().expect("refresh answer") = result;
        }

        pub(in crate::ui::settings_window) fn answer_setup(
            &self,
            result: Result<ServerConnection, ServerError>,
        ) {
            *self.setup.lock().expect("setup answer") = result;
        }

        pub(in crate::ui::settings_window) fn refreshes(&self) -> usize {
            self.refreshes.load(Ordering::SeqCst)
        }

        pub(in crate::ui::settings_window) fn setups(&self) -> usize {
            self.setups.load(Ordering::SeqCst)
        }
    }

    impl LocalServerControl for ScriptedServer {
        fn refresh(&self) -> Result<ServerConnection, ServerError> {
            self.refreshes.fetch_add(1, Ordering::SeqCst);
            *self.refresh.lock().expect("refresh answer")
        }

        fn setup_and_start(&self) -> Result<ServerConnection, ServerError> {
            self.setups.fetch_add(1, Ordering::SeqCst);
            *self.setup.lock().expect("setup answer")
        }
    }

    /// One operation a [`ScriptedService`] received.
    #[derive(Clone, Debug, Eq, PartialEq)]
    pub(in crate::ui::settings_window) enum ServiceCall {
        Status,
        Install(PathBuf),
        Start,
        Stop,
        Restart,
        Uninstall,
    }

    /// A service that changes its status as a real one would, refuses what the test tells it to,
    /// and records every operation.
    pub(in crate::ui::settings_window) struct ScriptedService {
        status: Mutex<Result<ServiceStatus, ServerError>>,
        /// The refusal the next changing operation returns, leaving the status unchanged.
        refusal: Mutex<Option<ServerError>>,
        calls: Mutex<Vec<ServiceCall>>,
    }

    impl ScriptedService {
        pub(in crate::ui::settings_window) fn new(
            status: Result<ServiceStatus, ServerError>,
        ) -> Arc<Self> {
            Arc::new(Self {
                status: Mutex::new(status),
                refusal: Mutex::new(None),
                calls: Mutex::new(Vec::new()),
            })
        }

        pub(in crate::ui::settings_window) fn refuse_next(&self, error: ServerError) {
            *self.refusal.lock().expect("refusal") = Some(error);
        }

        pub(in crate::ui::settings_window) fn calls(&self) -> Vec<ServiceCall> {
            self.calls.lock().expect("calls").clone()
        }

        /// The operations that changed or tried to change the service, without status reads.
        pub(in crate::ui::settings_window) fn changes(&self) -> Vec<ServiceCall> {
            self.calls()
                .into_iter()
                .filter(|call| *call != ServiceCall::Status)
                .collect()
        }

        fn change(
            &self,
            call: ServiceCall,
            next: impl FnOnce(ServiceStatus) -> ServiceStatus,
        ) -> Result<ServiceStatus, ServerError> {
            self.calls.lock().expect("calls").push(call);
            if let Some(error) = self.refusal.lock().expect("refusal").take() {
                return Err(error);
            }
            let mut status = self.status.lock().expect("status");
            let changed = next((*status).unwrap_or(not_installed()));
            *status = Ok(changed);
            Ok(changed)
        }
    }

    impl ServerServiceControl for ScriptedService {
        fn status(&self) -> Result<ServiceStatus, ServerError> {
            self.calls.lock().expect("calls").push(ServiceCall::Status);
            *self.status.lock().expect("status")
        }

        fn install(&self, installation: &Path) -> Result<ServiceStatus, ServerError> {
            // Installing keeps a recorded Stop, as the service does.
            self.change(ServiceCall::Install(installation.to_owned()), |status| {
                if status.stopped {
                    ServiceStatus {
                        installed: true,
                        ..status
                    }
                } else {
                    running()
                }
            })
        }

        fn start(&self) -> Result<ServiceStatus, ServerError> {
            self.change(ServiceCall::Start, |_| running())
        }

        fn stop(&self) -> Result<ServiceStatus, ServerError> {
            self.change(ServiceCall::Stop, |_| stopped())
        }

        fn restart(&self) -> Result<ServiceStatus, ServerError> {
            self.change(ServiceCall::Restart, |_| running())
        }

        fn uninstall(&self) -> Result<ServiceStatus, ServerError> {
            self.change(ServiceCall::Uninstall, |_| not_installed())
        }
    }

    pub(in crate::ui::settings_window) const fn not_installed() -> ServiceStatus {
        ServiceStatus {
            installed: false,
            enabled: false,
            running: false,
            stopped: false,
        }
    }

    pub(in crate::ui::settings_window) const fn running() -> ServiceStatus {
        ServiceStatus {
            installed: true,
            enabled: true,
            running: true,
            stopped: false,
        }
    }

    /// Stopped by the person, which disables automatic startup.
    pub(in crate::ui::settings_window) const fn stopped() -> ServiceStatus {
        ServiceStatus {
            installed: true,
            enabled: false,
            running: false,
            stopped: true,
        }
    }

    /// Installed and not stopped by the person, yet not running.
    pub(in crate::ui::settings_window) const fn not_running() -> ServiceStatus {
        ServiceStatus {
            installed: true,
            enabled: true,
            running: false,
            stopped: false,
        }
    }

    /// A server that runs the Development Pane.
    pub(in crate::ui::settings_window) const fn connected(protocol_major: u16) -> ServerConnection {
        ServerConnection {
            protocol_major,
            development_pane: true,
        }
    }

    /// A server from before terminal operations, which authenticates and offers status only.
    pub(in crate::ui::settings_window) const fn status_only(
        protocol_major: u16,
    ) -> ServerConnection {
        ServerConnection {
            protocol_major,
            development_pane: false,
        }
    }
}

#[cfg(test)]
#[path = "servers_tests.rs"]
mod tests;
