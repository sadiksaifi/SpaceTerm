//! The selected server's card: Status, Version, Start automatically and Terminal Sessions.
//!
//! Each row reads what SpaceTerm already knows: this account's service and update rows, the SSH
//! connection registry and maintenance owner, and the direct registry. Rendering asks no server
//! anything. When SpaceTerm isn't connected to the server, the rows under Status dim and show what
//! SpaceTerm last observed, and the card's footnote says how to change them.

use gpui::prelude::*;
use gpui::{AnyElement, App, Context, ElementId, SharedString, Window, accesskit, div};
use spaceterm_ui::{
    AnchoredAlignment, AnchoredPlacement, AnchoredPlacementConfig, Button, ButtonSize,
    ButtonVariant, Icon, IconName, Menu, MenuEntry, ProgressRing, ProgressSize, ProgressState,
    Switch, ToggleSize,
};

use super::SettingsWindow;
use super::server_list::{ServerChoice, status_dot, tone_color};
use super::server_selection;
use super::server_update::UpdateAction;
use super::servers::{LocalServerStatus, ServiceAction, ServicePhase};
use crate::direct_connections::{DirectRouteStatus, RefusalReason};
use crate::local_server::ServerError;
use crate::local_server::service::control::Action as ServiceControlAction;
use crate::server_context::{LastKnownFacts, ObservedServerStatus};
use crate::ssh::server_connection::{
    ConnectPhase, IncompatibilityKind, RemoteConnectionStatus, ServerAvailability,
};
use crate::ssh::server_maintenance::SubmittedKind;
use crate::ssh::server_operations::RemoteOperationState;
use crate::ui::appearance::{ChromeAppearance, gpui_color};
use crate::ui::chrome_icons::IconRole;
use crate::ui::chrome_typography::{ChromeTextStyleExt as _, TextRole};
use crate::ui::direct_servers::DirectServerAction;
use crate::ui::remote_maintenance::{MaintenanceRow, RemoteAction};
use crate::ui::sidebar_window::form::{CaptionTone, badge};
use crate::ui::ssh_servers::SshServerAction;

pub(super) const STATUS_SELECTOR: &str = "settings-server-status";
pub(super) const MENU_SELECTOR: &str = "settings-server-menu";
pub(super) const VERSION_SELECTOR: &str = "settings-server-version";
pub(super) const AUTO_START_SELECTOR: &str = "settings-server-auto-start";

/// The color a status dot takes, and what it says.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::ui::settings_window) enum Tone {
    /// Running and connected.
    Live,
    /// Not connected, or stopped by the person.
    Idle,
    /// Waiting for the person, or for something that should resolve.
    Attention,
    /// Refused, incompatible or failed.
    Problem,
}

/// How one server's state reads.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(in crate::ui::settings_window) struct ServerState {
    /// What the card's Status row says, such as "Connected over SSH" or "Starting…".
    pub(in crate::ui::settings_window) state: SharedString,
    /// What the picker and All Servers say, such as "Connected · SSH".
    pub(in crate::ui::settings_window) short: SharedString,
    pub(in crate::ui::settings_window) tone: Tone,
    /// Work is underway, so a spinner stands in for the dot.
    pub(in crate::ui::settings_window) working: bool,
    /// SpaceTerm is connected to the server, so its settings can change.
    pub(in crate::ui::settings_window) live: bool,
}

impl ServerState {
    fn new(state: impl Into<SharedString>, short: impl Into<SharedString>, tone: Tone) -> Self {
        Self {
            state: state.into(),
            short: short.into(),
            tone,
            working: false,
            live: false,
        }
    }

    fn live(mut self) -> Self {
        self.live = true;
        self
    }

    /// Work underway reads as one verb, on the card and in the lists alike.
    fn working(verb: &'static str, live: bool) -> Self {
        Self {
            state: verb.into(),
            short: verb.into(),
            tone: Tone::Attention,
            working: true,
            live,
        }
    }
}

/// The verb a local service step shows while it runs.
pub(super) const fn service_verb(action: ServiceAction) -> &'static str {
    match action {
        ServiceAction::Check => "Checking\u{2026}",
        ServiceAction::Install => "Installing\u{2026}",
        ServiceAction::Start => "Starting\u{2026}",
        ServiceAction::Stop => "Stopping\u{2026}",
        ServiceAction::Restart => "Restarting\u{2026}",
        ServiceAction::Uninstall => "Uninstalling\u{2026}",
        ServiceAction::AuthorizeInstall | ServiceAction::AuthorizeStart => "Authorizing\u{2026}",
    }
}

/// The verb a remote maintenance operation shows while it runs. Work with no submitted
/// operation is a read.
pub(super) fn maintenance_verb(kind: Option<&SubmittedKind>) -> &'static str {
    match kind {
        Some(SubmittedKind::Start) => "Starting\u{2026}",
        Some(SubmittedKind::Service(ServiceControlAction::Stop)) => "Stopping\u{2026}",
        Some(SubmittedKind::Service(ServiceControlAction::Restart)) => "Restarting\u{2026}",
        Some(SubmittedKind::Service(ServiceControlAction::Uninstall)) => "Uninstalling\u{2026}",
        Some(SubmittedKind::Update { .. } | SubmittedKind::Return) => "Updating\u{2026}",
        None => "Checking\u{2026}",
    }
}

/// How this account's server reads. `service` is `None` where no service runs it, such as a
/// Development run. A connection refused for want of a protocol in common needs an update.
pub(super) fn local_state(
    connection: LocalServerStatus,
    service: Option<(Option<ServicePhase>, Option<ServiceAction>, bool)>,
    updating: bool,
) -> ServerState {
    let connected = matches!(connection, LocalServerStatus::Connected(_));
    if updating {
        return ServerState::working("Updating\u{2026}", connected);
    }
    if let Some((_, Some(action), _)) = service
        && action != ServiceAction::Check
    {
        return ServerState::working(service_verb(action), connected);
    }
    if matches!(
        connection,
        LocalServerStatus::Failed(
            ServerError::IncompatibleProtocol | ServerError::MissingRequiredCapability
        )
    ) {
        return ServerState::new("Needs update", "Needs update", Tone::Problem);
    }
    match service {
        Some((_, _, true)) => ServerState::new(
            "Needs authorization",
            "Needs authorization",
            Tone::Attention,
        ),
        Some((Some(ServicePhase::NotInstalled), _, _)) => {
            ServerState::new("Not installed", "Not installed", Tone::Idle)
        }
        Some((Some(ServicePhase::Stopped), _, _)) => {
            ServerState::new("Stopped", "Stopped", Tone::Idle)
        }
        Some((Some(ServicePhase::NotRunning), _, _)) => {
            ServerState::new("Not running", "Not running", Tone::Attention)
        }
        Some((Some(ServicePhase::Running), _, _)) | None if connected => {
            ServerState::new("Running", "Running \u{b7} this account", Tone::Live).live()
        }
        Some((Some(ServicePhase::Running), _, _)) => {
            ServerState::new("Running", "Running \u{b7} this account", Tone::Live)
        }
        _ => match connection {
            LocalServerStatus::NotChecked | LocalServerStatus::Checking => {
                ServerState::working("Checking\u{2026}", false)
            }
            LocalServerStatus::Failed(ServerError::Stopped) => {
                ServerState::new("Stopped", "Stopped", Tone::Idle)
            }
            LocalServerStatus::Failed(ServerError::NotInstalled | ServerError::NotSetUp) => {
                ServerState::new("Not installed", "Not installed", Tone::Idle)
            }
            LocalServerStatus::Failed(ServerError::NotRunning | ServerError::StartupFailed) => {
                ServerState::new("Not running", "Not running", Tone::Attention)
            }
            LocalServerStatus::Failed(_) => {
                ServerState::new("Unavailable", "Unavailable", Tone::Problem)
            }
            LocalServerStatus::Connected(_) => {
                ServerState::new("Running", "Running \u{b7} this account", Tone::Live).live()
            }
        },
    }
}

/// How an SSH server reads. `maintenance` is the operation underway on its account, if any.
pub(super) fn ssh_state(
    status: &RemoteConnectionStatus,
    maintenance: Option<Option<&SubmittedKind>>,
    service: Option<crate::local_server::service::ServiceStatus>,
    retained: Option<ObservedServerStatus>,
) -> ServerState {
    let connected = matches!(status, RemoteConnectionStatus::Connected { .. });
    if let Some(kind) = maintenance {
        return ServerState::working(maintenance_verb(kind), connected);
    }
    // A service read reports the server SpaceTerm isn't connected to, such as one stopped from
    // here, which only Start Server starts again.
    if let (false, Some(service)) = (
        connected
            || matches!(
                status,
                RemoteConnectionStatus::Working { .. }
                    | RemoteConnectionStatus::Reconnecting { .. }
                    | RemoteConnectionStatus::Cancelling
            ),
        service,
    ) {
        match ServicePhase::of(service) {
            ServicePhase::Stopped => return ServerState::new("Stopped", "Stopped", Tone::Idle),
            ServicePhase::NotInstalled => {
                return ServerState::new("Not installed", "Not installed", Tone::Idle);
            }
            ServicePhase::NotRunning => {
                return ServerState::new("Not running", "Not running", Tone::Attention);
            }
            ServicePhase::Running => {}
        }
    }
    match status {
        RemoteConnectionStatus::Connected { .. } => {
            ServerState::new("Connected over SSH", "Connected \u{b7} SSH", Tone::Live).live()
        }
        RemoteConnectionStatus::Working { phase, .. } => ServerState::working(
            match phase {
                ConnectPhase::Installing => "Installing\u{2026}",
                ConnectPhase::Starting => "Starting\u{2026}",
                ConnectPhase::Authenticating
                | ConnectPhase::Discovering
                | ConnectPhase::Connecting => "Connecting\u{2026}",
            },
            false,
        ),
        RemoteConnectionStatus::Reconnecting { .. } => {
            ServerState::working("Reconnecting\u{2026}", false)
        }
        RemoteConnectionStatus::Cancelling => ServerState::working("Cancelling\u{2026}", false),
        RemoteConnectionStatus::NeedsPerson { .. } => {
            ServerState::new("Waiting for you", "Waiting for you", Tone::Attention)
        }
        RemoteConnectionStatus::InstallationOffered { .. } => {
            ServerState::new("Not installed", "Not installed", Tone::Idle)
        }
        RemoteConnectionStatus::Checked { availability, .. } => match availability {
            ServerAvailability::Running => {
                ServerState::new("Not connected", "Not connected", Tone::Idle)
            }
            ServerAvailability::NotRunning => {
                ServerState::new("Not running", "Not running", Tone::Idle)
            }
            ServerAvailability::NotInstalled => {
                ServerState::new("Not installed", "Not installed", Tone::Idle)
            }
        },
        RemoteConnectionStatus::Stopped => ServerState::new("Stopped", "Stopped", Tone::Idle),
        RemoteConnectionStatus::Incompatible {
            kind: IncompatibilityKind::Platform,
        } => ServerState::new("Unsupported computer", "Unsupported", Tone::Problem),
        RemoteConnectionStatus::Incompatible { .. } => {
            ServerState::new("Needs update", "Needs update", Tone::Problem)
        }
        RemoteConnectionStatus::Paused { .. } => {
            ServerState::new("Server changed", "Server changed", Tone::Problem)
        }
        RemoteConnectionStatus::Refused { .. } => {
            ServerState::new("Refused", "Refused", Tone::Problem)
        }
        RemoteConnectionStatus::Unavailable { .. } => {
            ServerState::new("Not reachable", "Not reachable", Tone::Attention)
        }
        RemoteConnectionStatus::Unconfirmed { .. } => {
            ServerState::new("Unconfirmed", "Unconfirmed", Tone::Attention)
        }
        // Nothing reports on a saved connection that isn't active, so it shows what was last
        // observed. That presents a state only: what the row offers follows its connection.
        RemoteConnectionStatus::Inactive => match retained {
            Some(ObservedServerStatus::Stopped) => {
                ServerState::new("Stopped", "Stopped", Tone::Idle)
            }
            Some(ObservedServerStatus::NotInstalled) => {
                ServerState::new("Not installed", "Not installed", Tone::Idle)
            }
            Some(ObservedServerStatus::NeedsUpdate) => {
                ServerState::new("Needs update", "Needs update", Tone::Problem)
            }
            _ => ServerState::new("Not connected", "Not connected", Tone::Idle),
        },
    }
}

/// How a paired server reads.
pub(super) fn direct_state(status: DirectRouteStatus) -> ServerState {
    match status {
        DirectRouteStatus::Connected => ServerState::new(
            "Connected over the network",
            "Connected \u{b7} Paired",
            Tone::Live,
        )
        .live(),
        DirectRouteStatus::Connecting => ServerState::working("Connecting\u{2026}", false),
        DirectRouteStatus::Reconnecting => ServerState::working("Reconnecting\u{2026}", false),
        DirectRouteStatus::Disconnected => {
            ServerState::new("Not connected", "Not connected", Tone::Idle)
        }
        DirectRouteStatus::NeedsInteraction => {
            ServerState::new("Waiting for you", "Waiting for you", Tone::Attention)
        }
        DirectRouteStatus::Refused(RefusalReason::Revoked) => {
            ServerState::new("Unpaired", "Unpaired", Tone::Problem)
        }
        DirectRouteStatus::Refused(RefusalReason::Incompatible) => {
            ServerState::new("Needs update", "Needs update", Tone::Problem)
        }
        DirectRouteStatus::Refused(_) => {
            ServerState::new("Not verified", "Not verified", Tone::Problem)
        }
        DirectRouteStatus::Unavailable(_) => {
            ServerState::new("Not reachable", "Not reachable", Tone::Attention)
        }
    }
}

/// The name a person knows an operating system by, from the name the backend reports.
pub(super) fn operating_system_name(reported: &str) -> SharedString {
    match reported.to_ascii_lowercase().as_str() {
        "macos" | "darwin" | "mac os" => "macOS".into(),
        "linux" => "Linux".into(),
        _ => SharedString::from(reported.to_owned()),
    }
}

/// A step the Status row or its menu can take.
#[derive(Clone, Debug, Eq, PartialEq)]
enum StatusStep {
    Local(ServiceAction),
    Ssh(SshServerAction),
    Maintenance(RemoteAction),
    Direct(DirectServerAction),
    /// Asks before forgetting the saved SSH connection.
    RemoveSsh,
    UnpairThisDevice,
}

/// One entry of the Status row's ⋯ menu.
struct MenuItem {
    label: &'static str,
    step: StatusStep,
    enabled: bool,
    destructive: bool,
    separator_before: bool,
}

impl MenuItem {
    fn new(label: &'static str, step: StatusStep, enabled: bool) -> Self {
        Self {
            label,
            step,
            enabled,
            destructive: false,
            separator_before: false,
        }
    }

    fn destructive(mut self) -> Self {
        self.destructive = true;
        self
    }

    fn after_separator(mut self) -> Self {
        self.separator_before = true;
        self
    }
}

impl SettingsWindow {
    /// How `choice` reads now, from what SpaceTerm already knows about it.
    pub(in crate::ui::settings_window) fn server_state(
        &self,
        choice: &ServerChoice,
    ) -> ServerState {
        match choice {
            ServerChoice::Local => {
                let connection = self
                    .local_server
                    .as_ref()
                    .map_or(LocalServerStatus::NotChecked, |row| row.current_status());
                let service = self.server_service.as_ref().map(|row| {
                    let presentation = row.presentation();
                    (
                        row.state.status.map(ServicePhase::of),
                        presentation.working,
                        presentation.authorization.is_some(),
                    )
                });
                let updating = self
                    .server_update
                    .as_ref()
                    .and_then(|row| row.state.working)
                    .is_some_and(|step| step.changes_service());
                local_state(connection, service, updating)
            }
            ServerChoice::Ssh(destination) => {
                let status = self
                    .ssh_servers
                    .as_ref()
                    .map_or(RemoteConnectionStatus::Inactive, |rows| {
                        rows.connections.status(destination)
                    });
                let entries: Vec<RemoteOperationState> = self
                    .ssh_maintenance
                    .as_ref()
                    .map(|rows| {
                        rows.operations
                            .entries()
                            .into_iter()
                            .filter(|entry| entry.destination == *destination && !entry.removed)
                            .map(|entry| entry.state)
                            .collect()
                    })
                    .unwrap_or_default();
                let working = entries.iter().find_map(|state| match state {
                    RemoteOperationState::Working { kind, .. } => Some(kind.as_ref()),
                    _ => None,
                });
                let service = entries.iter().find_map(|state| match state {
                    RemoteOperationState::Read(facts) => Some(facts.status),
                    _ => None,
                });
                let retained = self.retained_status.get(&choice.key()).copied();
                let state = ssh_state(&status, working, service, retained);
                // An operation whose result SpaceTerm couldn't confirm is what the person waits
                // on, until resolving that same operation settles it.
                let unconfirmed = entries
                    .iter()
                    .any(|state| matches!(state, RemoteOperationState::Unconfirmed { .. }));
                if unconfirmed && !state.working {
                    ServerState {
                        state: "Unconfirmed".into(),
                        short: "Unconfirmed".into(),
                        tone: Tone::Attention,
                        ..state
                    }
                } else {
                    state
                }
            }
            ServerChoice::Direct(route) => {
                let retained = self.retained_status.get(&choice.key()).copied();
                match self
                    .saved_direct_servers()
                    .into_iter()
                    .find(|(saved, _, _)| saved == route)
                {
                    Some((_, DirectRouteStatus::Disconnected, _))
                        if retained == Some(ObservedServerStatus::NeedsUpdate) =>
                    {
                        ServerState::new("Needs update", "Needs update", Tone::Problem)
                    }
                    Some((_, status, _)) => direct_state(status),
                    None => ServerState::new("Not connected", "Not connected", Tone::Idle),
                }
            }
        }
    }

    /// "machine · OS" for this computer and SSH servers, "address · OS" for paired ones. A
    /// part SpaceTerm hasn't observed is left out, and a server it knows nothing about says how
    /// it is reached.
    pub(in crate::ui::settings_window) fn server_detail(
        &self,
        choice: &ServerChoice,
        cx: &App,
    ) -> SharedString {
        let facts = server_selection::facts(&choice.key(), cx);
        let observed_system = facts
            .as_ref()
            .and_then(|facts| facts.operating_system.clone())
            .map(|system| operating_system_name(&system));
        let (place, system) = match choice {
            ServerChoice::Local => (
                facts.as_ref().and_then(|facts| facts.machine.clone()),
                observed_system,
            ),
            ServerChoice::Ssh(destination) => {
                let reported = self
                    .ssh_servers
                    .as_ref()
                    .and_then(|rows| rows.connections.last_facts(destination))
                    .map(|facts| operating_system_name(&facts.platform.0));
                (
                    facts
                        .as_ref()
                        .and_then(|facts| facts.machine.clone())
                        .or_else(|| Some(destination.as_str().to_owned())),
                    observed_system.or(reported),
                )
            }
            ServerChoice::Direct(route) => (
                self.direct_routes
                    .as_ref()
                    .and_then(|rows| rows.connections.route(route))
                    .map(|route| route.endpoint),
                observed_system,
            ),
        };
        match (place, system) {
            (Some(place), Some(system)) => SharedString::from(format!("{place} \u{b7} {system}")),
            (Some(place), None) => SharedString::from(place),
            (None, Some(system)) => system,
            (None, None) => match choice {
                ServerChoice::Local => SharedString::new_static("This account on this computer"),
                ServerChoice::Ssh(_) => SharedString::new_static("Over SSH"),
                ServerChoice::Direct(_) => SharedString::new_static("Paired over the network"),
            },
        }
    }

    /// Whether SpaceTerm is connected to the selected server now.
    pub(in crate::ui::settings_window) fn selected_server_live(&self) -> bool {
        self.selected_server()
            .is_some_and(|choice| self.server_state(&choice).live)
    }

    /// What the selected server's card says while it isn't live: how to change its settings,
    /// and when the shown values were observed.
    pub(super) fn server_card_footnote(&self, cx: &App) -> Option<SharedString> {
        let choice = self.selected_server()?;
        if self.server_state(&choice).live {
            return None;
        }
        let name = self.server_name(&choice);
        let ask = match choice {
            ServerChoice::Local => format!("Start the server on {name} to change these settings."),
            _ => format!("Connect to {name} to change these settings."),
        };
        let observed = server_selection::facts(&choice.key(), cx)
            .map(|facts| facts.observed_at)
            .filter(|at| *at > 0)
            .map(|at| format!(" Shown values are from {}.", observed_time(at)));
        Some(SharedString::from(format!(
            "{ask}{}",
            observed.unwrap_or_default()
        )))
    }

    /// The facts the selected server's dimmed rows show while it isn't live.
    fn selected_facts(&self, cx: &App) -> Option<LastKnownFacts> {
        server_selection::facts(&self.selected_server()?.key(), cx)
    }

    /// What the selected server's Status reports under its control: a refusal or failure, work
    /// underway, or a result that waits for the person. A plain state needs no words.
    pub(super) fn status_message(&self) -> Option<(SharedString, CaptionTone)> {
        let attention = |text: SharedString, tone: CaptionTone, attention: bool| {
            (attention || tone != CaptionTone::Guidance).then_some((text, tone))
        };
        match self.selected_server()? {
            ServerChoice::Local => {
                let service = self.server_service_presentation().and_then(|service| {
                    let busy = service.working.is_some() || service.authorization.is_some();
                    attention(service.explanation, service.tone, busy)
                });
                service.or_else(|| {
                    let local = self.local_server_presentation()?;
                    attention(local.explanation, local.tone, false)
                })
            }
            ServerChoice::Ssh(destination) => {
                let maintenance = self
                    .maintenance_presentation(MaintenanceRow::Service)
                    .and_then(|maintenance| {
                        let waiting = maintenance_waits(&maintenance);
                        attention(maintenance.explanation, maintenance.tone, waiting)
                    });
                maintenance.or_else(|| {
                    let status = self.ssh_servers.as_ref()?.connections.status(&destination);
                    let connection = crate::ui::ssh_servers::present(&status);
                    attention(connection.explanation, connection.tone, false)
                })
            }
            ServerChoice::Direct(route) => self
                .direct_problem()
                .map(|problem| (problem, CaptionTone::Error))
                .or_else(|| {
                    let (_, status, name) = self
                        .saved_direct_servers()
                        .into_iter()
                        .find(|(saved, _, _)| *saved == route)?;
                    let presentation = crate::ui::direct_servers::present(status, &name);
                    attention(
                        SharedString::from(presentation.explanation),
                        presentation.tone,
                        false,
                    )
                }),
        }
    }

    /// What the selected server's Version reports under its control while an update runs, waits
    /// for the person or didn't finish.
    pub(super) fn version_message(&self) -> Option<(SharedString, CaptionTone)> {
        match self.selected_server()? {
            ServerChoice::Local => {
                let update = self.server_update_presentation()?;
                (update.working || update.tone != CaptionTone::Guidance)
                    .then_some((update.explanation, update.tone))
            }
            ServerChoice::Ssh(_) => {
                let update = self.maintenance_presentation(MaintenanceRow::Update)?;
                (maintenance_waits(&update) || update.tone != CaptionTone::Guidance)
                    .then_some((update.explanation, update.tone))
            }
            ServerChoice::Direct(_) => None,
        }
    }

    /// The Status row's description: where the server is.
    pub(super) fn status_description(&self, cx: &App) -> SharedString {
        self.selected_server()
            .map_or_else(SharedString::default, |choice| {
                self.server_detail(&choice, cx)
            })
    }

    /// The steps the Status row shows beside its state, in order, and the ones its ⋯ menu holds.
    fn status_steps(&self, choice: &ServerChoice) -> (Vec<(StatusStep, bool)>, Vec<MenuItem>) {
        match choice {
            ServerChoice::Local => self.local_status_steps(),
            ServerChoice::Ssh(destination) => self.ssh_status_steps(destination),
            ServerChoice::Direct(route) => self.direct_status_steps(*route),
        }
    }

    fn local_status_steps(&self) -> (Vec<(StatusStep, bool)>, Vec<MenuItem>) {
        let Some(presentation) = self.server_service_presentation() else {
            // Without a service, such as in a Development run, the connection can still be
            // checked again.
            let check = self
                .local_server_presentation()
                .is_some_and(|local| local.check && !local.working);
            let buttons = check
                .then_some((StatusStep::Local(ServiceAction::Check), false))
                .into_iter()
                .collect();
            return (buttons, Vec::new());
        };
        let offered = |action: ServiceAction| presentation.actions.contains(&action);
        let idle = presentation.working.is_none();
        let mut buttons = Vec::new();
        for action in [
            ServiceAction::AuthorizeStart,
            ServiceAction::AuthorizeInstall,
            ServiceAction::Start,
            ServiceAction::Install,
            ServiceAction::Check,
        ] {
            if offered(action) {
                buttons.push((
                    StatusStep::Local(action),
                    matches!(action, ServiceAction::Install),
                ));
            }
        }
        let installed = self
            .server_service
            .as_ref()
            .and_then(|row| row.state.status)
            .is_some_and(|status| status.installed);
        let menu = if installed {
            vec![
                MenuItem::new(
                    "Restart Server",
                    StatusStep::Local(ServiceAction::Restart),
                    idle && offered(ServiceAction::Restart),
                ),
                MenuItem::new(
                    "Stop Server",
                    StatusStep::Local(ServiceAction::Stop),
                    idle && offered(ServiceAction::Stop),
                ),
                MenuItem::new(
                    "Uninstall Server\u{2026}",
                    StatusStep::Local(ServiceAction::Uninstall),
                    idle && offered(ServiceAction::Uninstall),
                )
                .destructive()
                .after_separator(),
            ]
        } else {
            Vec::new()
        };
        (buttons, menu)
    }

    fn ssh_status_steps(
        &self,
        destination: &crate::domain::SshDestination,
    ) -> (Vec<(StatusStep, bool)>, Vec<MenuItem>) {
        let status = self
            .ssh_servers
            .as_ref()
            .map_or(RemoteConnectionStatus::Inactive, |rows| {
                rows.connections.status(destination)
            });
        let connection = self.ssh_server_actions(destination, &status);
        let maintenance = self
            .maintenance_presentation(MaintenanceRow::Service)
            .map(|presentation| presentation.actions)
            .unwrap_or_default();
        let offered = |action: RemoteAction| maintenance.contains(&action);
        let mut buttons: Vec<(StatusStep, bool)> = Vec::new();
        // A deliberately stopped server starts only by Start, which the maintenance owner runs
        // on the account route.
        if offered(RemoteAction::Start) {
            buttons.push((StatusStep::Maintenance(RemoteAction::Start), false));
        }
        for action in [
            RemoteAction::Finish,
            RemoteAction::CheckAgain,
            RemoteAction::Acknowledge,
            RemoteAction::Dismiss,
            RemoteAction::Cancel,
            RemoteAction::Discard,
        ] {
            if offered(action) {
                buttons.push((StatusStep::Maintenance(action), false));
            }
        }
        for action in [
            SshServerAction::Install,
            SshServerAction::CheckStatus,
            SshServerAction::Connect,
            SshServerAction::Cancel,
            SshServerAction::Disconnect,
            SshServerAction::StopReconnecting,
        ] {
            if connection.contains(&action) {
                buttons.push((
                    StatusStep::Ssh(action),
                    matches!(action, SshServerAction::Install),
                ));
            }
        }
        let menu = vec![
            MenuItem::new(
                "Restart Server",
                StatusStep::Maintenance(RemoteAction::Restart),
                offered(RemoteAction::Restart),
            ),
            MenuItem::new(
                "Stop Server",
                StatusStep::Maintenance(RemoteAction::Stop),
                offered(RemoteAction::Stop),
            ),
            MenuItem::new(
                "Uninstall Server\u{2026}",
                StatusStep::Maintenance(RemoteAction::Uninstall),
                offered(RemoteAction::Uninstall),
            )
            .destructive()
            .after_separator(),
            MenuItem::new("Remove Server\u{2026}", StatusStep::RemoveSsh, true),
        ];
        (buttons, menu)
    }

    fn direct_status_steps(
        &self,
        route: crate::direct_connections::DirectRouteId,
    ) -> (Vec<(StatusStep, bool)>, Vec<MenuItem>) {
        let Some((_, status, _)) = self
            .saved_direct_servers()
            .into_iter()
            .find(|(saved, _, _)| *saved == route)
        else {
            return (Vec::new(), Vec::new());
        };
        let presentation = crate::ui::direct_servers::present(status, "");
        let buttons = presentation
            .actions
            .iter()
            .map(|action| (StatusStep::Direct(*action), false))
            .collect();
        let reconnectable = !matches!(
            status,
            DirectRouteStatus::Connecting | DirectRouteStatus::Refused(RefusalReason::Revoked)
        );
        let unpair = if self.this_computer == "This Mac" {
            "Unpair This Mac\u{2026}"
        } else {
            "Unpair This Computer\u{2026}"
        };
        let menu = vec![
            MenuItem::new(
                "Reconnect",
                StatusStep::Direct(DirectServerAction::Connect),
                reconnectable,
            ),
            MenuItem::new(unpair, StatusStep::UnpairThisDevice, true)
                .destructive()
                .after_separator(),
        ];
        (buttons, menu)
    }

    /// Takes one step from the Status row or its menu.
    fn take_status_step(&mut self, step: StatusStep, window: &mut Window, cx: &mut Context<Self>) {
        let Some(choice) = self.selected_server() else {
            return;
        };
        match (step, choice) {
            (StatusStep::Local(ServiceAction::Install), ServerChoice::Local) => {
                self.confirm_local_installation(window, cx);
            }
            (StatusStep::Local(ServiceAction::Check), ServerChoice::Local) => {
                self.check_local_server(cx);
                self.check_server_service(cx);
                self.check_server_update(cx);
            }
            (StatusStep::Local(action), ServerChoice::Local) => {
                self.request_server_service(action, window, cx);
            }
            (StatusStep::Ssh(action), ServerChoice::Ssh(destination)) => {
                self.run_ssh_server_action(destination, action, window, cx);
            }
            (StatusStep::Maintenance(action), ServerChoice::Ssh(destination)) => {
                self.request_maintenance(destination, MaintenanceRow::Service, action, cx);
            }
            (StatusStep::RemoveSsh, ServerChoice::Ssh(destination)) => {
                self.confirm_remove_ssh_server(destination, window, cx);
            }
            (StatusStep::Direct(action), ServerChoice::Direct(route)) => {
                self.run_direct_action(route, action, cx);
            }
            (StatusStep::UnpairThisDevice, ServerChoice::Direct(_)) => {
                self.confirm_unpair_this_device(window, cx);
            }
            // A step offered for a server the selection has since left does nothing.
            _ => {}
        }
    }

    /// Asks before installing the one verified build this account's service would run. Several
    /// builds are chosen from the Install menu instead, which then asks the same question.
    fn confirm_local_installation(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(presentation) = self.server_service_presentation() else {
            return;
        };
        let [only] = presentation.install_choices.as_slice() else {
            return;
        };
        let directory = only.directory().to_path_buf();
        self.confirm_installing(only.label.clone(), directory, window, cx);
    }

    /// Asks before installing `build`, saying what installing does on this computer.
    fn confirm_installing(
        &mut self,
        build: SharedString,
        directory: std::path::PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let at_boot = self.this_computer != "This Mac";
        let owner = cx.weak_entity();
        let message = format!(
            "SpaceTerm installs {build} for your account and starts it. It keeps running after \
             SpaceTerm quits."
        );
        let detail = if at_boot {
            "If your computer needs an administrator password to start it at boot, SpaceTerm asks \
             you first."
        } else {
            "No administrator password is needed."
        };
        let result = spaceterm_ui::Alert::new(
            spaceterm_ui::ModalId::new("settings-server-install"),
            "Install SpaceTerm Server?",
            "Install SpaceTerm Server?",
            message,
            vec![
                spaceterm_ui::ModalAction::new(
                    true,
                    "Install",
                    spaceterm_ui::ModalActionRole::Affirmative,
                    "settings-server-install-confirm",
                )
                .with_emphasis(spaceterm_ui::ModalActionEmphasis::Prominent),
                spaceterm_ui::ModalAction::new(
                    false,
                    "Cancel",
                    spaceterm_ui::ModalActionRole::Cancel,
                    "settings-server-install-cancel",
                ),
            ],
        )
        .intent(spaceterm_ui::AlertIntent::Informational)
        .detail(detail)
        .present(window, cx, move |outcome, cx| {
            if matches!(
                outcome,
                spaceterm_ui::AlertOutcome::Activated {
                    action_id: true,
                    ..
                }
            ) {
                let _ = owner.update(cx, |settings, cx| {
                    settings.install_server_service(directory, cx);
                });
            }
        });
        if result.is_err() {
            eprintln!("failed to present the SpaceTerm server installation confirmation");
        }
    }

    /// Asks before forgetting a saved SSH connection. The server, its Terminal Sessions and the
    /// account's SSH access stay.
    fn confirm_remove_ssh_server(
        &mut self,
        destination: crate::domain::SshDestination,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let owner = cx.weak_entity();
        let name = destination.as_str().to_owned();
        let title = format!("Remove \u{201C}{name}\u{201D}?");
        let result = spaceterm_ui::Alert::new(
            spaceterm_ui::ModalId::new("settings-server-remove"),
            title.clone(),
            title,
            format!(
                "SpaceTerm forgets this connection and stops reconnecting to it. The server on \
                 \u{201C}{name}\u{201D} and its Terminal Sessions keep running."
            ),
            vec![
                spaceterm_ui::ModalAction::new(
                    true,
                    "Remove",
                    spaceterm_ui::ModalActionRole::Affirmative,
                    "settings-server-remove-confirm",
                )
                .with_intent(spaceterm_ui::ModalActionIntent::Destructive)
                .with_emphasis(spaceterm_ui::ModalActionEmphasis::Prominent),
                spaceterm_ui::ModalAction::new(
                    false,
                    "Cancel",
                    spaceterm_ui::ModalActionRole::Cancel,
                    "settings-server-remove-cancel",
                ),
            ],
        )
        .intent(spaceterm_ui::AlertIntent::Critical)
        .detail("Your SSH access stays. To use the server again, add it again.")
        .present(window, cx, move |outcome, cx| {
            if !matches!(
                outcome,
                spaceterm_ui::AlertOutcome::Activated {
                    action_id: true,
                    ..
                }
            ) {
                return;
            }
            let _ = owner.update(cx, |settings, cx| {
                settings.remove_ssh_server(&destination, cx);
            });
        });
        if result.is_err() {
            eprintln!("failed to present the remove server confirmation");
        }
    }

    /// Forgets the saved connection at once. The selection then presents this computer's server,
    /// since the removed one is no longer listed.
    fn remove_ssh_server(
        &mut self,
        destination: &crate::domain::SshDestination,
        cx: &mut Context<Self>,
    ) {
        let Some(rows) = self.ssh_servers.as_ref() else {
            return;
        };
        rows.connections.remove(destination);
        self.reconcile_servers(cx);
        self.sync_server_location(cx);
        cx.notify();
    }
}

/// When the shown values were observed, in this computer's local time: "08:15" today, or
/// "Oct 21, 08:15" on another day.
pub(super) fn observed_time(at: u64) -> String {
    let now = crate::ui::direct_access::unix_now();
    match (local_time(at), local_time(now)) {
        (Some(at), Some(now)) => clock_text(at, now),
        _ => "earlier".to_owned(),
    }
}

/// A local calendar time: year, month from 0, day of the month, hour and minute.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct LocalTime {
    year: i32,
    month: usize,
    day: u32,
    hour: u32,
    minute: u32,
}

fn local_time(unix_seconds: u64) -> Option<LocalTime> {
    let seconds = libc::time_t::try_from(unix_seconds).ok()?;
    // SAFETY: `tm` is a plain C struct that localtime_r fully initializes on success, and both
    // pointers stay valid for the call.
    let tm = unsafe {
        let mut tm: libc::tm = std::mem::zeroed();
        if libc::localtime_r(&seconds, &mut tm).is_null() {
            return None;
        }
        tm
    };
    Some(LocalTime {
        year: tm.tm_year,
        month: usize::try_from(tm.tm_mon).ok()?,
        day: u32::try_from(tm.tm_mday).ok()?,
        hour: u32::try_from(tm.tm_hour).ok()?,
        minute: u32::try_from(tm.tm_min).ok()?,
    })
}

pub(super) fn clock_text(at: LocalTime, now: LocalTime) -> String {
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let clock = format!("{:02}:{:02}", at.hour, at.minute);
    if (at.year, at.month, at.day) == (now.year, now.month, now.day) {
        clock
    } else {
        let month = MONTHS.get(at.month).copied().unwrap_or("");
        format!("{month} {}, {clock}", at.day)
    }
}

/// How the Version row reads.
struct VersionView {
    /// The running version, or what SpaceTerm last observed.
    value: Option<SharedString>,
    badge: Option<SharedString>,
    description: SharedString,
    /// Work underway, named by its verb.
    working: Option<SharedString>,
    actions: Vec<VersionStep>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum VersionStep {
    Local(UpdateAction),
    Remote(RemoteAction),
    /// Check Now for a server with no update journey here, such as a paired server or a
    /// Development run: SpaceTerm checks for its own update and reads the server's facts again.
    Observe,
}

/// Check Now in progress for one server. It runs until SpaceTerm's own check settles and the
/// server it was asked for has been read again.
pub(super) struct VersionCheck {
    target: ServerChoice,
    _check: gpui::Task<()>,
}

/// The selector of Check Now where the server has no update journey of its own here.
pub(super) const VERSION_CHECK_SELECTOR: &str = "settings-server-version-check";
/// How often Check Now looks again at work it waits for.
const CHECK_POLL: std::time::Duration = std::time::Duration::from_millis(200);

impl VersionStep {
    const fn label(self) -> &'static str {
        match self {
            Self::Local(action) => action.label(),
            Self::Remote(action) => action.label(),
            Self::Observe => "Check Now",
        }
    }

    const fn selector(self) -> &'static str {
        match self {
            Self::Local(action) => action.selector(),
            Self::Remote(action) => action.selector(),
            Self::Observe => VERSION_CHECK_SELECTOR,
        }
    }

    /// The step that changes the server leads, as the primary button.
    const fn primary(self) -> bool {
        matches!(
            self,
            Self::Local(UpdateAction::Update | UpdateAction::Finish)
                | Self::Remote(RemoteAction::Update | RemoteAction::Finish)
        )
    }
}

impl SettingsWindow {
    fn version_view(&self, cx: &App) -> VersionView {
        let mut view = self.journey_view(cx);
        // Check Now reads as checking from SpaceTerm's own check to the server's read after it.
        if self
            .version_check
            .as_ref()
            .is_some_and(|check| Some(&check.target) == self.selected_server().as_ref())
        {
            view.working = Some(SharedString::new_static("Checking\u{2026}"));
        }
        view
    }

    /// The Version row as the selected server's update journey presents it.
    fn journey_view(&self, cx: &App) -> VersionView {
        let observed = self
            .selected_facts(cx)
            .and_then(|facts| facts.version)
            .map(SharedString::from);
        let Some(choice) = self.selected_server() else {
            return VersionView {
                value: None,
                badge: None,
                description: SharedString::default(),
                working: None,
                actions: Vec::new(),
            };
        };
        let needs_update = self.server_state(&choice).state.as_ref() == "Needs update";
        match choice {
            ServerChoice::Local => self.local_version_view(observed, needs_update),
            ServerChoice::Ssh(destination) => {
                self.ssh_version_view(&destination, observed, needs_update)
            }
            // A paired connection carries no authority to update the server, so Check Now only
            // checks SpaceTerm and reads the server's facts again over that connection.
            ServerChoice::Direct(_) => VersionView {
                value: observed,
                badge: needs_update.then(|| SharedString::new_static("Needs update")),
                description: SharedString::new_static("Update it on its own computer or over SSH."),
                working: None,
                actions: vec![VersionStep::Observe],
            },
        }
    }

    fn local_version_view(
        &self,
        observed: Option<SharedString>,
        needs_update: bool,
    ) -> VersionView {
        let Some(row) = self.server_update.as_ref() else {
            return VersionView {
                value: observed,
                badge: None,
                description: SharedString::new_static(
                    "This run builds its own server, so it has no update.",
                ),
                working: None,
                actions: vec![VersionStep::Observe],
            };
        };
        let presentation = row.presentation();
        let facts = row.state.facts.as_ref();
        let value = facts
            .and_then(|facts| facts.approved.as_ref())
            .map(|approved| SharedString::from(approved.version.clone()))
            .or(observed);
        let included = facts
            .and_then(|facts| facts.included.as_ref().ok())
            .map(|included| included.version.clone());
        let working = row
            .state
            .working
            .map(|step| SharedString::new_static(step.verb()));
        let offered = |action: UpdateAction| presentation.actions.contains(&action);
        let (badge, description, actions): (Option<SharedString>, &str, Vec<UpdateAction>) =
            if offered(UpdateAction::Return) {
                (
                    Some("Update failed".into()),
                    "The update didn\u{2019}t finish, and its Terminal Sessions ended.",
                    vec![UpdateAction::Return, UpdateAction::Check],
                )
            } else if offered(UpdateAction::Finish) {
                (
                    Some("Update unfinished".into()),
                    "An update you confirmed hasn\u{2019}t finished.",
                    vec![UpdateAction::Finish],
                )
            } else if offered(UpdateAction::Update) && needs_update {
                (
                    Some("Needs update".into()),
                    "SpaceTerm can\u{2019}t use this server until it\u{2019}s updated.",
                    vec![UpdateAction::Update],
                )
            } else if offered(UpdateAction::Update) {
                (
                    Some(match &included {
                        Some(version) => format!("{version} available").into(),
                        None => "Update available".into(),
                    }),
                    "Updating ends its Terminal Sessions for every client.",
                    vec![UpdateAction::Update],
                )
            } else if presentation.state_selector == "settings-server-update-state-current" {
                (
                    Some("Up to date".into()),
                    "Server updates come with SpaceTerm.",
                    vec![UpdateAction::Check],
                )
            } else if presentation.state_selector == "settings-server-update-state-not-installed" {
                (None, "Install the server to see its version.", Vec::new())
            } else {
                (
                    Some(presentation.state.into()),
                    "Server updates come with SpaceTerm.",
                    vec![UpdateAction::Check],
                )
            };
        VersionView {
            value,
            badge,
            description: SharedString::from(description.to_owned()),
            working,
            actions: actions.into_iter().map(VersionStep::Local).collect(),
        }
    }

    fn ssh_version_view(
        &self,
        destination: &crate::domain::SshDestination,
        observed: Option<SharedString>,
        needs_update: bool,
    ) -> VersionView {
        let presentation = self.maintenance_presentation(MaintenanceRow::Update);
        let reported = self
            .ssh_servers
            .as_ref()
            .and_then(|rows| rows.connections.last_facts(destination))
            .and_then(|facts| facts.version)
            .map(SharedString::from);
        let value = reported.or(observed);
        let Some(presentation) = presentation else {
            return VersionView {
                value,
                badge: needs_update.then(|| SharedString::new_static("Needs update")),
                description: SharedString::new_static("Server updates come with SpaceTerm."),
                working: None,
                actions: Vec::new(),
            };
        };
        let offered = |action: RemoteAction| presentation.actions.contains(&action);
        // Maintenance work names its own verb, as the Status row does.
        let working = presentation.working.then(|| {
            self.server_state(&ServerChoice::Ssh(destination.clone()))
                .state
        });
        let (badge, description): (Option<SharedString>, &str) = if offered(RemoteAction::Return) {
            (
                Some("Update failed".into()),
                "The update didn\u{2019}t finish, and its Terminal Sessions ended.",
            )
        } else if offered(RemoteAction::Finish) {
            (
                Some("Update unfinished".into()),
                "An update you confirmed hasn\u{2019}t finished.",
            )
        } else if offered(RemoteAction::Update) && needs_update {
            (
                Some("Needs update".into()),
                "SpaceTerm can\u{2019}t use this server until it\u{2019}s updated.",
            )
        } else if offered(RemoteAction::Update) {
            (
                Some(format!("{} available", env!("SPACETERM_VERSION")).into()),
                "Updating ends its Terminal Sessions for every client.",
            )
        } else if presentation.state == Some("Up to Date") {
            (
                Some("Up to date".into()),
                "Server updates come with SpaceTerm.",
            )
        } else {
            (
                needs_update.then(|| SharedString::new_static("Needs update")),
                "Server updates come with SpaceTerm.",
            )
        };
        let mut actions: Vec<VersionStep> = presentation
            .actions
            .iter()
            .copied()
            .filter(|action| {
                matches!(
                    action,
                    RemoteAction::Update
                        | RemoteAction::Finish
                        | RemoteAction::Return
                        | RemoteAction::CheckAgain
                        | RemoteAction::Cancel
                        | RemoteAction::Acknowledge
                        | RemoteAction::Discard
                        | RemoteAction::Dismiss
                )
            })
            .map(VersionStep::Remote)
            .collect();
        // Check Now reads the service and its approved version, whichever journey offers it.
        let checkable = presentation.actions.contains(&RemoteAction::Check)
            || self
                .maintenance_presentation(MaintenanceRow::Service)
                .is_some_and(|service| service.actions.contains(&RemoteAction::Check));
        if checkable {
            actions.push(VersionStep::Remote(RemoteAction::Check));
        }
        VersionView {
            value,
            badge,
            description: SharedString::from(description.to_owned()),
            working,
            actions,
        }
    }

    /// The Version row's description.
    pub(super) fn version_description(&self, cx: &App) -> SharedString {
        self.version_view(cx).description
    }

    /// Checks for a newer SpaceTerm, which brings each server's build, then reads the server it
    /// was asked for again once that check settles. Reading connects to, starts and installs
    /// nothing: this account's service and an SSH account's route are read as Check Status reads
    /// them, and a paired server only over a connection SpaceTerm already has.
    fn check_versions(&mut self, cx: &mut Context<Self>) {
        let Some(target) = self.selected_server() else {
            return;
        };
        if self
            .version_check
            .as_ref()
            .is_some_and(|check| check.target == target)
        {
            return;
        }
        let updates = cx
            .try_global::<crate::updates::UpdateService>()
            .map(|service| service.0.clone());
        if let Some(updates) = &updates {
            updates.update(cx, |updates, cx| updates.check(true, cx));
        }
        let checked = target.clone();
        let check = cx.spawn(async move |settings, cx| {
            if let Some(updates) = updates {
                while cx.update(|cx| {
                    matches!(
                        updates.read(cx).state(),
                        crate::updates::UpdateState::Checking
                    )
                }) {
                    cx.background_executor().timer(CHECK_POLL).await;
                }
            }
            let facts = settings
                .update(cx, |settings, cx| settings.reread_server(&checked, cx))
                .ok()
                .flatten();
            if let Some(facts) = facts {
                facts.await;
            }
            while settings
                .update(cx, |settings, _| settings.server_reading(&checked))
                .unwrap_or(false)
            {
                cx.background_executor().timer(CHECK_POLL).await;
            }
            let _ = settings.update(cx, |settings, cx| {
                if settings
                    .version_check
                    .as_ref()
                    .is_some_and(|check| check.target == checked)
                {
                    settings.version_check = None;
                }
                cx.notify();
            });
        });
        self.version_check = Some(VersionCheck {
            target,
            _check: check,
        });
        cx.notify();
    }

    /// Reads `target` again as Check Now does, and returns the read of its own facts when one runs
    /// over an existing connection.
    fn reread_server(
        &mut self,
        target: &ServerChoice,
        cx: &mut Context<Self>,
    ) -> Option<gpui::Task<()>> {
        match target {
            ServerChoice::Local => {
                if self.server_update.is_some() {
                    self.request_update_action(UpdateAction::Check, cx);
                }
                self.check_local_server(cx);
            }
            ServerChoice::Ssh(destination) => {
                self.request_remote_maintenance(destination.clone(), RemoteAction::Check, cx);
            }
            ServerChoice::Direct(_) => {}
        }
        self.reread_facts(target, cx)
    }

    /// Whether a read Check Now started for `target` is still underway.
    fn server_reading(&self, target: &ServerChoice) -> bool {
        match target {
            ServerChoice::Local => {
                self.server_update
                    .as_ref()
                    .is_some_and(|row| row.state.working.is_some())
                    || self.local_server_working()
            }
            ServerChoice::Ssh(_) if self.selected_server().as_ref() == Some(target) => {
                [MaintenanceRow::Service, MaintenanceRow::Update]
                    .into_iter()
                    .filter_map(|row| self.maintenance_presentation(row))
                    .any(|presentation| presentation.working)
            }
            _ => false,
        }
    }

    fn take_version_step(&mut self, step: VersionStep, cx: &mut Context<Self>) {
        match (step, self.selected_server()) {
            (VersionStep::Local(UpdateAction::Check), _)
            | (VersionStep::Remote(RemoteAction::Check), _)
            | (VersionStep::Observe, _) => self.check_versions(cx),
            (VersionStep::Local(action), Some(ServerChoice::Local)) => {
                self.request_update_action(action, cx)
            }
            (VersionStep::Remote(action), Some(ServerChoice::Ssh(destination))) => {
                self.request_maintenance(destination, MaintenanceRow::Update, action, cx)
            }
            _ => {}
        }
    }

    /// The Status row: a dot and the state, the steps it allows, and a ⋯ menu.
    pub(super) fn render_server_status(
        &mut self,
        appearance: &ChromeAppearance,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(choice) = self.selected_server() else {
            return div().into_any_element();
        };
        let name = self.server_name(&choice);
        let state = self.server_state(&choice);
        let colors = appearance.host_colors(spaceterm_ui::ControlHost::Card);
        let indicator = if state.working {
            ProgressRing::new(
                SharedString::from(format!("{STATUS_SELECTOR}-progress")),
                state.state.clone(),
                ProgressState::Indeterminate,
            )
            .size(ProgressSize::Compact)
            .debug_selector(format!("{STATUS_SELECTOR}-progress"))
            .into_any_element()
        } else {
            status_dot(tone_color(state.tone, colors), appearance.spacing(8.0)).into_any_element()
        };
        // Assistive technology announces the state when it changes, as a status.
        let label = div()
            .id(SharedString::from(format!("{STATUS_SELECTOR}-state")))
            .debug_selector(|| format!("{STATUS_SELECTOR}-state"))
            .role(accesskit::Role::Status)
            .aria_label(SharedString::from(format!("{name}: {}", state.state)))
            .flex()
            .flex_row()
            .items_center()
            .gap(appearance.spacing(6.0))
            .child(indicator)
            .child(
                div()
                    .chrome_text(appearance.typography.style(TextRole::Body))
                    .text_color(gpui_color(colors.text_secondary))
                    .child(state.state.clone()),
            );
        let (buttons, menu) = self.status_steps(&choice);
        let install_choices = self
            .server_service_presentation()
            .map(|presentation| presentation.install_choices)
            .unwrap_or_default();
        let owner = cx.weak_entity();
        let buttons = buttons
            .into_iter()
            .enumerate()
            .map(|(index, (step, primary))| {
                let owner = owner.clone();
                let selector = format!("{STATUS_SELECTOR}-step-{index}");
                // Several verified builds make Install a menu of them, so the person names the build
                // before SpaceTerm asks to install it.
                if step == StatusStep::Local(ServiceAction::Install) && install_choices.len() > 1 {
                    let entries = install_choices
                        .iter()
                        .enumerate()
                        .map(|(position, choice)| {
                            MenuEntry::action(
                                choice.label.clone(),
                                (choice.label.clone(), choice.directory().to_path_buf()),
                            )
                            .debug_selector(format!(
                                "{}-{position}",
                                super::servers::INSTALL_CHOICE_SELECTOR
                            ))
                        })
                        .collect();
                    return Menu::new(
                        ElementId::from(SharedString::from(selector.clone())),
                        "Install\u{2026}",
                        entries,
                    )
                    .button_trigger(
                        ButtonVariant::Primary,
                        ButtonSize::Regular,
                        spaceterm_ui::ButtonShape::Rounded,
                    )
                    .debug_selector(status_step_selector(&step))
                    .on_activate(move |activation, window, cx| {
                        let (build, directory) = activation.action().clone();
                        let _ = owner.update(cx, |settings, cx| {
                            settings.confirm_installing(build, directory, window, cx);
                        });
                    })
                    .into_any_element();
                }
                let label = status_step_label(&step);
                // Steps that replace each other share a slot, so keyboard focus stays on it when,
                // say, Stop gives way to Start.
                let slot = status_step_slot(&step).map_or_else(|| selector.clone(), str::to_owned);
                let selector = status_step_selector(&step).to_owned();
                let single_build = match (&step, install_choices.as_slice()) {
                    (StatusStep::Local(ServiceAction::Install), [only]) => {
                        Some(SharedString::from(format!("Installs {}.", only.label)))
                    }
                    _ => None,
                };
                let description = single_build.unwrap_or_else(|| {
                    status_step_consequence(&step).map_or_else(
                        || {
                            SharedString::from(format!(
                                "{} {name}",
                                label.trim_end_matches('\u{2026}')
                            ))
                        },
                        SharedString::new_static,
                    )
                });
                Button::new(ElementId::from(SharedString::from(slot)), label)
                    .variant(if primary {
                        ButtonVariant::Primary
                    } else {
                        ButtonVariant::Outline
                    })
                    .size(ButtonSize::Regular)
                    .tab_stop(true)
                    .debug_selector(selector)
                    .accessibility_description(description)
                    .on_activate(move |_, window, cx| {
                        let step = step.clone();
                        let _ = owner.update(cx, |settings, cx| {
                            settings.take_status_step(step, window, cx);
                        });
                    })
                    .into_any_element()
            });
        let menu = (!menu.is_empty()).then(|| {
            let mut entries = Vec::new();
            for item in menu {
                if item.separator_before {
                    entries.push(MenuEntry::separator());
                }
                let selector = status_step_selector(&item.step);
                entries.push(
                    MenuEntry::action(item.label, item.step)
                        .disabled(!item.enabled)
                        .destructive(item.destructive)
                        .debug_selector(selector),
                );
            }
            let owner = cx.weak_entity();
            let icon_size = appearance.icons.metrics(IconRole::Control).glyph_size;
            Menu::new(
                MENU_SELECTOR,
                SharedString::from(format!("More for {name}")),
                entries,
            )
            .icon_trigger(move |foreground| {
                Icon::new(IconName::Ellipsis, icon_size, foreground).into_any_element()
            })
            .placement(AnchoredPlacementConfig::new(
                AnchoredPlacement::Bottom,
                AnchoredAlignment::End,
            ))
            .debug_selector(MENU_SELECTOR)
            .on_activate(move |activation, window, cx| {
                let step = activation.action().clone();
                // The confirmation a step asks opens after the menu closes.
                let owner = owner.clone();
                window.defer(cx, move |window, cx| {
                    let _ = owner.update(cx, |settings, cx| {
                        settings.take_status_step(step, window, cx);
                    });
                });
            })
        });
        div()
            .debug_selector(|| STATUS_SELECTOR.to_owned())
            .flex()
            .flex_row()
            .items_center()
            .gap(appearance.spacing(8.0))
            .child(label)
            .children(buttons)
            .children(menu)
            .into_any_element()
    }

    /// The Version row: the running version, a badge, and Check Now or the update's next step.
    pub(super) fn render_server_version(
        &mut self,
        appearance: &ChromeAppearance,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let view = self.version_view(cx);
        let colors = appearance.host_colors(spaceterm_ui::ControlHost::Card);
        let owner = cx.weak_entity();
        let value = view.value.clone().unwrap_or_else(|| "Unknown".into());
        let busy = view.working.is_some();
        let working = view.working.clone().map(|verb| {
            ProgressRing::new(
                SharedString::from(format!("{VERSION_SELECTOR}-progress")),
                verb,
                ProgressState::Indeterminate,
            )
            .size(ProgressSize::Compact)
            .debug_selector(format!("{VERSION_SELECTOR}-progress"))
            .into_any_element()
        });
        let machine = self.selected_server_name();
        let buttons = view.actions.into_iter().map(|step| {
            let owner = owner.clone();
            let selector = step.selector().to_owned();
            let description = match step {
                VersionStep::Local(action) => action.consequence().map(SharedString::new_static),
                VersionStep::Remote(action) => Some(action.consequence(&machine)),
                VersionStep::Observe => Some(SharedString::from(format!(
                    "Checks for a newer SpaceTerm, then reads {machine} again. It connects to \
                     nothing."
                ))),
            };
            Button::new(
                ElementId::from(SharedString::from(selector.clone())),
                step.label(),
            )
            .variant(if step.primary() {
                ButtonVariant::Primary
            } else {
                ButtonVariant::Outline
            })
            .size(ButtonSize::Regular)
            .tab_stop(true)
            // One step runs at a time. The steps act through this account's service or an SSH
            // account's route, so they need no connection to the server.
            .disabled(busy)
            .when_some(description, |button, description| {
                button.accessibility_description(description)
            })
            .debug_selector(selector)
            .on_activate(move |_, _, cx| {
                let _ = owner.update(cx, |settings, cx| settings.take_version_step(step, cx));
            })
            .into_any_element()
        });
        div()
            .debug_selector(|| VERSION_SELECTOR.to_owned())
            .flex()
            .flex_row()
            .items_center()
            .gap(appearance.spacing(8.0))
            .child(
                div()
                    .id(SharedString::from(format!("{VERSION_SELECTOR}-value")))
                    .role(accesskit::Role::Status)
                    .aria_label(SharedString::from(match (&view.badge, &view.working) {
                        (_, Some(verb)) => format!("Version {value}, {verb}"),
                        (Some(badge), None) => format!("Version {value}, {badge}"),
                        (None, None) => format!("Version {value}"),
                    }))
                    .chrome_text(appearance.typography.style(TextRole::Body))
                    .text_color(gpui_color(colors.text_secondary))
                    .child(value.clone()),
            )
            .children(working)
            .children(view.badge.map(|label| {
                badge(
                    SharedString::from(format!("{VERSION_SELECTOR}-badge")),
                    label,
                    appearance,
                )
            }))
            .children(buttons)
            .into_any_element()
    }

    /// Whether the selected server starts on its own, as it reports or as last observed.
    pub(super) fn automatic_start(&self, cx: &App) -> Option<bool> {
        match self.selected_server()? {
            ServerChoice::Local => self
                .server_service
                .as_ref()
                .and_then(|row| row.state.status)
                .filter(|status| status.installed)
                .map(|status| status.enabled),
            ServerChoice::Ssh(destination) => self
                .ssh_maintenance
                .as_ref()
                .and_then(|rows| {
                    rows.operations
                        .entries()
                        .into_iter()
                        .find(|entry| entry.destination == destination && !entry.removed)
                })
                .and_then(|entry| match entry.state {
                    RemoteOperationState::Read(facts) => Some(facts.status.enabled),
                    _ => None,
                })
                .or_else(|| {
                    self.ssh_servers
                        .as_ref()
                        .and_then(|rows| rows.connections.last_facts(&destination))
                        .map(|facts| facts.starts_automatically)
                })
                .or_else(|| self.selected_facts(cx)?.starts_automatically),
            ServerChoice::Direct(_) => None,
        }
    }

    /// The Start automatically row's description, naming when the server starts.
    pub(super) fn auto_start_description(&self) -> SharedString {
        match self.selected_server() {
            Some(ServerChoice::Ssh(destination)) => SharedString::from(format!(
                "Start the server when {} starts.",
                destination.as_str()
            )),
            _ if self.this_computer == "This Mac" => {
                SharedString::new_static("Start the server when you log in.")
            }
            _ => SharedString::new_static("Start the server when this computer starts, at boot."),
        }
    }

    /// Why the last change to automatic start didn't take effect, under its switch.
    pub(super) fn auto_start_error(&self) -> Option<SharedString> {
        match self.selected_server()? {
            ServerChoice::Local => self
                .server_service
                .as_ref()?
                .automatic_start
                .failure
                .map(|error| SharedString::from(error.to_string())),
            // A change the person asked for that the account route refused, or whose answer was
            // lost, reads as the maintenance read reports it.
            ServerChoice::Ssh(destination) => {
                let asked = self.ssh_automatic_start.as_ref()?;
                if asked.0 != destination {
                    return None;
                }
                self.maintenance_presentation(MaintenanceRow::Service)
                    .filter(|maintenance| maintenance.tone == CaptionTone::Error)
                    .map(|maintenance| maintenance.explanation)
            }
            ServerChoice::Direct(_) => None,
        }
    }

    /// Whether the selected server's automatic start can change now: this account's installed
    /// service, or an SSH account's route while its maintenance owner is idle. A paired
    /// connection carries no such authority.
    fn automatic_start_changeable(&self, cx: &App) -> bool {
        if self.automatic_start(cx).is_none() {
            return false;
        }
        match self.selected_server() {
            Some(ServerChoice::Local) => self
                .server_service
                .as_ref()
                .is_some_and(|row| !row.automatic_start.working && row.state.working.is_none()),
            Some(ServerChoice::Ssh(_)) => self
                .maintenance_presentation(MaintenanceRow::Service)
                .is_some_and(|maintenance| !maintenance.working),
            _ => false,
        }
    }

    /// Asks the selected server to start on its own, or not. This account's service changes at
    /// once; an SSH account's route changes over the route its last read bound. A deliberate Stop
    /// stays in force either way.
    fn set_automatic_start(&mut self, enabled: bool, window: &mut Window, cx: &mut Context<Self>) {
        match self.selected_server() {
            Some(ServerChoice::Local) => self.set_local_automatic_start(enabled, window, cx),
            Some(ServerChoice::Ssh(destination)) => {
                let Some(rows) = self.ssh_maintenance.as_ref() else {
                    return;
                };
                rows.operations.set_automatic_start(&destination, enabled);
                self.ssh_automatic_start = Some((destination, enabled));
                cx.notify();
            }
            _ => {}
        }
    }

    /// The Start automatically switch. It controls automatic start only; Uninstall stays in the
    /// Status menu.
    pub(super) fn render_auto_start(
        &mut self,
        _: &ChromeAppearance,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let value = self.automatic_start(cx);
        let changeable = self.automatic_start_changeable(cx);
        let owner = cx.weak_entity();
        Switch::new(
            AUTO_START_SELECTOR,
            "Start automatically",
            value.unwrap_or(false),
        )
        .size(ToggleSize::Regular)
        .label_hidden(true)
        .disabled(!changeable)
        .debug_selector(AUTO_START_SELECTOR)
        .on_change(move |change, window, cx| {
            let enabled = change.requested();
            let _ = owner.update(cx, |settings, cx| {
                settings.set_automatic_start(enabled, window, cx);
            });
        })
        .into_any_element()
    }
}

/// The selector a test addresses a Status step by.
fn status_step_selector(step: &StatusStep) -> &'static str {
    match step {
        StatusStep::Local(action) => action.selector(),
        StatusStep::Ssh(action) => action.selector(),
        StatusStep::Maintenance(action) => action.selector(),
        StatusStep::Direct(action) => action.selector(),
        StatusStep::RemoveSsh => "settings-server-remove",
        StatusStep::UnpairThisDevice => "settings-server-unpair",
    }
}

/// The element a Status step occupies, shared by steps that replace one another.
fn status_step_slot(step: &StatusStep) -> Option<&'static str> {
    match step {
        StatusStep::Local(action) => Some(action.slot()),
        _ => None,
    }
}

/// What a Status step does beyond its name, for assistive technology.
fn status_step_consequence(step: &StatusStep) -> Option<&'static str> {
    match step {
        StatusStep::Local(action) => action.consequence(),
        _ => None,
    }
}

/// Whether a maintenance row reports work underway or a result that waits for the person.
fn maintenance_waits(
    presentation: &crate::ui::remote_maintenance::MaintenancePresentation,
) -> bool {
    presentation.working
        || presentation.actions.iter().any(|action| {
            matches!(
                action,
                RemoteAction::Acknowledge
                    | RemoteAction::Discard
                    | RemoteAction::Dismiss
                    | RemoteAction::CheckAgain
                    | RemoteAction::Finish
                    | RemoteAction::Cancel
            )
        })
}

/// The label a Status step reads by.
fn status_step_label(step: &StatusStep) -> &'static str {
    match step {
        StatusStep::Local(ServiceAction::Install) => "Install\u{2026}",
        StatusStep::Local(ServiceAction::Check) => "Check Status",
        StatusStep::Local(action) => action.label(),
        StatusStep::Ssh(SshServerAction::Install) => "Install\u{2026}",
        StatusStep::Ssh(action) => action.label(),
        StatusStep::Maintenance(action) => action.label(),
        StatusStep::Direct(action) => action.label(),
        StatusStep::RemoveSsh => "Remove Server\u{2026}",
        StatusStep::UnpairThisDevice => "Unpair\u{2026}",
    }
}

#[cfg(test)]
#[path = "server_card_tests.rs"]
mod tests;
