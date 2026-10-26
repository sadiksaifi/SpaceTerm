//! The selected server's card: the Server row, Start automatically and Terminal Sessions.
//!
//! Each row reads what SpaceTerm already knows: this account's service and update rows, and the
//! direct registry. Rendering asks no server anything. A paired server's native service is
//! administered on its own machine, so its card offers only connection steps. When SpaceTerm isn't connected to the server, the rows under Server dim and show what
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
use super::server_update::{UpdateAction, UpdateState, UpdateStep};
use super::servers::{LocalServerStatus, ServiceAction, ServicePhase};
use crate::desktop_profile::LocalTime;
use crate::direct_connections::{DirectRouteStatus, RefusalReason};
use crate::server_connection::ServerError;
use crate::server_context::{LastKnownFacts, ObservedServerStatus};
use crate::ui::appearance::{ChromeAppearance, gpui_color};
use crate::ui::chrome_icons::IconRole;
use crate::ui::chrome_typography::{ChromeTextStyleExt as _, TextRole};
use crate::ui::direct_servers::DirectServerAction;
use crate::ui::sidebar_window::form::CaptionTone;

pub(super) const STATUS_SELECTOR: &str = "settings-server-status";
pub(super) const MENU_SELECTOR: &str = "settings-server-menu";
/// The spinner leading the Server row's subtitle while it names work underway.
pub(super) const PROGRESS_SELECTOR: &str = "settings-server-progress";
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
    /// What the Server row's state says, such as "Connected over the network" or "Stopped".
    pub(in crate::ui::settings_window) state: SharedString,
    /// What the picker and All Servers say, such as "Connected · Paired".
    pub(in crate::ui::settings_window) short: SharedString,
    pub(in crate::ui::settings_window) tone: Tone,
    /// The connection is changing, such as while connecting, so a spinner stands in for the dot.
    pub(in crate::ui::settings_window) working: bool,
    /// SpaceTerm is connected to the server, so its settings can change.
    pub(in crate::ui::settings_window) live: bool,
    /// A read of the server is underway. The state stays as last observed, and the Server row
    /// says Checking only once the read runs longer than a second.
    pub(in crate::ui::settings_window) checking: bool,
    /// The verb of an operation underway that changes the server, such as "Updating…". The
    /// Server row keeps the state and names the operation under its label, and All Servers names
    /// it for a server that isn't selected.
    pub(in crate::ui::settings_window) operation: Option<&'static str>,
}

impl ServerState {
    fn new(state: impl Into<SharedString>, short: impl Into<SharedString>, tone: Tone) -> Self {
        Self {
            state: state.into(),
            short: short.into(),
            tone,
            working: false,
            live: false,
            checking: false,
            operation: None,
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
            checking: false,
            operation: None,
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

/// How this account's server reads. `service` is `None` where no service runs it, such as a
/// Development run. A connection refused for want of a protocol in common needs an update. Work
/// underway leaves the state as last observed and names itself apart from it.
pub(super) fn local_state(
    connection: LocalServerStatus,
    service: Option<(Option<ServicePhase>, Option<ServiceAction>, bool)>,
    updating: bool,
) -> ServerState {
    let working = service.and_then(|(_, working, _)| working);
    let operation = if updating {
        Some("Updating\u{2026}")
    } else {
        working
            .filter(|action| *action != ServiceAction::Check)
            .map(service_verb)
    };
    ServerState {
        checking: working == Some(ServiceAction::Check),
        operation,
        ..local_observed(connection, service)
    }
}

/// How this account's server reads from what SpaceTerm last observed of it.
fn local_observed(
    connection: LocalServerStatus,
    service: Option<(Option<ServicePhase>, Option<ServiceAction>, bool)>,
) -> ServerState {
    let connected = matches!(connection, LocalServerStatus::Connected(_));
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

/// A step the Server row or its menu can take.
#[derive(Clone, Debug, Eq, PartialEq)]
enum StatusStep {
    Local(ServiceAction),
    Direct(DirectServerAction),
    UnpairThisDevice,
    /// A step of the server's update journey, or Check Now.
    Version(VersionStep),
    /// Asks before installing one of several verified builds, named by its label.
    InstallBuild(SharedString, std::path::PathBuf),
}

/// One entry of the Server row's ⋯ menu.
struct MenuItem {
    label: SharedString,
    /// The selector a test addresses the entry by.
    selector: SharedString,
    step: StatusStep,
    enabled: bool,
    destructive: bool,
    separator_before: bool,
}

impl MenuItem {
    fn new(label: impl Into<SharedString>, step: StatusStep, enabled: bool) -> Self {
        Self {
            label: label.into(),
            selector: status_step_selector(&step).into(),
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
                let update_step = self
                    .server_update
                    .as_ref()
                    .and_then(|row| row.state.working);
                let updating = update_step.is_some_and(|step| step.changes_service());
                let mut state = local_state(connection, service, updating);
                // Reading the connection or the version leaves the state as last observed too.
                state.checking |= self.local_server_working()
                    || update_step == Some(UpdateStep::Check)
                    || self.version_checking(choice);
                let failed = self.server_update_presentation().is_some_and(|update| {
                    update.state_selector == "settings-server-update-state-failed"
                });
                if failed && state.operation.is_none() {
                    state.state = "Update failed".into();
                    state.short = "Update failed".into();
                    state.tone = Tone::Problem;
                }
                state
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

    /// "machine · OS" for this computer, "address · OS" for paired servers. A
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
                ServerChoice::Local => SharedString::new_static("This account"),
                ServerChoice::Direct(_) => SharedString::new_static("Paired over network"),
            },
        }
    }

    /// Whether SpaceTerm is connected to the selected server now.
    pub(in crate::ui::settings_window) fn selected_server_live(&self) -> bool {
        self.selected_server()
            .is_some_and(|choice| self.server_state(&choice).live)
    }

    /// The selected server's card footnote: what its state or update explains at length, and,
    /// while SpaceTerm isn't connected, how to change its settings and when the shown values were
    /// observed. The Server row itself keeps one line, so its height never changes.
    pub(super) fn server_card_footnote(&self, cx: &App) -> Option<SharedString> {
        let choice = self.selected_server()?;
        let mut parts: Vec<SharedString> = Vec::new();
        for (text, _) in [self.status_message(), self.version_message()]
            .into_iter()
            .flatten()
        {
            if !parts.contains(&text) {
                parts.push(text);
            }
        }
        parts.extend(self.offline_guidance(&choice, cx));
        (!parts.is_empty()).then(|| {
            SharedString::from(
                parts
                    .iter()
                    .map(SharedString::as_ref)
                    .collect::<Vec<&str>>()
                    .join(" "),
            )
        })
    }

    /// What the card says while SpaceTerm isn't connected to `choice`. A server SpaceTerm can't
    /// reach says so; one stopped or never connected says how to change its settings.
    fn offline_guidance(&self, choice: &ServerChoice, cx: &App) -> Option<SharedString> {
        if self.server_state(choice).live {
            return None;
        }
        let name = self.server_name(choice);
        if self.server_unreachable(choice) {
            let observed = server_selection::facts(&choice.key(), cx)
                .map(|facts| facts.observed_at)
                .filter(|at| *at > 0)
                .map(|at| observed_time(at, cx));
            return Some(observed.map_or_else(
                || "SpaceTerm can't reach the server right now.".into(),
                |at| format!("SpaceTerm can't reach {name}. Last known settings: {at}.").into(),
            ));
        }
        Some(
            match choice {
                ServerChoice::Local => {
                    format!("Start the server on {name} to change these settings.")
                }
                _ => format!("Connect to {name} to change these settings."),
            }
            .into(),
        )
    }

    /// Whether the last attempt to reach `choice` failed, rather than finding it stopped, not
    /// installed, or never asked.
    fn server_unreachable(&self, choice: &ServerChoice) -> bool {
        match choice {
            ServerChoice::Local => {
                let connection = self
                    .local_server
                    .as_ref()
                    .map_or(LocalServerStatus::NotChecked, |row| row.current_status());
                let phase = self
                    .server_service
                    .as_ref()
                    .and_then(|row| row.state.status)
                    .map(ServicePhase::of);
                let LocalServerStatus::Failed(error) = connection else {
                    return false;
                };
                // These name the server's own state, which its row and footnote already explain.
                let explained = matches!(
                    error,
                    ServerError::Stopped
                        | ServerError::NotInstalled
                        | ServerError::NotSetUp
                        | ServerError::NotRunning
                        | ServerError::StartupFailed
                        | ServerError::AuthorizationRequired
                        | ServerError::IncompatibleProtocol
                        | ServerError::MissingRequiredCapability
                        | ServerError::ActivationUnresolved
                        | ServerError::AuthenticationRefused
                );
                !explained && matches!(phase, None | Some(ServicePhase::Running))
            }
            ServerChoice::Direct(route) => {
                self.saved_direct_servers()
                    .into_iter()
                    .any(|(saved, status, _)| {
                        saved == *route && matches!(status, DirectRouteStatus::Unavailable(_))
                    })
            }
        }
    }

    /// The facts the selected server's dimmed rows show while it isn't live.
    fn selected_facts(&self, cx: &App) -> Option<LastKnownFacts> {
        server_selection::facts(&self.selected_server()?.key(), cx)
    }

    /// What the selected server's state explains at length: a refusal or failure, or a result
    /// that waits for the person. Work underway names itself in the Server row instead, and a
    /// plain state needs no words.
    fn status_message(&self) -> Option<(SharedString, CaptionTone)> {
        let attention = |text: SharedString, tone: CaptionTone, attention: bool| {
            (attention || tone != CaptionTone::Guidance).then_some((text, tone))
        };
        match self.selected_server()? {
            ServerChoice::Local => {
                let service = self.server_service_presentation().and_then(|service| {
                    if service.working.is_some() {
                        return None;
                    }
                    let waiting = service.authorization.is_some();
                    attention(service.explanation, service.tone, waiting)
                });
                service.or_else(|| {
                    let local = self.local_server_presentation()?;
                    attention(local.explanation, local.tone, false)
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

    /// What the selected server's update explains at length when it waits for the person or
    /// didn't finish.
    fn version_message(&self) -> Option<(SharedString, CaptionTone)> {
        match self.selected_server()? {
            ServerChoice::Local => {
                let update = self.server_update_presentation()?;
                (!update.working && update.tone != CaptionTone::Guidance)
                    .then_some((update.explanation, update.tone))
            }
            ServerChoice::Direct(_) => None,
        }
    }

    /// The service and connection steps the Server row offers, in order, and the management
    /// steps its ⋯ menu always holds.
    fn status_steps(&self, choice: &ServerChoice) -> (Vec<(StatusStep, bool)>, Vec<MenuItem>) {
        match choice {
            ServerChoice::Local => self.local_status_steps(),
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

    /// Takes one step from the Server row or its menu.
    fn take_status_step(&mut self, step: StatusStep, window: &mut Window, cx: &mut Context<Self>) {
        let Some(choice) = self.selected_server() else {
            return;
        };
        match (step, choice) {
            (StatusStep::Version(step), _) => self.take_version_step(step, cx),
            (StatusStep::InstallBuild(build, directory), ServerChoice::Local) => {
                self.confirm_installing(build, directory, window, cx);
            }
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
        _build: SharedString,
        directory: std::path::PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let owner = cx.weak_entity();
        let message = "It keeps running after you quit SpaceTerm.";
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
}

/// When the shown values were observed, in this computer's local time: "08:15" today, or
/// "Oct 21, 08:15" on another day.
pub(super) fn observed_time(at: u64, cx: &App) -> String {
    let calendar = crate::desktop_profile::DesktopPresentation::get(cx);
    let now = crate::ui::direct_access::unix_now();
    match (calendar.local_time(at), calendar.local_time(now)) {
        (Some(at), Some(now)) => clock_text(at, now),
        _ => "earlier".to_owned(),
    }
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

/// How the selected server's version and update read.
struct VersionView {
    /// The Server row's line under its label, such as "Version 0.14.2 · 0.15.0 is available".
    subtitle: SharedString,
    actions: Vec<VersionStep>,
    /// The version a failed update can return to, as the server's record of that update names
    /// it.
    restore: Option<SharedString>,
}

/// How the Server row's steps read.
struct ServerRowView {
    /// The one button, if any. Every other step is in the ⋯ menu.
    button: Option<StatusStep>,
    /// Work underway changes the server, so the row's steps wait for it.
    busy: bool,
    menu: Vec<MenuItem>,
    /// The label of the safe return after a failed update, naming the version it returns to.
    restore: Option<SharedString>,
}

/// How long work runs before the Server row shows it, so quick work never flashes.
const PROGRESS_DELAY: std::time::Duration = std::time::Duration::from_secs(1);

/// Work the Server row shows only once it has run longer than [`PROGRESS_DELAY`] on the server
/// the row presents.
#[derive(Default)]
pub(super) struct DelayedProgress {
    /// The server whose work is underway, and when that work began.
    since: Option<(ServerChoice, std::time::Instant)>,
    _reveal: Option<gpui::Task<()>>,
}

impl DelayedProgress {
    /// Follows the server with work underway, if any, and draws again once the work has run
    /// long enough to show. Presenting another server starts the delay over.
    fn track(&mut self, working: Option<ServerChoice>, cx: &mut Context<SettingsWindow>) {
        let Some(working) = working else {
            self.since = None;
            self._reveal = None;
            return;
        };
        if self
            .since
            .as_ref()
            .is_some_and(|(server, _)| *server == working)
        {
            return;
        }
        self.since = Some((working, cx.background_executor().now()));
        self._reveal = Some(cx.spawn(async move |settings, cx| {
            cx.background_executor().timer(PROGRESS_DELAY).await;
            let _ = settings.update(cx, |_, cx| cx.notify());
        }));
    }

    /// Whether the work underway has run long enough to show at `now`.
    fn revealed(&self, now: std::time::Instant) -> bool {
        self.since
            .as_ref()
            .is_some_and(|(_, since)| now.saturating_duration_since(*since) >= PROGRESS_DELAY)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum VersionStep {
    Local(UpdateAction),
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

/// The selector of Check Now on every server but this computer's installed one.
pub(super) const VERSION_CHECK_SELECTOR: &str = "settings-server-version-check";
/// How often Check Now looks again at work it waits for.
const CHECK_POLL: std::time::Duration = std::time::Duration::from_millis(200);

impl VersionStep {
    const fn label(self) -> &'static str {
        match self {
            Self::Local(action) => action.label(),
            Self::Observe => "Check Now",
        }
    }

    const fn selector(self) -> &'static str {
        match self {
            Self::Local(action) => action.selector(),
            Self::Observe => VERSION_CHECK_SELECTOR,
        }
    }

    /// The element the step occupies. Steps that replace one another share a slot, so keyboard
    /// focus stays on it when, say, Update Server gives way to Finish Update.
    const fn slot(self) -> &'static str {
        match self {
            Self::Local(action) => action.slot(),
            Self::Observe => VERSION_CHECK_SELECTOR,
        }
    }

    /// The step that changes the server leads, as the primary button.
    const fn primary(self) -> bool {
        matches!(
            self,
            Self::Local(UpdateAction::Update | UpdateAction::Finish)
        )
    }
}

impl SettingsWindow {
    /// Whether Check Now is reading `choice` again.
    fn version_checking(&self, choice: &ServerChoice) -> bool {
        self.version_check
            .as_ref()
            .is_some_and(|check| check.target == *choice)
    }

    /// The selected server's version and update, as its update journey presents them.
    fn version_view(&self, cx: &App) -> VersionView {
        let observed = self
            .selected_facts(cx)
            .and_then(|facts| facts.version)
            .map(SharedString::from);
        let Some(choice) = self.selected_server() else {
            return VersionView {
                subtitle: SharedString::default(),
                actions: Vec::new(),
                restore: None,
            };
        };
        let needs_update = self.server_state(&choice).state.as_ref() == "Needs update";
        match choice {
            ServerChoice::Local => self.local_version_view(observed, needs_update),
            // A server is updated on its own computer, so Check Now only checks SpaceTerm and
            // reads the paired server's facts again over its connection.
            ServerChoice::Direct(_) => {
                let version = version_text(observed.as_ref());
                VersionView {
                    subtitle: format!("{version} \u{b7} Update it from its own computer").into(),
                    actions: vec![VersionStep::Observe],
                    restore: None,
                }
            }
        }
    }

    fn local_version_view(
        &self,
        observed: Option<SharedString>,
        needs_update: bool,
    ) -> VersionView {
        let Some(row) = self.server_update.as_ref() else {
            return VersionView {
                subtitle: SharedString::new_static(
                    "This run builds its own server, so it has no update.",
                ),
                actions: vec![VersionStep::Observe],
                restore: None,
            };
        };
        let presentation = row.presentation();
        let facts = row.state.facts.as_ref();
        let value = facts
            .and_then(|facts| facts.approved.as_ref())
            .map(|approved| SharedString::from(approved.version.clone()))
            .or(observed);
        let version = version_text(value.as_ref());
        let included = facts
            .and_then(|facts| facts.included.as_ref().ok())
            .map(|included| included.version.clone());
        let activation = facts
            .and_then(|facts| facts.activation.as_ref())
            .filter(|activation| !activation.returned);
        let candidate = activation
            .map(|activation| activation.candidate.version.clone())
            .or_else(|| included.clone());
        let offered = |action: UpdateAction| presentation.actions.contains(&action);
        let failed = presentation.state_selector == "settings-server-update-state-failed";
        // The update's own record names the version it left, even before the approved build
        // reads again.
        let previous = activation.map_or_else(
            || value.as_ref().map_or_else(String::new, ToString::to_string),
            |activation| activation.previous.version.clone(),
        );
        let restore = (failed && offered(UpdateAction::Return) && !previous.is_empty())
            .then(|| SharedString::from(previous.clone()));
        let (subtitle, actions): (String, Vec<UpdateAction>) = if failed {
            (
                failed_subtitle(candidate.as_deref(), &previous),
                if offered(UpdateAction::Return) {
                    vec![UpdateAction::Return, UpdateAction::Check]
                } else {
                    vec![UpdateAction::Check]
                },
            )
        } else if offered(UpdateAction::Finish) {
            (
                unfinished_subtitle(&version, candidate.as_deref()),
                vec![UpdateAction::Finish],
            )
        } else if offered(UpdateAction::Update) && needs_update {
            (
                format!("{version} · Update required"),
                vec![UpdateAction::Update],
            )
        } else if offered(UpdateAction::Update) {
            (
                available_subtitle(&version, included.as_deref()),
                vec![UpdateAction::Update],
            )
        } else if presentation.state_selector == "settings-server-update-state-not-installed" {
            ("Install to see the server version.".to_owned(), Vec::new())
        } else {
            (version, vec![UpdateAction::Check])
        };
        VersionView {
            subtitle: subtitle.into(),
            actions: actions.into_iter().map(VersionStep::Local).collect(),
            restore,
        }
    }

    /// The Server row's steps: at most one button, and every other step in the ⋯ menu. The
    /// button is a safe restore after a failed update, Update when one is offered, otherwise
    /// Start Server for a stopped server.
    fn server_row_view(&self, cx: &App) -> Option<ServerRowView> {
        let choice = self.selected_server()?;
        let version = self.version_view(cx);
        let (buttons, menu) = self.status_steps(&choice);
        let busy = self.server_progress(&choice).is_some();
        let mut steps: Vec<StatusStep> = version
            .actions
            .iter()
            .copied()
            .map(StatusStep::Version)
            .chain(buttons.into_iter().map(|(step, _)| step))
            .collect();
        let leading = steps
            .iter()
            .enumerate()
            .filter_map(|(index, step)| button_rank(step).map(|rank| (rank, index)))
            .min()
            .map(|(_, index)| index);
        let button = leading.map(|index| steps.remove(index));
        // Stopping a connection attempt stays possible while it runs.
        let cancels =
            |step: &StatusStep| matches!(step, StatusStep::Direct(DirectServerAction::Cancel));
        let install_choices = self
            .server_service_presentation()
            .map(|presentation| presentation.install_choices)
            .unwrap_or_default();
        let mut items: Vec<MenuItem> = Vec::new();
        for step in steps {
            let enabled = !busy || cancels(&step);
            // Each verified build is its own entry, so the person names the build before
            // SpaceTerm asks to install it.
            if step == StatusStep::Local(ServiceAction::Install) {
                let several = install_choices.len() > 1;
                for (position, choice) in install_choices.iter().enumerate() {
                    let mut item = MenuItem::new(
                        format!("Install {}\u{2026}", choice.label),
                        StatusStep::InstallBuild(
                            choice.label.clone(),
                            choice.directory().to_path_buf(),
                        ),
                        enabled,
                    );
                    if several {
                        item.selector = SharedString::from(format!(
                            "{}-{position}",
                            super::servers::INSTALL_CHOICE_SELECTOR
                        ));
                    } else {
                        item.selector = super::servers::INSTALL_SELECTOR.into();
                    }
                    items.push(item);
                }
                continue;
            }
            items.push(MenuItem::new(status_step_label(&step), step, enabled));
        }
        // The management steps follow the row's other steps, apart.
        let mut separate = !items.is_empty();
        for item in menu {
            // A step the row already offers, such as Reconnect for a paired server offering
            // Connect, appears once.
            if items.iter().any(|offered| offered.step == item.step) {
                continue;
            }
            let item = MenuItem {
                enabled: item.enabled && !busy,
                ..item
            };
            items.push(if std::mem::take(&mut separate) {
                item.after_separator()
            } else {
                item
            });
        }
        let restore = version
            .restore
            .map(|previous| SharedString::from(format!("Restore {previous}\u{2026}")));
        Some(ServerRowView {
            button,
            busy,
            menu: items,
            restore,
        })
    }

    /// The operation underway on `choice` that changes it, as the Server row names it under its
    /// label. A read isn't one: the state says Checking instead.
    fn server_progress(&self, choice: &ServerChoice) -> Option<SharedString> {
        match choice {
            ServerChoice::Local => {
                if let Some(row) = self.server_update.as_ref()
                    && let Some(text) = row
                        .state
                        .working
                        .and_then(|step| update_progress(step, &row.state))
                {
                    return Some(text.into());
                }
                self.server_service
                    .as_ref()
                    .and_then(|row| row.state.working)
                    .filter(|action| *action != ServiceAction::Check)
                    .map(|action| SharedString::new_static(service_progress(action)))
            }
            ServerChoice::Direct(_) => None,
        }
    }

    /// Follows the work underway on the selected server, so the Server row shows it only once it
    /// runs longer than a second. Rendering the row calls this before reading its text.
    pub(super) fn track_server_progress(&mut self, cx: &mut Context<Self>) {
        let (checking, busy) = self.selected_server().map_or((None, None), |choice| {
            (
                self.server_state(&choice).checking.then(|| choice.clone()),
                self.server_progress(&choice).is_some().then_some(choice),
            )
        });
        self.check_progress.track(checking, cx);
        self.operation_progress.track(busy, cx);
    }

    /// The Server row's line under its label: the work underway once it has run longer than a
    /// second, otherwise the version and any update.
    pub(super) fn server_subtitle(&self, cx: &App) -> SharedString {
        let Some(choice) = self.selected_server() else {
            return SharedString::default();
        };
        if self
            .operation_progress
            .revealed(cx.background_executor().now())
            && let Some(progress) = self.server_progress(&choice)
        {
            return progress;
        }
        self.version_view(cx).subtitle
    }

    /// The spinner leading the Server row's subtitle while it names work underway.
    pub(super) fn server_progress_indicator(&self, cx: &App) -> Option<AnyElement> {
        let choice = self.selected_server()?;
        if !self
            .operation_progress
            .revealed(cx.background_executor().now())
        {
            return None;
        }
        let progress = self.server_progress(&choice)?;
        Some(
            ProgressRing::new(
                SharedString::from(PROGRESS_SELECTOR),
                progress,
                ProgressState::Indeterminate,
            )
            .size(ProgressSize::Compact)
            .debug_selector(PROGRESS_SELECTOR)
            .into_any_element(),
        )
    }

    /// Checks for a newer SpaceTerm, which brings each server's build, then reads the server it
    /// was asked for again once that check settles. Reading connects to, starts and installs
    /// nothing: this account's service is read as Check Status reads it, and a paired server only
    /// over a connection SpaceTerm already has.
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
                // A read is always allowed, whichever steps the last read offered. The service is
                // read too, so a change made outside Settings shows.
                self.check_server_update(cx);
                self.check_local_server(cx);
                self.check_server_service(cx);
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
            ServerChoice::Direct(_) => false,
        }
    }

    fn take_version_step(&mut self, step: VersionStep, cx: &mut Context<Self>) {
        match (step, self.selected_server()) {
            (VersionStep::Local(UpdateAction::Check), _) | (VersionStep::Observe, _) => {
                self.check_versions(cx)
            }
            // An update asked for while a read runs starts once the read offers it again.
            (VersionStep::Local(UpdateAction::Update), Some(ServerChoice::Local)) => {
                self.request_server_update(cx)
            }
            (VersionStep::Local(action), Some(ServerChoice::Local)) => {
                self.request_update_action(action, cx)
            }
            _ => {}
        }
    }

    /// The Server row's control: the state, at most one button, and a ⋯ menu. A read running
    /// longer than a second shows as Checking in the state's place; the state never moves the
    /// row.
    pub(super) fn render_server_status(
        &mut self,
        appearance: &ChromeAppearance,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(choice) = self.selected_server() else {
            return div().into_any_element();
        };
        let Some(view) = self.server_row_view(cx) else {
            return div().into_any_element();
        };
        let name = self.server_name(&choice);
        let state = self.server_state(&choice);
        let checking =
            state.checking && self.check_progress.revealed(cx.background_executor().now());
        let shown = if checking {
            SharedString::new_static("Checking")
        } else {
            state.state.clone()
        };
        let colors = appearance.host_colors(spaceterm_ui::ControlHost::Card);
        let indicator = if state.working || checking {
            ProgressRing::new(
                SharedString::from(format!("{STATUS_SELECTOR}-progress")),
                shown.clone(),
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
            .aria_label(SharedString::from(format!("{name}: {shown}")))
            .flex()
            .flex_row()
            .items_center()
            .gap(appearance.spacing(6.0))
            .child(indicator)
            .child(
                div()
                    .chrome_text(appearance.typography.style(TextRole::Body))
                    .text_color(gpui_color(colors.text_secondary))
                    .child(shown),
            );
        let owner = cx.weak_entity();
        let busy = view.busy;
        let restore = view.restore.clone();
        // Restore prepares through the update row, which takes no step while it reads, so it
        // waits for that read rather than taking a click it would drop.
        let reading_update = self
            .server_update
            .as_ref()
            .is_some_and(|row| row.state.working.is_some());
        let button = view.button.map(|step| {
            let owner = owner.clone();
            let waits = busy
                || (reading_update
                    && step == StatusStep::Version(VersionStep::Local(UpdateAction::Return)));
            let label: SharedString = match (&step, &restore) {
                (StatusStep::Version(VersionStep::Local(UpdateAction::Return)), Some(restore)) => {
                    restore.clone()
                }
                // The row names the server, so its update reads as one word.
                (StatusStep::Version(VersionStep::Local(UpdateAction::Update)), _) => {
                    SharedString::new_static("Update\u{2026}")
                }
                _ => SharedString::new_static(status_step_label(&step)),
            };
            // Steps that replace each other share a slot, so keyboard focus stays on it when,
            // say, Start Server gives way to Update.
            let slot = status_step_slot(&step).unwrap_or_else(|| status_step_selector(&step));
            let description = status_step_consequence(&step, &name).unwrap_or_else(|| {
                SharedString::from(format!("{} {name}", label.trim_end_matches('\u{2026}')))
            });
            Button::new(
                ElementId::from(SharedString::from(format!("{STATUS_SELECTOR}-{slot}"))),
                label,
            )
            .variant(if status_step_primary(&step) {
                ButtonVariant::Primary
            } else {
                ButtonVariant::Outline
            })
            .size(ButtonSize::Regular)
            .tab_stop(true)
            .disabled(waits)
            .debug_selector(status_step_selector(&step))
            .accessibility_description(description)
            .on_activate(move |_, window, cx| {
                let step = step.clone();
                let _ = owner.update(cx, |settings, cx| {
                    settings.take_status_step(step, window, cx);
                });
            })
            .into_any_element()
        });
        let menu = (!view.menu.is_empty()).then(|| {
            let mut entries = Vec::new();
            for item in view.menu {
                if item.separator_before {
                    entries.push(MenuEntry::separator());
                }
                entries.push(
                    MenuEntry::action(item.label, item.step)
                        .disabled(!item.enabled)
                        .destructive(item.destructive)
                        .debug_selector(item.selector.to_string()),
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
            .children(button)
            .children(menu)
            .into_any_element()
    }

    /// Whether the selected server starts on its own, as it reports or as last observed. A
    /// change the person asked for shows at once, until the service answers it.
    pub(super) fn automatic_start(&self) -> Option<bool> {
        match self.selected_server()? {
            ServerChoice::Local => self
                .server_service
                .as_ref()
                .and_then(|row| row.automatic_start_shown()),
            ServerChoice::Direct(_) => None,
        }
    }

    /// The Start automatically row's description, naming when the server starts.
    pub(super) fn auto_start_description(&self) -> SharedString {
        if self.this_computer == "This Mac" {
            SharedString::new_static("Start at login.")
        } else {
            SharedString::new_static("Start the server when this computer starts.")
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
            ServerChoice::Direct(_) => None,
        }
    }

    /// Whether the selected server's automatic start can change now: this account's installed
    /// service unless another operation changes it. A change asked for during a read or an
    /// earlier change follows it. Only this computer's service offers it.
    fn automatic_start_changeable(&self) -> bool {
        if self.automatic_start().is_none() {
            return false;
        }
        match self.selected_server() {
            // A failed read shows the confirmed setting without offering to change it.
            Some(ServerChoice::Local) => self.server_service.as_ref().is_some_and(|row| {
                row.state.status.is_some()
                    && row
                        .state
                        .working
                        .is_none_or(|action| action == ServiceAction::Check)
            }),
            _ => false,
        }
    }

    /// Asks this computer's server to start on its own, or not. A deliberate Stop stays in force.
    fn set_automatic_start(&mut self, enabled: bool, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected_server() == Some(ServerChoice::Local) {
            self.set_local_automatic_start(enabled, window, cx);
        }
    }

    /// The Start automatically switch. It controls automatic start only; Uninstall stays in the
    /// Status menu.
    pub(super) fn render_auto_start(
        &mut self,
        _: &ChromeAppearance,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let value = self.automatic_start();
        let changeable = self.automatic_start_changeable();
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
        StatusStep::Direct(action) => action.selector(),
        StatusStep::UnpairThisDevice => "settings-server-unpair",
        StatusStep::Version(step) => step.selector(),
        StatusStep::InstallBuild(..) => super::servers::INSTALL_CHOICE_SELECTOR,
    }
}

/// The element a Status step occupies, shared by steps that replace one another.
fn status_step_slot(step: &StatusStep) -> Option<&'static str> {
    match step {
        StatusStep::Local(action) => Some(action.slot()),
        StatusStep::Version(step) => Some(step.slot()),
        _ => None,
    }
}

/// What a Status step does beyond its name, for assistive technology.
fn status_step_consequence(step: &StatusStep, machine: &str) -> Option<SharedString> {
    match step {
        StatusStep::Local(action) => action.consequence().map(SharedString::new_static),
        StatusStep::Version(VersionStep::Local(UpdateAction::Check) | VersionStep::Observe) => {
            Some(SharedString::from(format!(
                "Checks for a newer SpaceTerm and reads {machine} again."
            )))
        }
        StatusStep::Version(VersionStep::Local(action)) => {
            action.consequence().map(SharedString::new_static)
        }
        _ => None,
    }
}

/// Whether the step leads as the primary button: one that updates the server.
fn status_step_primary(step: &StatusStep) -> bool {
    matches!(step, StatusStep::Version(step) if step.primary())
}

/// How strongly a step claims the Server row's one button, lowest first. `None` belongs in the
/// ⋯ menu.
fn button_rank(step: &StatusStep) -> Option<u8> {
    match step {
        StatusStep::Version(VersionStep::Local(UpdateAction::Return)) => Some(0),
        StatusStep::Version(VersionStep::Local(UpdateAction::Update)) => Some(1),
        StatusStep::Local(ServiceAction::Start) => Some(2),
        _ => None,
    }
}

/// "Version 0.14.2", or "Version unknown" before SpaceTerm has observed one.
fn version_text(value: Option<&SharedString>) -> String {
    value.map_or_else(
        || "Version unknown".to_owned(),
        |value| format!("Version {value}"),
    )
}

fn available_subtitle(version: &str, candidate: Option<&str>) -> String {
    match candidate {
        Some(candidate) => format!("{version} \u{b7} {candidate} is available"),
        None => format!("{version} · Update available"),
    }
}

fn unfinished_subtitle(version: &str, candidate: Option<&str>) -> String {
    match candidate {
        Some(candidate) => format!("{version} · Update to {candidate} unfinished"),
        None => format!("{version} · Update unfinished"),
    }
}

fn failed_subtitle(candidate: Option<&str>, previous: &str) -> String {
    let attempted = candidate.map_or_else(
        || "Couldn't update".to_owned(),
        |candidate| format!("Couldn't update to {candidate}"),
    );
    if previous.is_empty() {
        attempted
    } else {
        format!("{attempted} \u{b7} Still on {previous}")
    }
}

/// The update step underway, as the Server row names it. A read isn't one.
fn update_progress(step: UpdateStep, state: &UpdateState) -> Option<String> {
    let facts = state.facts.as_ref();
    let included = facts
        .and_then(|facts| facts.included.as_ref().ok())
        .map(|included| included.version.as_str());
    let activation = facts.and_then(|facts| facts.activation.as_ref());
    let candidate = activation
        .map(|activation| activation.candidate.version.as_str())
        .or(included);
    let previous = activation.map(|activation| activation.previous.version.as_str());
    let named = |version: Option<&str>, named: &str, unnamed: &str| {
        version.map_or_else(
            || unnamed.to_owned(),
            |version| format!("{named} {version}\u{2026}"),
        )
    };
    Some(match step {
        UpdateStep::Check => return None,
        UpdateStep::Prepare => named(
            included,
            "Preparing the update to",
            "Preparing the update\u{2026}",
        ),
        UpdateStep::Update { .. } => named(included, "Updating to", "Updating the server\u{2026}"),
        UpdateStep::PrepareReturn => named(
            previous,
            "Preparing to restore",
            "Preparing to restore the previous version\u{2026}",
        ),
        UpdateStep::Return => named(
            previous,
            "Restoring",
            "Restoring the previous version\u{2026}",
        ),
        UpdateStep::Finish => named(
            candidate,
            "Finishing the update to",
            "Finishing the update\u{2026}",
        ),
    })
}

/// A local service step underway, as the Server row names it.
const fn service_progress(action: ServiceAction) -> &'static str {
    match action {
        ServiceAction::Check => "Checking the server\u{2026}",
        ServiceAction::Install => "Installing the server\u{2026}",
        ServiceAction::Start => "Starting the server\u{2026}",
        ServiceAction::Stop => "Stopping the server\u{2026}",
        ServiceAction::Restart => "Restarting the server\u{2026}",
        ServiceAction::Uninstall => "Uninstalling the server\u{2026}",
        ServiceAction::AuthorizeInstall | ServiceAction::AuthorizeStart => {
            "Waiting for authorization\u{2026}"
        }
    }
}

/// The label a Status step reads by.
fn status_step_label(step: &StatusStep) -> &'static str {
    match step {
        StatusStep::Local(ServiceAction::Install) => "Install\u{2026}",
        StatusStep::Local(ServiceAction::Check) => "Check Status",
        StatusStep::Local(action) => action.label(),
        StatusStep::Direct(action) => action.label(),
        StatusStep::UnpairThisDevice => "Unpair\u{2026}",
        StatusStep::Version(step) => step.label(),
        StatusStep::InstallBuild(..) => "Install\u{2026}",
    }
}

#[cfg(test)]
#[path = "server_card_tests.rs"]
mod tests;
