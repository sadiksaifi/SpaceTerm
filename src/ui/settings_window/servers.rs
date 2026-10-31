//! Account-local server connection and service controls. Construction has no effects.
//! Presenting this section checks the existing server; explicit actions set up or start it.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use gpui::prelude::*;
use gpui::{Context, SharedString, Task, Window};
use spaceterm_ui::{
    Alert, AlertIntent, AlertOutcome, ModalAction, ModalActionEmphasis, ModalActionIntent,
    ModalActionRole, ModalId,
};

use super::SettingsWindow;
use super::server_update::ServerUpdateControl;
use crate::application_identity::ApplicationIdentity;
pub(super) use crate::server_connection::service::ServiceStartup;
use crate::server_connection::service::{
    LingerAuthorization, RetainedInstallation, Service, ServiceStatus, control,
};
use crate::server_connection::{AuthenticatedStatus, ServerConnection, ServerError};
use crate::ui::direct_access::DirectAccessControl;
use crate::ui::sidebar_window::form::CaptionTone;

/// The blocking local server check the section runs on the background executor. The Workspace
/// Window sets up and starts the server; this row only reports how it answers.
pub(super) trait LocalServerControl: Send + Sync {
    fn worktree_server(&self) -> Option<ServerConnection> {
        None
    }
    /// The server's direct network access, managed with this account's authority.
    fn direct_access(&self) -> Option<Arc<dyn DirectAccessControl>> {
        None
    }
    /// Connects using owning-account authority. Never creates an identity or starts a server.
    fn refresh(&self) -> Result<ConnectedServer, ServerError>;
    fn lifetime(&self) -> ServerLifetime {
        ServerLifetime::Independent
    }
}

/// How long the local server runs relative to the SpaceTerm presenting it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ServerLifetime {
    /// An installed service runs the server, which keeps running after SpaceTerm quits.
    Independent,
    /// The Development command owns a foreground server that ends, with its Terminal Sessions,
    /// when the run ends.
    DevelopmentRun,
}

/// The foreground server a source Development run owns. No service runs it and no update
/// replaces it, so it is offered without either.
pub(super) struct DevelopmentRunServer(pub(super) Arc<dyn LocalServerControl>);

impl LocalServerControl for DevelopmentRunServer {
    fn worktree_server(&self) -> Option<ServerConnection> {
        self.0.worktree_server()
    }
    fn direct_access(&self) -> Option<Arc<dyn DirectAccessControl>> {
        self.0.direct_access()
    }
    fn refresh(&self) -> Result<ConnectedServer, ServerError> {
        self.0.refresh()
    }
    fn lifetime(&self) -> ServerLifetime {
        ServerLifetime::DevelopmentRun
    }
}

/// The Servers section's description while a Development run owns the local server.
pub(super) const DEVELOPMENT_RUN_DESCRIPTION: &str = "Servers run your Workspaces and their \
                                                       Terminal Sessions. This Development \
                                                       run\u{2019}s server ends when the run ends.";

impl LocalServerControl for ServerConnection {
    fn worktree_server(&self) -> Option<ServerConnection> {
        Some(self.clone())
    }
    fn direct_access(&self) -> Option<Arc<dyn DirectAccessControl>> {
        Some(Arc::new(self.clone()))
    }
    fn refresh(&self) -> Result<ConnectedServer, ServerError> {
        ServerConnection::refresh(self).map(ConnectedServer::from)
    }
}

/// What the section presents about an authenticated connection.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct ConnectedServer {
    pub(super) protocol_major: u16,
}

impl From<AuthenticatedStatus> for ConnectedServer {
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
    /// Changes only whether the server starts on its own at login or boot. A deliberate Stop
    /// stays in force. Enabling it at boot may need [`Self::authorize_startup`] first.
    fn set_automatic_start(&self, enabled: bool) -> Result<ServiceStatus, ServerError>;
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

    fn set_automatic_start(&self, enabled: bool) -> Result<ServiceStatus, ServerError> {
        Service::set_automatic_start(self, enabled)
    }
}

/// The local server a Settings window presents, and the service that runs it where the platform
/// has one.
#[derive(Clone)]
pub(super) struct OfferedServer {
    pub(super) connection: Result<Arc<dyn LocalServerControl>, ServerError>,
    /// `None` omits the service row, which keeps the connection row's own Set Up and Start.
    pub(super) service: Option<Result<Arc<dyn ServerServiceControl>, ServerError>>,
    /// The server version and its update journey. `None` omits the version row.
    pub(super) update: Option<Arc<dyn ServerUpdateControl>>,
    /// This computer's command for the server's identity. `None` omits its row.
    pub(super) command_line_tool: Option<Arc<crate::command_line_tool::CommandLineTool>>,
}

/// The local server a Settings window presents. Every build offers one. `None` hides the Servers
/// section.
pub(super) type OfferedLocalServer = Option<OfferedServer>;

/// Resolves the running build's local server and its service. Resolution reads paths only: it
/// sets up, installs, starts, and connects to nothing.
pub(super) fn offered_local_server(
    command_line_tool: Option<Arc<crate::command_line_tool::CommandLineTool>>,
) -> OfferedLocalServer {
    let identity = ApplicationIdentity::current().directory_name();
    if crate::local_startup::owned_foreground() {
        return Some(OfferedServer {
            connection: ServerConnection::for_identity(identity).map(|server| {
                Arc::new(DevelopmentRunServer(Arc::new(server))) as Arc<dyn LocalServerControl>
            }),
            service: None,
            update: None,
            command_line_tool,
        });
    }
    let resolved = Service::for_identity(identity);
    // Only a resolved service can report or replace its approved build.
    let update = resolved
        .as_ref()
        .ok()
        .map(|service| Arc::new(service.clone()) as Arc<dyn ServerUpdateControl>);
    let service = match resolved {
        // A platform without a service layer keeps the connection row's own workflow.
        Err(ServerError::Unsupported) => None,
        resolved => {
            Some(resolved.map(|service| Arc::new(service) as Arc<dyn ServerServiceControl>))
        }
    };
    Some(OfferedServer {
        connection: ServerConnection::for_identity(identity)
            .map(|server| Arc::new(server) as Arc<dyn LocalServerControl>),
        service,
        update,
        command_line_tool,
    })
}

/// What SpaceTerm last learned about the local server.
#[derive(Clone, Copy, Debug)]
pub(super) enum LocalServerStatus {
    /// Nothing has been asked yet.
    NotChecked,
    /// A check awaits its result.
    Checking,
    Connected(ConnectedServer),
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
    /// The last result. A check keeps it in place until the check answers, so a refresh never
    /// makes a connected server read as disconnected.
    status: LocalServerStatus,
    /// A check awaits its result.
    checking: bool,
    /// Returns the background result to GPUI. Dropping the window drops it, and a late result then
    /// has nowhere to land.
    _operation: Option<Task<()>>,
}

impl LocalServerRow {
    pub(super) fn worktree_server(&self) -> Option<ServerConnection> {
        self.control.as_ref().ok()?.worktree_server()
    }
    pub(super) fn direct_access(&self) -> Option<Arc<dyn DirectAccessControl>> {
        self.control.as_ref().ok()?.direct_access()
    }
    pub(super) fn new(control: Result<Arc<dyn LocalServerControl>, ServerError>) -> Self {
        Self {
            control,
            status: LocalServerStatus::NotChecked,
            checking: false,
            _operation: None,
        }
    }

    pub(super) fn lifetime(&self) -> ServerLifetime {
        self.control
            .as_ref()
            .map_or(ServerLifetime::Independent, |control| control.lifetime())
    }

    /// What SpaceTerm last learned about the server. A server that could not be resolved reads as
    /// that failure.
    pub(super) fn current_status(&self) -> LocalServerStatus {
        match &self.control {
            Err(error) => LocalServerStatus::Failed(*error),
            Ok(_) => self.status,
        }
    }

    pub(super) fn presentation(&self) -> LocalServerPresentation {
        let status = match &self.control {
            Err(error) => LocalServerStatus::Failed(*error),
            Ok(_) => self.status,
        };
        let mut presentation = present(status);
        if let (ServerLifetime::DevelopmentRun, LocalServerStatus::Connected(connection)) =
            (self.lifetime(), status)
        {
            presentation.explanation = format!(
                "Connected to the server this Development run owns. It ends with its Terminal \
                 Sessions when the run ends. Protocol version {}.",
                connection.protocol_major
            )
            .into();
        }
        // A server that could not be resolved offers no step: checking again resolves nothing.
        if self.control.is_err() {
            presentation.check = false;
        }
        presentation.working |= self.checking;
        presentation
    }
}

/// The presentation of one status.
pub(super) fn present(status: LocalServerStatus) -> LocalServerPresentation {
    let guidance =
        |explanation: &'static str| (SharedString::new_static(explanation), CaptionTone::Guidance);
    let (state, state_selector, (explanation, tone), check, working) = match status {
        LocalServerStatus::NotChecked => (
            "Not checked",
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
                ServerError::AuthenticationRefused | ServerError::IncompatibleState => {
                    (SharedString::from(error.to_string()), CaptionTone::Error)
                }
                // The refusal is scoped to this connection. The server and its Terminal Sessions
                // keep running, and the Server row offers any verified update.
                ServerError::IncompatibleProtocol => (
                    SharedString::new_static(INCOMPATIBLE_PROTOCOL),
                    CaptionTone::Error,
                ),
                ServerError::MissingRequiredCapability => (
                    SharedString::new_static(MISSING_CAPABILITY),
                    CaptionTone::Error,
                ),
                ServerError::ActivationUnresolved => {
                    guidance("Choose Finish Update to complete the server update and reconnect.")
                }
                // Another service operation is still running. It is not a fault.
                ServerError::ServiceBusy => guidance(
                    "Another server operation is running. Choose Check Now when it finishes.",
                ),
                // A deliberate Stop is a state the person chose, not a fault.
                ServerError::Stopped => guidance(STOPPED_GUIDANCE),
                ServerError::NotInstalled => {
                    guidance("Install the server\u{2019}s service below to start it.")
                }
                // Only the service's explicit Start can ask for this authorization.
                ServerError::AuthorizationRequired => guidance(
                    "Choose Start Server. Your computer asks for an administrator password.",
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
        ServerError::NotSetUp => ("Not set up", "settings-local-server-state-not-set-up", true),
        ServerError::NotRunning => (
            "Not running",
            "settings-local-server-state-not-running",
            true,
        ),
        ServerError::StartupFailed => (
            "Not started",
            "settings-local-server-state-startup-failed",
            true,
        ),
        // Only the service's explicit Start or Install reverses these, so checking again says
        // nothing new.
        ServerError::Stopped => ("Stopped", "settings-local-server-state-stopped", false),
        ServerError::NotInstalled => (
            "Not installed",
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
        // Another service operation outlasted the admission wait. A check waits for it again.
        ServerError::ServiceBusy => ("Busy", "settings-local-server-state-busy", true),
        ServerError::UnsafeStorage => (
            "Unsafe storage",
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
        ServerError::MissingRequiredCapability => (
            "Incompatible",
            "settings-local-server-state-missing-capability",
            true,
        ),
        // Only a fresh SpaceTerm process uses its replaced installation's server package, so
        // checking again from this one says nothing new.
        ServerError::CandidateChanged => (
            "Restart SpaceTerm",
            "settings-local-server-state-candidate-changed",
            false,
        ),
        ServerError::IncompatibleState => (
            "Incompatible state",
            "settings-local-server-state-incompatible-state",
            true,
        ),
        ServerError::Protocol => (
            "Protocol error",
            "settings-local-server-state-protocol-error",
            true,
        ),
        // Terminal Session refusals reach this row only through a check, which another check
        // answers afresh.
        ServerError::Controlled => ("In use", "settings-local-server-state-controlled", true),
        ServerError::SessionUnavailable | ServerError::StaleControl => (
            "Unavailable",
            "settings-local-server-state-session-unavailable",
            true,
        ),
        ServerError::DirectoryUnavailable => (
            "Directory unavailable",
            "settings-local-server-state-directory-unavailable",
            true,
        ),
        ServerError::Unsupported => (
            "Unsupported",
            "settings-local-server-state-unsupported",
            true,
        ),
        ServerError::RuntimeUnavailable => (
            "Runtime unavailable",
            "settings-local-server-state-runtime-unavailable",
            true,
        ),
        // Only another SpaceTerm build includes a server package, so checking again from this
        // one says nothing new.
        ServerError::CandidateMissing => (
            "Not included",
            "settings-local-server-state-candidate-missing",
            false,
        ),
        ServerError::AuthorizationRequired => (
            "Authorization required",
            "settings-local-server-state-authorization-required",
            false,
        ),
        ServerError::CleanupUnresolved => (
            "Cleanup unresolved",
            "settings-local-server-state-cleanup-unresolved",
            true,
        ),
        // The server is holding shared edit outcomes for clients that have not acknowledged
        // them. Another check reports whether they have since.
        ServerError::Capacity => ("At capacity", "settings-local-server-state-capacity", true),
        // Another surface changed the service after a confirmation was prepared. A check reads
        // its current state before the person asks again.
        ServerError::Changed => ("Changed", "settings-local-server-state-changed", true),
        // A direct network refusal reaches this row only through a check, which another check
        // answers afresh.
        ServerError::Direct(_) => ("Refused", "settings-local-server-state-direct", true),
        // An update the person confirmed has not resolved, so the server admits no connection
        // until Finish Update resolves it. A check reports whether it has since.
        ServerError::ActivationUnresolved => (
            "Update unfinished",
            "settings-local-server-state-update-unfinished",
            true,
        ),
        ServerError::ReturnUnavailable => (
            "Update failed",
            "settings-local-server-state-return-unavailable",
            true,
        ),
    }
}

/// Why this client refused a server with no protocol major in common. Neither side's release
/// number says which one is older, so the wording names no side.
pub(crate) const INCOMPATIBLE_PROTOCOL: &str = "SpaceTerm and this server are incompatible.";

/// Why this client refused a server that lacks a capability it requires.
pub(crate) const MISSING_CAPABILITY: &str = "This server needs an update.";

/// The connection row's guidance once the person has stopped the server. A stopped server stays
/// stopped through account login and computer startup alike.
const STOPPED_GUIDANCE: &str = "You stopped the server. It stays stopped, even after you \
                                restart the computer, until you start it.";

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
pub(crate) enum ServiceAction {
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
    pub(crate) const fn label(self) -> &'static str {
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

    pub(super) const fn selector(self) -> &'static str {
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
    pub(super) const fn slot(self) -> &'static str {
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
    pub(super) const fn consequence(self) -> Option<&'static str> {
        match self {
            Self::Stop => Some("Ends the server\u{2019}s Terminal Sessions."),
            Self::Restart => Some("Ends the server\u{2019}s Terminal Sessions."),
            Self::Uninstall => Some("Removes the service."),
            Self::AuthorizeInstall => {
                Some("Requires an administrator password to install and start the server.")
            }
            Self::AuthorizeStart => Some("Requires an administrator password to start the server."),
            Self::Check | Self::Install | Self::Start => None,
        }
    }

    /// Whether the action changes the service. A check only reads it.
    const fn changes_service(self) -> bool {
        !matches!(self, Self::Check)
    }

    /// The progress badge and caption. Installing keeps a Stop the person chose, so it starts the
    /// server only when `stopped` is false.
    pub(crate) const fn progress(self, stopped: bool) -> (&'static str, &'static str) {
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
pub(crate) enum AuthorizedRequest {
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

/// The alert that asks before Stop, Restart, or Uninstall, in the wording every control surface
/// shows: it names this build's server and says every client's Terminal Sessions end.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct ServiceConfirmation {
    id: &'static str,
    pub(super) title: &'static str,
    pub(super) message: SharedString,
    pub(super) detail: SharedString,
    pub(super) confirm: &'static str,
    confirm_selector: &'static str,
    cancel_selector: &'static str,
}

impl ServiceConfirmation {
    /// The confirmation for one action, or `None` for an action that needs none. Uninstalling a
    /// server that is not running ends no Terminal Session, and the message says so. The detail
    /// names the platform's startup, since a stopped or uninstalled server misses it.
    pub(super) fn for_action(
        action: ServiceAction,
        running: bool,
        startup: ServiceStartup,
    ) -> Option<Self> {
        let (id, control, confirm_selector, cancel_selector) = match action {
            ServiceAction::Stop => (
                "settings-server-stop",
                control::Action::Stop,
                STOP_CONFIRM_SELECTOR,
                STOP_CANCEL_SELECTOR,
            ),
            ServiceAction::Restart => (
                "settings-server-restart",
                control::Action::Restart,
                RESTART_CONFIRM_SELECTOR,
                RESTART_CANCEL_SELECTOR,
            ),
            ServiceAction::Uninstall => (
                "settings-server-uninstall",
                control::Action::Uninstall,
                UNINSTALL_CONFIRM_SELECTOR,
                UNINSTALL_CANCEL_SELECTOR,
            ),
            ServiceAction::Check
            | ServiceAction::Install
            | ServiceAction::Start
            | ServiceAction::AuthorizeInstall
            | ServiceAction::AuthorizeStart => return None,
        };
        let wording = control::Wording::new(
            ApplicationIdentity::current().directory_name(),
            control,
            running,
            startup,
        );
        Some(Self {
            id,
            title: wording.title(),
            message: wording.message().into(),
            detail: wording.detail().into(),
            confirm: wording.confirm_label(),
            confirm_selector,
            cancel_selector,
        })
    }
}

/// The alert that explains startup authorization before the operating system asks for an
/// administrator password. Cancel takes initial focus, and cancelling here or in the operating
/// system's prompt changes nothing.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct AuthorizationPrompt {
    pub(crate) title: &'static str,
    pub(crate) message: &'static str,
    pub(crate) detail: &'static str,
    pub(crate) confirm: &'static str,
}

impl AuthorizationPrompt {
    /// The prompt for one waiting request. `build` names the build Install installs.
    pub(crate) fn for_request(request: &AuthorizedRequest, _build: Option<&str>) -> Self {
        let confirm = match request {
            AuthorizedRequest::Install(_) => "Authorize and Install",
            AuthorizedRequest::Start => "Authorize and Start",
        };
        Self {
            title: "Run the Server at Startup?",
            message: "Enter an administrator password so your account\u{2019}s services keep running after you log out.",
            detail: "This applies to every service your account runs and stays on if you uninstall the server.",
            confirm,
        }
    }
}

/// Where the service stands, read from its status.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ServicePhase {
    NotInstalled,
    Running,
    /// The person stopped it. It stays stopped until an explicit Start.
    Stopped,
    /// Installed and not stopped by the person, yet not running, such as after a crash.
    NotRunning,
}

impl ServicePhase {
    /// A running process wins over a recorded Stop, since the process is what ends Sessions.
    pub(crate) fn of(status: ServiceStatus) -> Self {
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
    pub(crate) fn actions(self) -> &'static [ServiceAction] {
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
    description_label: SharedString,
    directory: PathBuf,
}

impl InstallChoice {
    /// The verified build's directory, which Install installs.
    pub(super) fn directory(&self) -> &Path {
        &self.directory
    }

    /// Names each build by version and abbreviated commit. Two builds of one commit and version
    /// also name their package, so no two choices read alike.
    pub(super) fn of(installations: &[RetainedInstallation]) -> Vec<Self> {
        let named = |installation: &RetainedInstallation| {
            let commit = installation.commit.get(..7).unwrap_or(&installation.commit);
            format!("{} ({commit}", installation.version)
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
                    Some(package) if ambiguous => format!("{name}, {package})"),
                    _ => format!("{name})"),
                };
                let commit = installation.commit.get(..7).unwrap_or(&installation.commit);
                let description_label = match package {
                    Some(package) if ambiguous => format!(
                        "Version {} (commit {commit}, package {package})",
                        installation.version
                    ),
                    _ => format!("Version {} (commit {commit})", installation.version),
                };
                Self {
                    label: label.into(),
                    description_label: description_label.into(),
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
    /// The read failure, or the admitted failure retained until the next change.
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
            "Not checked",
            "settings-server-service-state-not-checked",
            guidance("SpaceTerm reads the service when you open this section."),
        ),
        // A Stop that published its intent but could not confirm its cleanup. Only another
        // explicit Stop retries it; nothing offered here starts the server again.
        (None, Some(ServerError::CleanupUnresolved), _) => (
            "Cleanup unresolved",
            "settings-server-service-state-cleanup-unresolved",
            (
                SharedString::new_static(
                    "Terminal Session end unconfirmed. Choose Stop Server to try again.",
                ),
                CaptionTone::Warning,
            ),
        ),
        (None, Some(error), _) => (
            "Unknown",
            "settings-server-service-state-unknown",
            (SharedString::from(error.to_string()), CaptionTone::Error),
        ),
        (Some(phase), _, Some(pending)) => (
            "Authorization required",
            "settings-server-service-state-authorization-required",
            authorization_caption(pending, phase),
        ),
        (Some(phase), failure, None) => {
            let (badge, selector, caption) = match phase {
                ServicePhase::NotInstalled => (
                    "Not installed",
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
                                only.description_label
                            )),
                            CaptionTone::Guidance,
                        ),
                        [only] if at_boot => (
                            SharedString::from(format!(
                                "{} is verified and ready to install. The server then starts for \
                                 your account when the computer starts and keeps running after \
                                 you log out or quit SpaceTerm. If your computer needs an administrator \
                                 password to allow that, SpaceTerm asks you first.",
                                only.description_label
                            )),
                            CaptionTone::Guidance,
                        ),
                        [only] => (
                            SharedString::from(format!(
                                "{} is verified and ready to install. The server then runs for \
                                 your account whenever you\u{2019}re logged in, even after \
                                 SpaceTerm quits. No administrator password is needed.",
                                only.description_label
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
                    "Not running",
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
    let actions = match (phase, state.failure) {
        (Some(phase), _) => phase.actions().to_vec(),
        (None, Some(ServerError::CleanupUnresolved)) => {
            vec![ServiceAction::Stop, ServiceAction::Check]
        }
        (None, _) => vec![ServiceAction::Check],
    };
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
    let (_, step) = pending.request.steps();
    let step = step.label().trim_end_matches('\u{2026}');
    let message = match (pending.declined, phase) {
        (true, ServicePhase::NotInstalled | ServicePhase::Stopped) => {
            format!("Authorization didn\u{2019}t finish. Choose {step} to try again.")
        }
        (true, ServicePhase::NotRunning | ServicePhase::Running) => {
            format!("Choose {step} to try again.")
        }
        (false, ServicePhase::NotInstalled | ServicePhase::Stopped) => {
            format!("Choose {step} to run the server at startup.")
        }
        (false, ServicePhase::NotRunning | ServicePhase::Running) => {
            format!("Choose {step} to continue.")
        }
    };
    (message.into(), CaptionTone::Warning)
}

/// A change to automatic start and why the last one didn't take effect.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(super) struct AutomaticStart {
    pub(super) working: bool,
    pub(super) failure: Option<ServerError>,
    /// What the change underway asks for, which the switch shows until the service answers.
    pub(super) requested: Option<bool>,
}

/// Presents the offered service and owns passive reads.
pub(super) struct ServerServiceRow {
    control: Result<Arc<dyn ServerServiceControl>, ServerError>,
    pub(super) state: ServiceState,
    pub(super) automatic_start: AutomaticStart,
    /// A change to automatic start the person asked for while a read or an earlier change ran,
    /// and the window that asked. It follows once the service is idle.
    queued_automatic_start: Option<(bool, gpui::AnyWindowHandle)>,
    /// Whether the service started the server on its own as of the last read that could tell,
    /// kept while a later read fails.
    last_confirmed_automatic_start: Option<bool>,
    /// Start Server was chosen while a read ran. It follows once the reads finish, if they still
    /// offer it.
    start_requested: bool,
    /// The service changed outside Settings while a read ran. That read may have observed the
    /// service before the change, so its answer is set aside and the service is read again.
    read_outdated: bool,
    /// The window owns passive reads; admitted changes belong to LocalServerOperations.
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
            automatic_start: AutomaticStart::default(),
            queued_automatic_start: None,
            last_confirmed_automatic_start: None,
            start_requested: false,
            read_outdated: false,
            _operation: None,
        }
    }

    /// Whether the service starts the server on its own, as it last confirmed: the latest read,
    /// or the read before it while the latest one failed. `None` while no service is installed or
    /// none was ever read.
    fn confirmed_automatic_start(&self) -> Option<bool> {
        match self.state.status {
            Some(status) => status.installed.then_some(status.enabled),
            None => self.last_confirmed_automatic_start,
        }
    }

    /// Whether the service starts the server on its own, as the switch shows it: a change the
    /// person asked for, until the service answers it, otherwise what the service confirmed.
    pub(super) fn automatic_start_shown(&self) -> Option<bool> {
        self.confirmed_automatic_start().map(|reported| {
            self.queued_automatic_start
                .map(|(enabled, _)| enabled)
                .or(self.automatic_start.requested)
                .unwrap_or(reported)
        })
    }

    /// What the last status read explains, while nothing but a passive read runs. A check
    /// keeps the card's footnote as it was; another action's progress replaces it.
    pub(super) fn settled_explanation(&self) -> Option<(SharedString, CaptionTone)> {
        if self
            .state
            .working
            .is_some_and(|action| action != ServiceAction::Check)
        {
            return None;
        }
        let presentation = match &self.control {
            Err(error) => present_service(ServiceState {
                failure: Some(*error),
                ..ServiceState::default()
            }),
            Ok(_) => present_service(ServiceState {
                working: None,
                ..self.state.clone()
            }),
        };
        Some((presentation.explanation, presentation.tone))
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

fn perform_service_operation(
    control: &dyn ServerServiceControl,
    action: ServiceAction,
    installation: Option<PathBuf>,
    startup: ServiceStartup,
) -> ServiceState {
    use ServiceAction as Action;
    let authorizing = matches!(action, Action::AuthorizeInstall | Action::AuthorizeStart);
    let retry = installation.clone();
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
        (Some(ServerError::AuthorizationRequired), Action::Install | Action::AuthorizeInstall) => {
            installation.map(AuthorizedRequest::Install)
        }
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
    if authorized {
        // Authorization continues only the request the person approved, including its exact build.
        if let Some(installation) = retry {
            if state
                .installations
                .iter()
                .any(|listed| listed.directory == installation)
            {
                return perform_service_operation(
                    control,
                    Action::Install,
                    Some(installation),
                    startup,
                );
            }
        } else {
            return perform_service_operation(control, Action::Start, None, startup);
        }
    }
    state
}

impl SettingsWindow {
    /// Checks the connection on the background executor. One check runs at a time, and none
    /// while the service is changing: its result checks the connection afresh.
    pub(super) fn check_local_server(&mut self, cx: &mut Context<Self>) {
        if self.server_changing() {
            return;
        }
        let Some(row) = &mut self.local_server else {
            return;
        };
        if row.checking {
            return;
        }
        let Ok(control) = &row.control else {
            return;
        };
        let control = Arc::clone(control);
        row.checking = true;
        // Only a server never checked has nothing to show meanwhile.
        if matches!(row.status, LocalServerStatus::NotChecked) {
            row.status = LocalServerStatus::Checking;
        }
        let result = cx.background_spawn(async move { control.refresh() });
        row._operation = Some(cx.spawn(async move |settings, cx| {
            let result = result.await;
            let _ = settings.update(cx, |settings, cx| {
                if let Some(row) = &mut settings.local_server {
                    row.checking = false;
                    row.status = match result {
                        Ok(connection) => LocalServerStatus::Connected(connection),
                        Err(error) => LocalServerStatus::Failed(error),
                    };
                }
                settings.continue_requested_start(cx);
                cx.notify();
            });
        }));
        cx.notify();
    }

    /// The description under a section's heading. A Development run's server ends with the run,
    /// so the Servers section never says its server keeps running after SpaceTerm quits.
    pub(super) fn section_description(
        &self,
        section: super::SettingsSectionId,
        cx: &gpui::App,
    ) -> &'static str {
        let development_run = self
            .local_server
            .as_ref()
            .is_some_and(|row| row.lifetime() == ServerLifetime::DevelopmentRun);
        if section == super::SettingsSectionId::Servers && development_run {
            DEVELOPMENT_RUN_DESCRIPTION
        } else {
            section.description(crate::desktop_profile::DesktopPresentation::get(cx))
        }
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
    /// change waits for the connection row to finish. A change to automatic start owns the row's
    /// task until it finishes, so even a passive read waits; the change reports the status.
    fn server_service_ready(&self, action: ServiceAction) -> bool {
        // One server operation runs at a time across the Servers rows.
        let others_working = self.local_server_working()
            || self
                .server_update
                .as_ref()
                .is_some_and(|row| row.state.working.is_some());
        self.server_service.as_ref().is_some_and(|row| {
            row.control.is_ok()
                && row.state.working.is_none()
                && !row.automatic_start.working
                && !(action.changes_service() && others_working)
        })
    }

    /// Whether the service or version row is running a step that may change the server. The
    /// connection is checked afresh when it finishes.
    pub(super) fn server_changing(&self) -> bool {
        self.server_service
            .as_ref()
            .is_some_and(ServerServiceRow::changing)
            || self
                .server_update
                .as_ref()
                .is_some_and(super::server_update::ServerUpdateRow::changing)
    }

    /// Whether the service row has an action running.
    pub(super) fn server_service_working(&self) -> bool {
        self.server_service
            .as_ref()
            .is_some_and(|row| row.state.working.is_some())
    }

    /// Whether the connection row has an action running.
    pub(super) fn local_server_working(&self) -> bool {
        self.local_server.as_ref().is_some_and(|row| row.checking)
    }

    /// Reads the service as the section comes into view. A running action keeps its place.
    pub(super) fn check_server_service(&mut self, cx: &mut Context<Self>) {
        self.run_server_service(ServiceAction::Check, cx);
    }

    /// Reads the service after it changed outside Settings. A read already running may have seen
    /// it before the change, so its answer is set aside and the service is read again once it
    /// ends.
    pub(super) fn reread_server_service(&mut self, cx: &mut Context<Self>) {
        if let Some(row) = &mut self.server_service
            && row.state.working == Some(ServiceAction::Check)
        {
            row.read_outdated = true;
            return;
        }
        self.check_server_service(cx);
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
            // Start chosen while the Servers rows only read waits for them, as Update does.
            if action == ServiceAction::Start && self.server_only_reading() {
                if let Some(row) = &mut self.server_service {
                    row.start_requested = true;
                }
                cx.notify();
            }
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

    /// Whether the Servers rows are only reading the server, with nothing underway that changes
    /// it.
    fn server_only_reading(&self) -> bool {
        let service_reads = self.server_service.as_ref().is_some_and(|row| {
            row.control.is_ok()
                && row
                    .state
                    .working
                    .is_none_or(|action| action == ServiceAction::Check)
        });
        let update_reads = self.server_update.as_ref().is_none_or(|row| {
            row.state
                .working
                .is_none_or(|step| step == super::server_update::UpdateStep::Check)
        });
        service_reads && update_reads
    }

    /// Starts the server once the reads that held Start Server back finish, if the service still
    /// offers it. A read that found it running or couldn't read the service drops the request.
    pub(super) fn continue_requested_start(&mut self, cx: &mut Context<Self>) {
        let requested = self
            .server_service
            .as_ref()
            .is_some_and(|row| row.start_requested);
        if !requested || !self.server_service_ready(ServiceAction::Start) {
            return;
        }
        if let Some(row) = &mut self.server_service {
            row.start_requested = false;
        }
        let offered = self
            .server_service_presentation()
            .is_some_and(|presentation| presentation.actions.contains(&ServiceAction::Start));
        if offered {
            self.run_server_service(ServiceAction::Start, cx);
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

    /// Turns automatic start on or off for this account's server. On a desktop that starts it at
    /// boot, turning it on may need lingering, which SpaceTerm explains and asks for first; only
    /// the person's confirmation asks the operating system for a password.
    pub(super) fn set_local_automatic_start(
        &mut self,
        enabled: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.change_automatic_start(enabled, false, window.window_handle(), cx);
    }

    fn change_automatic_start(
        &mut self,
        enabled: bool,
        authorize: bool,
        window: gpui::AnyWindowHandle,
        cx: &mut Context<Self>,
    ) {
        let Some(row) = self.server_service.as_mut() else {
            return;
        };
        let Ok(control) = row.control.as_ref().map(Arc::clone) else {
            return;
        };
        if row.automatic_start.working || row.state.working.is_some() {
            // A read or an earlier change finishes first. The switch shows the change meanwhile,
            // and the change follows once the service answers. Other work changes the service,
            // so it takes no change.
            if !authorize
                && (row.automatic_start.working || row.state.working == Some(ServiceAction::Check))
            {
                row.queued_automatic_start = Some((enabled, window));
                cx.notify();
            }
            return;
        }
        row.queued_automatic_start = None;
        row.automatic_start = AutomaticStart {
            working: true,
            failure: None,
            requested: Some(enabled),
        };
        let changed = cx.background_spawn(async move {
            if authorize {
                control.authorize_startup()?;
            }
            control.set_automatic_start(enabled)
        });
        row._operation = Some(cx.spawn(async move |settings, cx| {
            let changed = changed.await;
            let _ = settings.update(cx, |settings, cx| {
                let Some(row) = settings.server_service.as_mut() else {
                    return;
                };
                row.automatic_start.working = false;
                row.automatic_start.requested = None;
                match changed {
                    Ok(status) => row.state.status = Some(status),
                    // Only the person's confirmation asks for a password, once per change.
                    Err(ServerError::AuthorizationRequired) if enabled && !authorize => {
                        settings.confirm_automatic_start_authorization(window, cx);
                    }
                    Err(error) => row.automatic_start.failure = Some(error),
                }
                settings.continue_automatic_start(cx);
                settings.continue_requested_start(cx);
                cx.notify();
            });
        }));
        cx.notify();
    }

    /// Makes the automatic start change the person asked for while a read or an earlier change
    /// ran, once the service is idle. A read that couldn't confirm the service drops it, so the
    /// switch returns to what the service last reported.
    fn continue_automatic_start(&mut self, cx: &mut Context<Self>) {
        let Some(row) = self.server_service.as_mut() else {
            return;
        };
        if row.automatic_start.working || row.state.working.is_some() {
            return;
        }
        let Some((enabled, window)) = row.queued_automatic_start.take() else {
            return;
        };
        // Only the latest read's own status admits the change. An earlier operation's failure
        // it still shows doesn't make the read fail.
        let reported = row
            .state
            .status
            .filter(|status| status.installed)
            .map(|status| status.enabled);
        if reported.is_some_and(|reported| reported != enabled) {
            self.change_automatic_start(enabled, false, window, cx);
        }
        cx.notify();
    }

    /// Explains lingering before the operating system asks for an administrator password, so the
    /// server can start at boot. Cancelling leaves automatic start off. The alert opens once the
    /// current update finishes, since the answer that needs it arrives inside one.
    fn confirm_automatic_start_authorization(
        &mut self,
        handle: gpui::AnyWindowHandle,
        cx: &mut Context<Self>,
    ) {
        let owner = cx.weak_entity();
        cx.defer(move |cx| {
            let presenting = owner.clone();
            let _ = handle.update(cx, move |_, window, cx| {
                // An alert belongs to the entity presenting it, so Settings presents it.
                let _ = presenting.update(cx, |_, cx| {
                    let result = Alert::new(
                        ModalId::new("settings-server-auto-start-authorize"),
                        "Start the Server at Boot?",
                        "Start the Server at Boot?",
                        "Enter an administrator password so your account\u{2019}s services start at boot.",
                        vec![
                            ModalAction::new(
                                true,
                                "Authorize",
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
                    .detail("This applies to every service your account runs.")
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
                            settings.change_automatic_start(true, true, handle, cx);
                        });
                    });
                    if result.is_err() {
                        eprintln!("failed to present the automatic start authorization");
                    }
                });
            });
        });
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
        let prior_failure = self.local_operations.read(cx).service_failure;
        let result = cx.background_spawn(async move {
            let mut state =
                perform_service_operation(control.as_ref(), action, installation, startup);
            if action == ServiceAction::Check {
                state.failure = prior_failure.or(state.failure);
            }
            state
        });
        if action.changes_service() {
            let state = row.state.clone();
            self.local_operations
                .update(cx, |owner, cx| owner.run_service(state, result, cx));
        } else {
            row._operation = Some(cx.spawn(async move |settings, cx| {
                let state = result.await;
                let _ = settings.update(cx, |settings, cx| {
                    // An answer from before an outside change describes nothing current.
                    if let Some(row) = &mut settings.server_service
                        && std::mem::take(&mut row.read_outdated)
                    {
                        row.state.working = None;
                        settings.check_server_service(cx);
                        cx.notify();
                        return;
                    }
                    settings
                        .local_operations
                        .update(cx, |owner, _| owner.service = Some(state.clone()));
                    if let Some(row) = &mut settings.server_service {
                        // A read that fails keeps the setting the service last confirmed.
                        let confirmed = row.confirmed_automatic_start();
                        row.state = state;
                        row.last_confirmed_automatic_start = confirmed;
                    }
                    settings.continue_automatic_start(cx);
                    settings.continue_requested_start(cx);
                    cx.notify();
                });
            }));
        }
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
            confirmation.message.clone(),
            confirmation.detail.clone(),
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
}

/// A local server whose answers a test sets and whose operations it counts.
#[cfg(test)]
pub(super) mod testing {
    use std::sync::Arc;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use std::path::{Path, PathBuf};

    use super::{ConnectedServer, LocalServerControl, ServerServiceControl, ServiceStartup};
    use crate::server_connection::ServerError;
    use crate::server_connection::service::{RetainedInstallation, ServiceStatus};

    /// A server whose answers the test sets and whose operations it counts.
    pub(in crate::ui::settings_window) struct ScriptedServer {
        refresh: Mutex<Result<ConnectedServer, ServerError>>,
        refreshes: AtomicUsize,
    }

    impl ScriptedServer {
        pub(in crate::ui::settings_window) fn new(
            refresh: Result<ConnectedServer, ServerError>,
        ) -> Arc<Self> {
            Arc::new(Self {
                refresh: Mutex::new(refresh),
                refreshes: AtomicUsize::new(0),
            })
        }

        pub(in crate::ui::settings_window) fn answer_refresh(
            &self,
            result: Result<ConnectedServer, ServerError>,
        ) {
            *self.refresh.lock().expect("refresh answer") = result;
        }

        pub(in crate::ui::settings_window) fn refreshes(&self) -> usize {
            self.refreshes.load(Ordering::SeqCst)
        }
    }

    impl LocalServerControl for ScriptedServer {
        fn refresh(&self) -> Result<ConnectedServer, ServerError> {
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
        SetAutomaticStart(bool),
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
        /// What the next status read answers, as a read the service began before a later change
        /// would, ahead of what the service reports now.
        earlier: Mutex<Option<Result<ServiceStatus, ServerError>>>,
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
                earlier: Mutex::new(None),
            })
        }

        /// Changes what the service reports, as the operating system would.
        pub(in crate::ui::settings_window) fn report(
            &self,
            status: Result<ServiceStatus, ServerError>,
        ) {
            *self.status.lock().expect("status") = status;
        }

        /// Makes the next status read answer what the service reports now, even after a later
        /// change, as a read that began before the change does.
        pub(in crate::ui::settings_window) fn read_before_change(&self) {
            let now = *self.status.lock().expect("status");
            *self.earlier.lock().expect("earlier") = Some(now);
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
            if let Some(earlier) = self.earlier.lock().expect("earlier").take() {
                return earlier;
            }
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

        fn set_automatic_start(&self, enabled: bool) -> Result<ServiceStatus, ServerError> {
            self.calls
                .lock()
                .expect("calls")
                .push(ServiceCall::SetAutomaticStart(enabled));
            if let Some(error) = self.refusal.lock().expect("refusal").take() {
                return Err(error);
            }
            // This fixture requires authorization for starting at boot.
            if enabled
                && self.startup == ServiceStartup::AtBoot
                && !*self.authorized.lock().expect("authorized")
            {
                return Err(ServerError::AuthorizationRequired);
            }
            let mut status = self.status.lock().expect("status");
            let changed = ServiceStatus {
                enabled,
                ..(*status).unwrap_or(not_installed())
            };
            *status = Ok(changed);
            Ok(changed)
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
    pub(in crate::ui::settings_window) const fn connected(protocol_major: u16) -> ConnectedServer {
        ConnectedServer { protocol_major }
    }
}

#[cfg(test)]
#[path = "servers_tests.rs"]
mod tests;
