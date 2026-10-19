//! Account-local server connection and service controls. Construction has no effects.
//! Presenting this section checks the existing server; explicit actions set up or start it.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use gpui::prelude::*;
use gpui::{AnyElement, Context, ElementId, SharedString, Task, Window, accesskit, div};
use spaceterm_ui::{
    Alert, AlertIntent, AlertOutcome, Button, Menu, MenuEntry, ModalAction, ModalActionEmphasis,
    ModalActionIntent, ModalActionRole, ModalId, ProgressRing, ProgressSize, ProgressState,
};

use super::SettingsWindow;
use crate::application_identity::ApplicationIdentity;
pub(super) use crate::local_server::service::ServiceStartup;
use crate::local_server::service::{
    LingerAuthorization, RetainedInstallation, Service, ServiceStatus,
};
use crate::local_server::{AuthenticatedStatus, LocalServer, ServerError};
use crate::ui::appearance::ChromeAppearance;
use crate::ui::sidebar_window::form::{CaptionTone, action_button, badge};

pub(super) const CONTROL_SELECTOR: &str = "settings-local-server-control";
pub(super) const PROGRESS_SELECTOR: &str = "settings-local-server-progress";
pub(super) const CHECK_SELECTOR: &str = "settings-local-server-check";

/// The blocking local server check the section runs on the background executor. The Workspace
/// Window sets up and starts the server; this row only reports how it answers.
pub(super) trait LocalServerControl: Send + Sync {
    fn worktree_server(&self) -> Option<LocalServer> {
        None
    }
    /// Connects using owning-account authority. Never creates an identity or starts a server.
    fn refresh(&self) -> Result<ServerConnection, ServerError>;
}

impl LocalServerControl for LocalServer {
    fn worktree_server(&self) -> Option<LocalServer> {
        Some(self.clone())
    }
    fn refresh(&self) -> Result<ServerConnection, ServerError> {
        LocalServer::refresh(self).map(ServerConnection::from)
    }
}

/// What the section presents about an authenticated connection.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct ServerConnection {
    pub(super) protocol_major: u16,
}

impl From<AuthenticatedStatus> for ServerConnection {
    fn from(status: AuthenticatedStatus) -> Self {
        Self {
            protocol_major: status.protocol_major,
        }
    }
}

/// The blocking service operations the section runs on the background executor. Each returns the
/// service's status after the operation, so the row never presents a state it assumed.
pub(super) trait ServerServiceControl: Send + Sync {
    /// Reads the service. Never installs, starts, or stops anything.
    fn status(&self) -> Result<ServiceStatus, ServerError>;
    /// Lists the verified server builds kept for this account. Never installs or starts anything.
    fn retained_installations(&self) -> Result<Vec<RetainedInstallation>, ServerError>;
    /// Installs the service from a verified retained server installation, keeping an approved
    /// runtime the service already uses.
    fn install(&self, installation: &Path) -> Result<ServiceStatus, ServerError>;
    fn start(&self) -> Result<ServiceStatus, ServerError>;
    /// Stops the server, ends its Terminal Sessions, and keeps it stopped until an explicit Start.
    fn stop(&self) -> Result<ServiceStatus, ServerError>;
    fn restart(&self) -> Result<ServiceStatus, ServerError>;
    /// Stops and removes the service. Workspaces and installations are kept.
    fn uninstall(&self) -> Result<ServiceStatus, ServerError>;
    /// When the service starts the server.
    fn startup(&self) -> ServiceStartup;
    /// Asks the operating system to let this account's services run at boot and after logout.
    /// Runs only from the person's confirmed authorization step. A cancelled or refused
    /// authorization returns an error and changes nothing.
    fn authorize_startup(&self) -> Result<(), ServerError>;
}

impl ServerServiceControl for Service {
    fn status(&self) -> Result<ServiceStatus, ServerError> {
        Service::status(self)
    }

    fn retained_installations(&self) -> Result<Vec<RetainedInstallation>, ServerError> {
        Service::retained_installations(self)
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

    fn startup(&self) -> ServiceStartup {
        Service::startup(self)
    }

    fn authorize_startup(&self) -> Result<(), ServerError> {
        // The desktop asks through pkexec for the one fixed loginctl command, for this account
        // only. No SpaceTerm code runs as root.
        Service::enable_linger(self, LingerAuthorization::Desktop)
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
/// sets up, installs, starts, and connects to nothing.
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

/// What SpaceTerm last learned about the local server.
#[derive(Clone, Copy, Debug)]
pub(super) enum LocalServerStatus {
    /// Nothing has been asked yet.
    NotChecked,
    /// A check awaits its result.
    Checking,
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
    /// Whether the row offers Check Now.
    pub(super) check: bool,
    /// Whether a check is running. The row shows progress and withholds Check Now meanwhile.
    pub(super) working: bool,
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
    pub(super) fn worktree_server(&self) -> Option<LocalServer> {
        self.control.as_ref().ok()?.worktree_server()
    }
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
            presentation.check = false;
        }
        presentation
    }
}

/// The presentation of one status.
pub(super) fn present(status: LocalServerStatus) -> LocalServerPresentation {
    let guidance =
        |explanation: &'static str| (SharedString::new_static(explanation), CaptionTone::Guidance);
    let (state, state_selector, (explanation, tone), check, working) = match status {
        LocalServerStatus::NotChecked => (
            "Not Checked",
            "settings-local-server-state-not-checked",
            guidance("SpaceTerm checks the server when you open this section."),
            true,
            false,
        ),
        LocalServerStatus::Checking => (
            "Checking",
            "settings-local-server-state-working",
            guidance("Connecting to the server…"),
            true,
            true,
        ),
        LocalServerStatus::Connected(connection) => (
            "Connected",
            "settings-local-server-state-connected",
            (
                SharedString::from(format!(
                    "Connected to the server for your account. Protocol version {}.",
                    connection.protocol_major
                )),
                CaptionTone::Guidance,
            ),
            true,
            false,
        ),
        LocalServerStatus::Failed(error) => {
            let (state, state_selector, check) = failure(error);
            let explanation = match error {
                ServerError::NotSetUp => guidance(
                    "SpaceTerm sets up the server for your account when its window opens. Only \
                     programs running as your account can connect to it.",
                ),
                ServerError::NotRunning => guidance(
                    "The server is set up but not running. Start it below, or open a SpaceTerm \
                     window, to connect.",
                ),
                ServerError::AuthenticationRefused => (
                    SharedString::from(format!("{error} Nothing changed.")),
                    CaptionTone::Error,
                ),
                ServerError::IncompatibleState => (
                    SharedString::from(format!(
                        "{error} To start over, review and confirm a development state reset, then choose Check Now."
                    )),
                    CaptionTone::Error,
                ),
                // A deliberate Stop is a state the person chose, not a fault.
                ServerError::Stopped => guidance(STOPPED_GUIDANCE),
                ServerError::NotInstalled => {
                    guidance("Install the server's service below to start it.")
                }
                // Only the service's explicit Start can ask for this authorization.
                ServerError::AuthorizationRequired => guidance(
                    "Your computer needs an administrator password before the server can run at startup \
                     and after you log out. Choose Start Server for the background service below \
                     to continue.",
                ),
                _ => (SharedString::from(error.to_string()), CaptionTone::Error),
            };
            (state, state_selector, explanation, check, false)
        }
    };
    LocalServerPresentation {
        state,
        state_selector,
        explanation,
        tone,
        check,
        working,
    }
}

/// The badge, its selector, and whether checking again can tell anything new.
fn failure(error: ServerError) -> (&'static str, &'static str, bool) {
    match error {
        ServerError::NotSetUp => ("Not Set Up", "settings-local-server-state-not-set-up", true),
        ServerError::NotRunning => (
            "Not Running",
            "settings-local-server-state-not-running",
            true,
        ),
        ServerError::StartupFailed => (
            "Not Started",
            "settings-local-server-state-startup-failed",
            true,
        ),
        // Only the service's explicit Start or Install reverses these, so checking again says
        // nothing new.
        ServerError::Stopped => ("Stopped", "settings-local-server-state-stopped", false),
        ServerError::NotInstalled => (
            "Not Installed",
            "settings-local-server-state-not-installed",
            false,
        ),
        // Unreadable or unsafe storage and a refused or incompatible server can change outside
        // SpaceTerm, so the row offers another check.
        ServerError::StorageUnavailable | ServerError::ServiceUnavailable => (
            "Unavailable",
            "settings-local-server-state-unavailable",
            true,
        ),
        ServerError::UnsafeStorage => (
            "Unsafe Storage",
            "settings-local-server-state-unsafe-storage",
            true,
        ),
        ServerError::AuthenticationRefused => {
            ("Refused", "settings-local-server-state-refused", true)
        }
        ServerError::IncompatibleProtocol => (
            "Incompatible",
            "settings-local-server-state-incompatible",
            true,
        ),
        ServerError::IncompatibleState => (
            "Incompatible State",
            "settings-local-server-state-incompatible-state",
            true,
        ),
        ServerError::Protocol => (
            "Protocol Error",
            "settings-local-server-state-protocol-error",
            true,
        ),
        // Terminal Session refusals reach this row only through a check, which another check
        // answers afresh.
        ServerError::Controlled => ("In Use", "settings-local-server-state-controlled", true),
        ServerError::SessionUnavailable | ServerError::StaleControl => (
            "Unavailable",
            "settings-local-server-state-session-unavailable",
            true,
        ),
        ServerError::DirectoryUnavailable => (
            "Directory Unavailable",
            "settings-local-server-state-directory-unavailable",
            true,
        ),
        ServerError::Unsupported => (
            "Unsupported",
            "settings-local-server-state-unsupported",
            true,
        ),
        ServerError::RuntimeUnavailable => (
            "Runtime Unavailable",
            "settings-local-server-state-runtime-unavailable",
            true,
        ),
        ServerError::AuthorizationRequired => (
            "Authorization Required",
            "settings-local-server-state-authorization-required",
            false,
        ),
        ServerError::CleanupUnresolved => (
            "Cleanup Unresolved",
            "settings-local-server-state-cleanup-unresolved",
            true,
        ),
        // The server is holding shared edit outcomes for clients that have not acknowledged
        // them. Another check reports whether they have since.
        ServerError::Capacity => ("At Capacity", "settings-local-server-state-capacity", true),
        // Another surface changed the service after a confirmation was prepared. A check reads
        // its current state before the person asks again.
        ServerError::Changed => ("Changed", "settings-local-server-state-changed", true),
    }
}

/// The connection row's guidance once the person has stopped the server. A stopped server stays
/// stopped through account login and computer startup alike.
const STOPPED_GUIDANCE: &str = "You stopped the server. It stays stopped, even after you \
                                restart the computer, until you start it.";

pub(super) const SERVICE_CONTROL_SELECTOR: &str = "settings-server-service-control";
pub(super) const SERVICE_PROGRESS_SELECTOR: &str = "settings-server-service-progress";
pub(super) const SERVICE_CHECK_SELECTOR: &str = "settings-server-service-check";
pub(super) const INSTALL_SELECTOR: &str = "settings-server-service-install";
/// Prefixes each build in Install's menu, followed by its position.
pub(super) const INSTALL_CHOICE_SELECTOR: &str = "settings-server-service-install-choice";
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
pub(super) const AUTHORIZE_INSTALL_SELECTOR: &str = "settings-server-service-authorize-install";
pub(super) const AUTHORIZE_START_SELECTOR: &str = "settings-server-service-authorize-start";
pub(super) const AUTHORIZE_CONFIRM_SELECTOR: &str = "settings-server-authorize-confirm";
pub(super) const AUTHORIZE_CANCEL_SELECTOR: &str = "settings-server-authorize-cancel";

/// The step a person can take from the service row.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ServiceAction {
    /// Reads the service again.
    Check,
    /// Installs and starts the service from a verified server build the person names. With several
    /// builds, the step is a menu of them.
    Install,
    Start,
    Stop,
    Restart,
    Uninstall,
    /// Explains startup authorization, asks the operating system for it, then installs the build
    /// whose installation it refused.
    AuthorizeInstall,
    /// Explains startup authorization, asks the operating system for it, then starts the server.
    AuthorizeStart,
}

impl ServiceAction {
    /// The button label. Start and Stop use the names the person knows the actions by.
    pub(super) const fn label(self) -> &'static str {
        match self {
            Self::Check => "Check Now",
            Self::Install => "Install",
            Self::Start => "Start Server",
            Self::Stop => "Stop Server",
            Self::Restart => "Restart Server",
            Self::Uninstall => "Uninstall",
            Self::AuthorizeInstall => "Authorize and Install\u{2026}",
            Self::AuthorizeStart => "Authorize and Start\u{2026}",
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
            Self::AuthorizeInstall => AUTHORIZE_INSTALL_SELECTOR,
            Self::AuthorizeStart => AUTHORIZE_START_SELECTOR,
        }
    }

    /// The element identity of the slot the button occupies. Install, Start, Stop, Check, and the
    /// authorization steps share the leading slot, so keyboard focus stays on it when one replaces
    /// another.
    const fn slot(self) -> &'static str {
        match self {
            Self::Check
            | Self::Install
            | Self::Start
            | Self::Stop
            | Self::AuthorizeInstall
            | Self::AuthorizeStart => "settings-server-service-primary",
            Self::Restart => "settings-server-service-secondary",
            Self::Uninstall => "settings-server-service-removal",
        }
    }

    /// What the button does beyond its name, for assistive technology. Install names its build
    /// where the row renders it.
    const fn consequence(self) -> Option<&'static str> {
        match self {
            Self::Stop => Some("Ends the server's Terminal Sessions. SpaceTerm asks first."),
            Self::Restart => Some("Ends the server's Terminal Sessions. SpaceTerm asks first."),
            Self::Uninstall => Some("Removes the service. SpaceTerm asks first."),
            Self::AuthorizeInstall => Some(
                "SpaceTerm explains the change first. Your computer then asks for an administrator \
                 password, and SpaceTerm installs and starts the server.",
            ),
            Self::AuthorizeStart => Some(
                "SpaceTerm explains the change first. Your computer then asks for an administrator \
                 password, and SpaceTerm starts the server.",
            ),
            Self::Check | Self::Install | Self::Start => None,
        }
    }

    /// Whether the action changes the service. A check only reads it.
    const fn changes_service(self) -> bool {
        !matches!(self, Self::Check)
    }

    /// The progress badge and caption. Installing keeps a Stop the person chose, so it starts the
    /// server only when `stopped` is false.
    const fn progress(self, stopped: bool) -> (&'static str, &'static str) {
        match self {
            Self::Check => ("Checking", "Reading the service\u{2026}"),
            Self::Install if stopped => (
                "Installing",
                "Installing the server without starting it\u{2026}",
            ),
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
            Self::AuthorizeInstall | Self::AuthorizeStart => (
                "Authorizing",
                "Waiting for administrator authorization\u{2026}",
            ),
        }
    }
}

/// The Install or Start the service refused until the person authorizes startup at boot. The
/// authorization step retries only this request, and only after the person confirms it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum AuthorizedRequest {
    /// Installs and starts the named verified build.
    Install(PathBuf),
    /// Starts the installed server. A Restart refused after stopping retries as Start.
    Start,
}

impl AuthorizedRequest {
    /// The step it replaces and the authorization step that takes its place.
    const fn steps(&self) -> (ServiceAction, ServiceAction) {
        match self {
            Self::Install(_) => (ServiceAction::Install, ServiceAction::AuthorizeInstall),
            Self::Start => (ServiceAction::Start, ServiceAction::AuthorizeStart),
        }
    }
}

/// A request waiting for startup authorization.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct PendingAuthorization {
    pub(super) request: AuthorizedRequest,
    /// Whether the person already tried and the operating system's prompt was cancelled, refused,
    /// or timed out.
    pub(super) declined: bool,
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
    /// server that is not running ends no Terminal Session, and the message says so. The detail
    /// names the platform's startup, since a stopped or uninstalled server misses it.
    pub(super) const fn for_action(
        action: ServiceAction,
        running: bool,
        startup: ServiceStartup,
    ) -> Option<Self> {
        Some(match action {
            ServiceAction::Stop => Self {
                id: "settings-server-stop",
                title: "Stop Server",
                message: "Stop the server and end its Terminal Sessions?",
                detail: match startup {
                    ServiceStartup::AtLogin => {
                        "Programs running in them stop. Workspaces are kept. The server stays \
                         stopped, even after you log in again, until you choose Start Server."
                    }
                    ServiceStartup::AtBoot => {
                        "Programs running in them stop. Workspaces are kept. The server stays \
                         stopped, even after the computer restarts, until you choose Start Server."
                    }
                },
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
                detail: match startup {
                    ServiceStartup::AtLogin => {
                        "It no longer starts when you log in. Workspaces and \
                         verified server installations are kept, so you can install it again."
                    }
                    // Lingering is account policy other services may rely on, so it stays on.
                    ServiceStartup::AtBoot => {
                        "It no longer starts when the computer starts. Workspaces and \
                         verified server installations are kept, so you can install \
                         it again. Lingering stays on for your account, because other services \
                         may rely on it. To turn it off, run loginctl disable-linger."
                    }
                },
                confirm: "Uninstall",
                confirm_selector: UNINSTALL_CONFIRM_SELECTOR,
                cancel_selector: UNINSTALL_CANCEL_SELECTOR,
            },
            ServiceAction::Check
            | ServiceAction::Install
            | ServiceAction::Start
            | ServiceAction::AuthorizeInstall
            | ServiceAction::AuthorizeStart => return None,
        })
    }
}

/// The alert that explains startup authorization before the operating system asks for an
/// administrator password. Cancel takes initial focus, and cancelling here or in the operating
/// system's prompt changes nothing.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct AuthorizationPrompt {
    pub(super) title: &'static str,
    pub(super) message: &'static str,
    pub(super) detail: SharedString,
    pub(super) confirm: &'static str,
}

impl AuthorizationPrompt {
    /// The prompt for one waiting request. `build` names the build Install installs.
    pub(super) fn for_request(request: &AuthorizedRequest, build: Option<&str>) -> Self {
        let (then, confirm) = match (request, build) {
            (AuthorizedRequest::Install(_), Some(build)) => (
                format!("SpaceTerm then installs {build} and starts the server."),
                "Authorize and Install",
            ),
            (AuthorizedRequest::Install(_), None) => (
                "SpaceTerm then installs and starts the server.".to_owned(),
                "Authorize and Install",
            ),
            (AuthorizedRequest::Start, _) => (
                "SpaceTerm then starts the server.".to_owned(),
                "Authorize and Start",
            ),
        };
        Self {
            title: "Run the Server at Startup",
            message: "Let your account\u{2019}s services start at boot and keep running after you \
                      log out?",
            detail: SharedString::from(format!(
                "Your computer asks for an administrator password to turn on lingering for your \
                 account. Lingering applies to every service your account runs, and \
                 uninstalling the server leaves it on. {then} If you cancel here or in the \
                 password prompt, nothing changes."
            )),
            confirm,
        }
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
    /// The builds Install offers while the service is not installed. Install is a button for one
    /// build and a menu for several.
    pub(super) install_choices: Vec<InstallChoice>,
    /// The action awaiting its result. The row shows progress and withholds its steps meanwhile.
    pub(super) working: Option<ServiceAction>,
    /// The refused request the row's authorization step retries, while the service still offers
    /// it.
    pub(super) authorization: Option<PendingAuthorization>,
}

/// A verified server build Install can install, named by its version and source commit.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct InstallChoice {
    pub(super) label: SharedString,
    directory: PathBuf,
}

impl InstallChoice {
    /// Names each build by version and abbreviated commit. Two builds of one commit and version
    /// also name their package, so no two choices read alike.
    pub(super) fn of(installations: &[RetainedInstallation]) -> Vec<Self> {
        let named = |installation: &RetainedInstallation| {
            let commit = installation.commit.get(..7).unwrap_or(&installation.commit);
            format!("Version {} (commit {commit}", installation.version)
        };
        installations
            .iter()
            .map(|installation| {
                let name = named(installation);
                let ambiguous = installations
                    .iter()
                    .filter(|other| named(other) == name)
                    .count()
                    > 1;
                let package = installation
                    .directory
                    .file_name()
                    .and_then(|digest| digest.to_str())
                    .and_then(|digest| digest.get(..8));
                let label = match package {
                    Some(package) if ambiguous => format!("{name}, package {package})"),
                    _ => format!("{name})"),
                };
                Self {
                    label: label.into(),
                    directory: installation.directory.clone(),
                }
            })
            .collect()
    }
}

/// What the service row last learned and what it is doing.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(super) struct ServiceState {
    /// When the platform's service starts the server.
    pub(super) startup: ServiceStartup,
    /// The last status read. `None` until the first read succeeds, or after a read failed.
    pub(super) status: Option<ServiceStatus>,
    /// Why the last read or operation failed, until the next action.
    pub(super) failure: Option<ServerError>,
    /// The verified builds found with the last status read that showed no installed service.
    pub(super) installations: Vec<RetainedInstallation>,
    pub(super) working: Option<ServiceAction>,
    /// The Install or Start refused for want of startup authorization, until the next action.
    pub(super) authorization: Option<PendingAuthorization>,
}

/// The presentation of one service state. A failed operation keeps the badge and steps of the
/// status read after it, so the row says what is true now and why the operation did not finish.
/// A request refused for want of startup authorization instead offers the step that authorizes
/// and retries it.
pub(super) fn present_service(state: ServiceState) -> ServicePresentation {
    let guidance = |text: &'static str| (SharedString::new_static(text), CaptionTone::Guidance);
    let phase = state.status.map(ServicePhase::of);
    // A Stop the person chose outlasts Uninstall, and installing again keeps it.
    let stopped = state.status.is_some_and(|status| status.stopped);
    let install_choices = if phase == Some(ServicePhase::NotInstalled) {
        InstallChoice::of(&state.installations)
    } else {
        Vec::new()
    };
    // The authorization applies only while the service still offers the step it replaces, and
    // Install only while its build is still listed.
    let authorization = state.authorization.filter(|pending| {
        let (step, _) = pending.request.steps();
        let listed = match &pending.request {
            AuthorizedRequest::Install(directory) => install_choices
                .iter()
                .any(|choice| &choice.directory == directory),
            AuthorizedRequest::Start => true,
        };
        listed && phase.is_some_and(|phase| phase.actions().contains(&step))
    });
    let at_boot = state.startup == ServiceStartup::AtBoot;
    let (badge, state_selector, (explanation, tone)) = match (phase, state.failure, &authorization)
    {
        (None, None, _) => (
            "Not Checked",
            "settings-server-service-state-not-checked",
            guidance("SpaceTerm reads the service when you open this section."),
        ),
        (None, Some(error), _) => (
            "Unknown",
            "settings-server-service-state-unknown",
            (SharedString::from(error.to_string()), CaptionTone::Error),
        ),
        (Some(phase), _, Some(pending)) => (
            "Authorization Required",
            "settings-server-service-state-authorization-required",
            authorization_caption(pending, phase),
        ),
        (Some(phase), failure, None) => {
            let (badge, selector, caption) = match phase {
                ServicePhase::NotInstalled => (
                    "Not Installed",
                    "settings-server-service-state-not-installed",
                    match install_choices.as_slice() {
                        [] => guidance(
                            "No verified server package is available. Use the server installer \
                             command to keep a signed package for this account, then choose \
                             Check Now.",
                        ),
                        // Installing a stopped server starts nothing, so it needs no
                        // authorization on either platform.
                        [only] if stopped => (
                            SharedString::from(format!(
                                "{} is verified and ready to install. You stopped the server. \
                                 Installing keeps it stopped until you start it. No \
                                 administrator password is needed.",
                                only.label
                            )),
                            CaptionTone::Guidance,
                        ),
                        [only] if at_boot => (
                            SharedString::from(format!(
                                "{} is verified and ready to install. The server then starts for \
                                 your account when the computer starts and keeps running after \
                                 you log out or quit SpaceTerm. If your computer needs an administrator \
                                 password to allow that, SpaceTerm asks you first.",
                                only.label
                            )),
                            CaptionTone::Guidance,
                        ),
                        [only] => (
                            SharedString::from(format!(
                                "{} is verified and ready to install. The server then runs for \
                                 your account whenever you\u{2019}re logged in, even after \
                                 SpaceTerm quits. No administrator password is needed.",
                                only.label
                            )),
                            CaptionTone::Guidance,
                        ),
                        _ if stopped => guidance(
                            "Choose a verified server version from Install. You stopped the \
                             server. Installing keeps it stopped until you start it. No \
                             administrator password is needed.",
                        ),
                        _ if at_boot => guidance(
                            "Choose a verified server version from Install. It starts for your \
                             account when the computer starts and keeps running after you log \
                             out or quit SpaceTerm. If your computer needs an administrator password to \
                             allow that, SpaceTerm asks you first.",
                        ),
                        _ => guidance(
                            "Choose a verified server version from Install. It runs for your \
                             account whenever you\u{2019}re logged in, even after SpaceTerm \
                             quits. No administrator password is needed.",
                        ),
                    },
                ),
                ServicePhase::Running => (
                    "Running",
                    "settings-server-service-state-running",
                    // The status does not report lingering, which can be turned off outside
                    // SpaceTerm, so only surviving SpaceTerm's quit is promised outright.
                    guidance(if at_boot {
                        "The server keeps running after SpaceTerm quits. While lingering stays \
                         on for your account, it also starts when the computer starts and keeps \
                         running after you log out."
                    } else {
                        "The server starts when you log in and keeps running after SpaceTerm \
                         quits."
                    }),
                ),
                ServicePhase::Stopped => (
                    "Stopped",
                    "settings-server-service-state-stopped",
                    guidance(if at_boot {
                        "You stopped the server. It stays stopped, even after the computer \
                         restarts or you open SpaceTerm, until you start it."
                    } else {
                        "You stopped the server. It stays stopped, even after you log in again \
                         or open SpaceTerm, until you start it."
                    }),
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
    // The authorization step takes the place of the Install or Start it retries.
    let actions = match &authorization {
        Some(pending) => {
            let (step, authorize) = pending.request.steps();
            actions
                .into_iter()
                .map(|action| if action == step { authorize } else { action })
                .collect()
        }
        None => actions,
    };
    // Without a verified build, Install has nothing to install. Another check finds a build the
    // person has since kept.
    let actions = if phase == Some(ServicePhase::NotInstalled) && install_choices.is_empty() {
        vec![ServiceAction::Check]
    } else {
        actions
    };
    let (label, explanation, tone) = match state.working {
        Some(action) => {
            let (label, explanation) = action.progress(stopped);
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
        install_choices,
        working: state.working,
        authorization,
    }
}

/// Why the row asks for startup authorization, what is true meanwhile, and the step that
/// continues.
fn authorization_caption(
    pending: &PendingAuthorization,
    phase: ServicePhase,
) -> (SharedString, CaptionTone) {
    let reason = if pending.declined {
        "Authorization did not complete, so nothing changed."
    } else {
        "Your computer needs an administrator password before the server can start at boot and keep \
         running after you log out."
    };
    let now = match phase {
        ServicePhase::NotInstalled => "The server is not installed.",
        ServicePhase::Stopped => "The server is stopped.",
        ServicePhase::NotRunning | ServicePhase::Running => "The server is not running.",
    };
    let (_, step) = pending.request.steps();
    let step = step.label().trim_end_matches('\u{2026}');
    let next = if pending.declined {
        "to try again"
    } else {
        "to continue"
    };
    (
        SharedString::from(format!("{reason} {now} Choose {step} {next}.")),
        CaptionTone::Warning,
    )
}

/// Owns the offered service, its last state, and the pending operation.
pub(super) struct ServerServiceRow {
    control: Result<Arc<dyn ServerServiceControl>, ServerError>,
    state: ServiceState,
    /// Returns the background result to GPUI. Dropping the window drops it.
    _operation: Option<Task<()>>,
}

impl ServerServiceRow {
    pub(super) fn new(control: Result<Arc<dyn ServerServiceControl>, ServerError>) -> Self {
        let startup = control
            .as_ref()
            .map_or_else(|_| ServiceStartup::default(), |control| control.startup());
        Self {
            control,
            state: ServiceState {
                startup,
                ..ServiceState::default()
            },
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
            Ok(_) => present_service(self.state.clone()),
        }
    }

    /// Whether an operation that changes the service is running.
    fn changing(&self) -> bool {
        self.state
            .working
            .is_some_and(ServiceAction::changes_service)
    }
}

impl SettingsWindow {
    /// Checks the connection on the background executor. One check runs at a time, and none
    /// while the service is changing: its result checks the connection afresh.
    pub(super) fn check_local_server(&mut self, cx: &mut Context<Self>) {
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
        if matches!(row.status, LocalServerStatus::Checking) {
            return;
        }
        let Ok(control) = &row.control else {
            return;
        };
        let control = Arc::clone(control);
        row.status = LocalServerStatus::Checking;
        let result = cx.background_spawn(async move { control.refresh() });
        row._operation = Some(cx.spawn(async move |settings, cx| {
            let result = result.await;
            let _ = settings.update(cx, |settings, cx| {
                if let Some(row) = &mut settings.local_server {
                    row.status = match result {
                        Ok(connection) => LocalServerStatus::Connected(connection),
                        Err(error) => LocalServerStatus::Failed(error),
                    };
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
                && row.state.working.is_none()
                && !(action.changes_service() && connection_working)
        })
    }

    /// Whether the connection row has an action running.
    fn local_server_working(&self) -> bool {
        self.local_server
            .as_ref()
            .is_some_and(|row| matches!(row.status, LocalServerStatus::Checking))
    }

    /// Reads the service as the section comes into view. A running action keeps its place.
    pub(super) fn check_server_service(&mut self, cx: &mut Context<Self>) {
        self.run_server_service(ServiceAction::Check, cx);
    }

    /// Runs one action from the service row. Install installs the one verified build the row
    /// names; several builds are chosen from Install's menu instead. Stop, Restart, Uninstall, and
    /// the authorization steps ask for confirmation first, so they need a window.
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
            ServiceAction::Install => {
                let choices = self
                    .server_service_presentation()
                    .map(|presentation| presentation.install_choices)
                    .unwrap_or_default();
                if let [only] = choices.as_slice() {
                    self.install_server_service(only.directory.clone(), cx);
                }
            }
            ServiceAction::Stop | ServiceAction::Restart | ServiceAction::Uninstall => {
                self.confirm_server_service(action, window, cx);
            }
            ServiceAction::AuthorizeInstall | ServiceAction::AuthorizeStart => {
                self.confirm_startup_authorization(action, window, cx);
            }
        }
    }

    /// Runs an action that needs no installation. Stop, Restart, and Uninstall run only after the
    /// person confirms.
    pub(super) fn run_server_service(&mut self, action: ServiceAction, cx: &mut Context<Self>) {
        debug_assert!(!matches!(
            action,
            ServiceAction::Install
                | ServiceAction::AuthorizeInstall
                | ServiceAction::AuthorizeStart
        ));
        self.run_service_operation(action, None, cx);
    }

    /// Asks the operating system for startup authorization, then retries the request it refused.
    /// Runs only after the person confirms the explanation, and only for the request the row
    /// still offers to authorize.
    pub(super) fn authorize_server_startup(
        &mut self,
        action: ServiceAction,
        cx: &mut Context<Self>,
    ) {
        let Some(pending) = self
            .server_service_presentation()
            .and_then(|presentation| presentation.authorization)
        else {
            return;
        };
        let (_, step) = pending.request.steps();
        if step != action {
            return;
        }
        let installation = match pending.request {
            AuthorizedRequest::Install(installation) => Some(installation),
            AuthorizedRequest::Start => None,
        };
        self.run_service_operation(action, installation, cx);
    }

    /// Installs the build the person chose. Only a build the last read verified and listed is
    /// installed, so a choice from a menu the row has since replaced does nothing.
    pub(super) fn install_server_service(&mut self, installation: PathBuf, cx: &mut Context<Self>) {
        let listed = self.server_service.as_ref().is_some_and(|row| {
            row.state
                .installations
                .iter()
                .any(|listed| listed.directory == installation)
        });
        if listed {
            self.run_service_operation(ServiceAction::Install, Some(installation), cx);
        }
    }

    /// Runs one service operation on the background executor, then reads the service again when
    /// the operation failed, so the row presents the status that is true afterwards. While no
    /// service is installed, it also lists the verified builds Install can offer. Only one runs at
    /// a time, and a change waits for the connection row to finish.
    ///
    /// An Install, Start, or Restart refused for want of startup authorization waits for the
    /// person's authorization step. That step asks the operating system, and only when it
    /// succeeds does it retry the refused request. Nothing else asks for authorization.
    fn run_service_operation(
        &mut self,
        action: ServiceAction,
        installation: Option<PathBuf>,
        cx: &mut Context<Self>,
    ) {
        use ServiceAction as Action;

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
        let startup = row.state.startup;
        row.state.working = Some(action);
        row.state.failure = None;
        let authorizing = matches!(action, Action::AuthorizeInstall | Action::AuthorizeStart);
        let retry = installation.clone();
        let result = cx.background_spawn(async move {
            let outcome = match (action, installation.as_deref()) {
                (Action::Install, Some(installation)) => control.install(installation),
                (Action::Install, None) => Err(ServerError::RuntimeUnavailable),
                (Action::Check, _) => control.status(),
                (Action::Start, _) => control.start(),
                (Action::Stop, _) => control.stop(),
                (Action::Restart, _) => control.restart(),
                (Action::Uninstall, _) => control.uninstall(),
                (Action::AuthorizeInstall | Action::AuthorizeStart, _) => {
                    control.authorize_startup().and_then(|()| control.status())
                }
            };
            let authorized = authorizing && outcome.is_ok();
            let (status, failure) = match outcome {
                Ok(status) => (Some(status), None),
                Err(error) if action == Action::Check => (None, Some(error)),
                Err(error) => (control.status().ok(), Some(error)),
            };
            // Listing verifies each build and never installs or starts one. An installed service
            // offers no Install, so it lists nothing.
            let (installations, failure) = match status {
                Some(status) if !status.installed => match control.retained_installations() {
                    Ok(installations) => (installations, failure),
                    Err(error) => (Vec::new(), failure.or(Some(error))),
                },
                _ => (Vec::new(), failure),
            };
            // A request that would start the server waits for authorization. A Restart refused
            // after stopping continues as Start, which is what the person asked for.
            let request = match (failure, action) {
                (
                    Some(ServerError::AuthorizationRequired),
                    Action::Install | Action::AuthorizeInstall,
                ) => installation.map(AuthorizedRequest::Install),
                (
                    Some(ServerError::AuthorizationRequired),
                    Action::Start | Action::Restart | Action::AuthorizeStart,
                ) => Some(AuthorizedRequest::Start),
                _ => None,
            };
            let state = ServiceState {
                startup,
                status,
                failure,
                installations,
                working: None,
                authorization: request.map(|request| PendingAuthorization {
                    request,
                    declined: authorizing,
                }),
            };
            (state, authorized)
        });
        row._operation = Some(cx.spawn(async move |settings, cx| {
            let (state, authorized) = result.await;
            let _ = settings.update(cx, |settings, cx| {
                if let Some(row) = &mut settings.server_service {
                    row.state = state;
                }
                // The authorization the person confirmed continues with the request it was asked
                // for. Install still installs only a build the last read listed.
                if authorized {
                    match retry {
                        Some(installation) => settings.install_server_service(installation, cx),
                        None => settings.run_server_service(Action::Start, cx),
                    }
                }
                // A change starts, stops, or replaces the server the connection row authenticates
                // with, so the connection is checked afresh. A retry checks it when it finishes.
                if action.changes_service() {
                    settings.check_local_server(cx);
                }
                // A Workspace Window waiting on a stopped or missing server opens its Workspaces
                // once the person starts it here.
                if action.changes_service()
                    && settings
                        .server_service
                        .as_ref()
                        .and_then(|row| row.state.status)
                        .is_some_and(|status| status.running && !status.stopped)
                {
                    crate::ui::workspace_manager::resume_waiting_launches(cx);
                }
                cx.notify();
            });
        }));
        cx.notify();
    }

    /// Explains what startup authorization changes before the operating system asks for an
    /// administrator password. Cancel takes initial focus and Return does nothing, so only an
    /// explicit choice continues. Cancelling leaves everything as it is.
    fn confirm_startup_authorization(
        &mut self,
        action: ServiceAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(presentation) = self.server_service_presentation() else {
            return;
        };
        let Some(pending) = presentation.authorization else {
            return;
        };
        let build = match &pending.request {
            AuthorizedRequest::Install(directory) => presentation
                .install_choices
                .iter()
                .find(|choice| &choice.directory == directory)
                .map(|choice| choice.label.clone()),
            AuthorizedRequest::Start => None,
        };
        let prompt = AuthorizationPrompt::for_request(&pending.request, build.as_deref());
        let owner = cx.weak_entity();
        let result = Alert::new(
            ModalId::new("settings-server-authorize"),
            prompt.title,
            prompt.title,
            prompt.message,
            vec![
                ModalAction::new(
                    true,
                    prompt.confirm,
                    ModalActionRole::Affirmative,
                    AUTHORIZE_CONFIRM_SELECTOR,
                )
                .with_emphasis(ModalActionEmphasis::Prominent),
                ModalAction::new(
                    false,
                    "Cancel",
                    ModalActionRole::Cancel,
                    AUTHORIZE_CANCEL_SELECTOR,
                ),
            ],
        )
        .intent(AlertIntent::Warning)
        .detail(prompt.detail)
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
            let _ = owner.update(cx, |settings, cx| {
                settings.authorize_server_startup(action, cx);
            });
        });
        if result.is_err() {
            eprintln!("failed to present the SpaceTerm server startup authorization");
        }
    }

    /// Asks before an action that ends the server's Terminal Sessions or removes the service.
    /// Cancel is the default, so Return and Escape both leave the server as it is.
    fn confirm_server_service(
        &mut self,
        action: ServiceAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(state) = self.server_service.as_ref().map(|row| &row.state) else {
            return;
        };
        let running = state
            .status
            .is_some_and(|status| ServicePhase::of(status) == ServicePhase::Running);
        let Some(confirmation) = ServiceConfirmation::for_action(action, running, state.startup)
        else {
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
        let install_choices = presentation.install_choices;
        // The authorization step names the build it installs, as Install does.
        let authorized_build = match presentation.authorization.map(|pending| pending.request) {
            Some(AuthorizedRequest::Install(directory)) => install_choices
                .iter()
                .find(|choice| choice.directory == directory)
                .map(|choice| choice.label.clone()),
            _ => None,
        };
        let actions = presentation.actions.iter().map(|&action| {
            let owner = owner.clone();
            // Several verified builds make Install a menu of them, so the person names the build
            // before anything is installed.
            if action == ServiceAction::Install && install_choices.len() > 1 {
                let entries = install_choices
                    .iter()
                    .enumerate()
                    .map(|(index, choice)| {
                        MenuEntry::action(choice.label.clone(), choice.directory.clone())
                            .debug_selector(format!("{INSTALL_CHOICE_SELECTOR}-{index}"))
                    })
                    .collect();
                return Menu::new(ElementId::from(action.slot()), action.label(), entries)
                    .debug_selector(action.selector())
                    .on_activate(move |activation, _, cx| {
                        let installation = activation.action().clone();
                        let _ = owner.update(cx, |settings, cx| {
                            settings.install_server_service(installation, cx);
                        });
                    })
                    .into_any_element();
            }
            let consequence = match (action, install_choices.as_slice(), &authorized_build) {
                (ServiceAction::Install, [only], _) => {
                    Some(SharedString::from(format!("Installs {}.", only.label)))
                }
                (ServiceAction::AuthorizeInstall, _, Some(build)) => {
                    Some(SharedString::from(format!(
                        "SpaceTerm explains the change first. Your computer then asks for an \
                         administrator password, and SpaceTerm installs {build} and starts the \
                         server."
                    )))
                }
                _ => action.consequence().map(SharedString::new_static),
            };
            Button::new(ElementId::from(action.slot()), action.label())
                .variant(spaceterm_ui::ButtonVariant::Outline)
                .size(spaceterm_ui::ButtonSize::Regular)
                .tab_stop(true)
                .debug_selector(action.selector())
                .when_some(consequence, |button, consequence| {
                    button.accessibility_description(consequence)
                })
                .on_activate(move |_, window, cx| {
                    let _ = owner.update(cx, |settings, cx| {
                        settings.request_server_service(action, window, cx);
                    });
                })
                .into_any_element()
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
        // A service change checks the connection when it finishes, so the check waits for it.
        let service_changing = self
            .server_service
            .as_ref()
            .is_some_and(ServerServiceRow::changing);
        let owner = cx.weak_entity();
        let check = presentation.check.then(|| {
            action_button(
                CHECK_SELECTOR,
                "Check Now",
                !presentation.working && !service_changing,
                move |_, cx| {
                    let _ = owner.update(cx, |settings, cx| settings.check_local_server(cx));
                },
            )
        });
        div()
            .debug_selector(|| CONTROL_SELECTOR.to_owned())
            .flex()
            .flex_row()
            .items_center()
            .gap(appearance.spacing(8.0))
            .child(state)
            .children(check)
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

    use super::{LocalServerControl, ServerConnection, ServerServiceControl, ServiceStartup};
    use crate::local_server::ServerError;
    use crate::local_server::service::{RetainedInstallation, ServiceStatus};

    /// A server whose answers the test sets and whose operations it counts.
    pub(in crate::ui::settings_window) struct ScriptedServer {
        refresh: Mutex<Result<ServerConnection, ServerError>>,
        refreshes: AtomicUsize,
    }

    impl ScriptedServer {
        pub(in crate::ui::settings_window) fn new(
            refresh: Result<ServerConnection, ServerError>,
        ) -> Arc<Self> {
            Arc::new(Self {
                refresh: Mutex::new(refresh),
                refreshes: AtomicUsize::new(0),
            })
        }

        pub(in crate::ui::settings_window) fn answer_refresh(
            &self,
            result: Result<ServerConnection, ServerError>,
        ) {
            *self.refresh.lock().expect("refresh answer") = result;
        }

        pub(in crate::ui::settings_window) fn refreshes(&self) -> usize {
            self.refreshes.load(Ordering::SeqCst)
        }
    }

    impl LocalServerControl for ScriptedServer {
        fn refresh(&self) -> Result<ServerConnection, ServerError> {
            self.refreshes.fetch_add(1, Ordering::SeqCst);
            *self.refresh.lock().expect("refresh answer")
        }
    }

    /// One operation a [`ScriptedService`] received.
    #[derive(Clone, Debug, Eq, PartialEq)]
    pub(in crate::ui::settings_window) enum ServiceCall {
        Status,
        RetainedInstallations,
        Install(PathBuf),
        Start,
        Stop,
        Restart,
        Uninstall,
        AuthorizeStartup,
    }

    /// What the operating system's authorization prompt does when the service asks.
    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    pub(in crate::ui::settings_window) enum Prompt {
        /// The person enters an administrator password, and the account's lingering turns on.
        Approve,
        /// The person dismisses the prompt, and nothing changes.
        Cancel,
    }

    /// A service that changes its status as a real one would, refuses what the test tells it to,
    /// and records every operation.
    pub(in crate::ui::settings_window) struct ScriptedService {
        status: Mutex<Result<ServiceStatus, ServerError>>,
        /// The verified builds the service lists, or why it cannot.
        installations: Mutex<Result<Vec<RetainedInstallation>, ServerError>>,
        /// The refusal the next changing operation returns, leaving the status unchanged.
        refusal: Mutex<Option<ServerError>>,
        startup: ServiceStartup,
        /// Whether the account may run services at boot and after logout. An account service refuses
        /// to start the server until it may.
        authorized: Mutex<bool>,
        prompt: Mutex<Prompt>,
        calls: Mutex<Vec<ServiceCall>>,
    }

    impl ScriptedService {
        pub(in crate::ui::settings_window) fn new(
            status: Result<ServiceStatus, ServerError>,
        ) -> Arc<Self> {
            Self::with_startup(status, ServiceStartup::AtLogin)
        }

        /// An account service on an account whose lingering is off, so starting the
        /// server waits for startup authorization.
        pub(in crate::ui::settings_window) fn with_boot_startup(
            status: Result<ServiceStatus, ServerError>,
        ) -> Arc<Self> {
            Self::with_startup(status, ServiceStartup::AtBoot)
        }

        fn with_startup(
            status: Result<ServiceStatus, ServerError>,
            startup: ServiceStartup,
        ) -> Arc<Self> {
            Arc::new(Self {
                status: Mutex::new(status),
                installations: Mutex::new(Ok(vec![retained("1.4.0", 'a')])),
                refusal: Mutex::new(None),
                startup,
                // A LaunchAgent needs no authorization to start.
                authorized: Mutex::new(startup == ServiceStartup::AtLogin),
                prompt: Mutex::new(Prompt::Approve),
                calls: Mutex::new(Vec::new()),
            })
        }

        /// Sets what the next authorization prompt does.
        pub(in crate::ui::settings_window) fn answer_prompt(&self, prompt: Prompt) {
            *self.prompt.lock().expect("prompt") = prompt;
        }

        pub(in crate::ui::settings_window) fn keep(
            &self,
            installations: Result<Vec<RetainedInstallation>, ServerError>,
        ) {
            *self.installations.lock().expect("installations") = installations;
        }

        pub(in crate::ui::settings_window) fn refuse_next(&self, error: ServerError) {
            *self.refusal.lock().expect("refusal") = Some(error);
        }

        pub(in crate::ui::settings_window) fn calls(&self) -> Vec<ServiceCall> {
            self.calls.lock().expect("calls").clone()
        }

        /// The operations that changed or tried to change the service, without reads.
        pub(in crate::ui::settings_window) fn changes(&self) -> Vec<ServiceCall> {
            self.calls()
                .into_iter()
                .filter(|call| {
                    !matches!(
                        call,
                        ServiceCall::Status | ServiceCall::RetainedInstallations
                    )
                })
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
            // Starting needs authorization first, and a refused start changes nothing.
            if changed.running && !*self.authorized.lock().expect("authorized") {
                return Err(ServerError::AuthorizationRequired);
            }
            *status = Ok(changed);
            Ok(changed)
        }
    }

    impl ServerServiceControl for ScriptedService {
        fn status(&self) -> Result<ServiceStatus, ServerError> {
            self.calls.lock().expect("calls").push(ServiceCall::Status);
            *self.status.lock().expect("status")
        }

        fn retained_installations(&self) -> Result<Vec<RetainedInstallation>, ServerError> {
            self.calls
                .lock()
                .expect("calls")
                .push(ServiceCall::RetainedInstallations);
            self.installations.lock().expect("installations").clone()
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
            let restarted = self.change(ServiceCall::Restart, |_| running());
            // Restarting stops the server before it starts again, so a refused start leaves it
            // stopped, as the service does.
            if restarted == Err(ServerError::AuthorizationRequired) {
                *self.status.lock().expect("status") = Ok(stopped());
            }
            restarted
        }

        fn uninstall(&self) -> Result<ServiceStatus, ServerError> {
            // Uninstalling stops the server first and keeps that Stop, as the service does.
            self.change(ServiceCall::Uninstall, |_| uninstalled())
        }

        fn startup(&self) -> ServiceStartup {
            self.startup
        }

        fn authorize_startup(&self) -> Result<(), ServerError> {
            self.calls
                .lock()
                .expect("calls")
                .push(ServiceCall::AuthorizeStartup);
            match *self.prompt.lock().expect("prompt") {
                Prompt::Approve => {
                    *self.authorized.lock().expect("authorized") = true;
                    Ok(())
                }
                Prompt::Cancel => Err(ServerError::AuthorizationRequired),
            }
        }
    }

    /// A verified build kept for this account, whose 40-digit commit repeats `digit`.
    pub(in crate::ui::settings_window) fn retained(
        version: &str,
        digit: char,
    ) -> RetainedInstallation {
        let digest: String = std::iter::repeat_n(digit, 64).collect();
        RetainedInstallation {
            directory: PathBuf::from("/retained/server/installations").join(digest),
            version: version.to_owned(),
            commit: std::iter::repeat_n(digit, 40).collect(),
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

    /// Uninstalled, which records a Stop that installing again keeps.
    pub(in crate::ui::settings_window) const fn uninstalled() -> ServiceStatus {
        ServiceStatus {
            stopped: true,
            ..not_installed()
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

    /// An authenticated server speaking `protocol_major`.
    pub(in crate::ui::settings_window) const fn connected(protocol_major: u16) -> ServerConnection {
        ServerConnection { protocol_major }
    }
}

#[cfg(test)]
#[path = "servers_tests.rs"]
mod tests;
