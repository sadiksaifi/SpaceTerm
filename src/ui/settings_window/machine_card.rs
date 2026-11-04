//! The selected server's card: the Server row, Start automatically and Terminal Sessions.
//!
//! Each row reads what SpaceTerm already knows: this account's service and update rows, and the
//! direct registry. Rendering asks no server anything. A paired server's card offers its
//! connection steps, and Stop, Restart and Update over its service lane, which
//! [`super::remote_admin`] takes. Start stays on a server's own computer. When SpaceTerm isn't
//! connected to the server, the rows under Server dim and show what SpaceTerm last observed, and
//! the card's footnote says how to change them.

use gpui::prelude::*;
use gpui::{AnyElement, App, Context, ElementId, SharedString, Window, accesskit, div};
use spaceterm_ui::{
    AnchoredAlignment, AnchoredPlacement, AnchoredPlacementConfig, Button, ButtonSize,
    ButtonVariant, Icon, IconName, Menu, MenuEntry, ProgressRing, ProgressSize, ProgressState,
    Switch, ToggleSize,
};

use super::SettingsWindow;
use super::core_update::{UpdateAction, UpdateState, UpdateStep};
use super::machine_list::{MachineChoice, status_dot, tone_color};
use super::machine_selection;
use super::machines::{LocalCoreStatus, ServiceAction, ServicePhase};
use super::remote_admin::{Change, Work};
use crate::core_context::{LastKnownFacts, ObservedCoreStatus};
use crate::desktop_profile::LocalTime;
use crate::direct_connections::{DirectRouteStatus, RefusalReason};
use crate::machine_connection::CoreError;
use crate::ui::appearance::{ChromeAppearance, gpui_color};
use crate::ui::chrome_icons::IconRole;
use crate::ui::chrome_typography::{ChromeTextStyleExt as _, TextRole};
use crate::ui::direct_machines::DirectMachineAction;
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
pub(in crate::ui::settings_window) struct CoreState {
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

impl CoreState {
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
    connection: LocalCoreStatus,
    service: Option<(Option<ServicePhase>, Option<ServiceAction>, bool)>,
    updating: bool,
) -> CoreState {
    let working = service.and_then(|(_, working, _)| working);
    let operation = if updating {
        Some("Updating\u{2026}")
    } else {
        working
            .filter(|action| *action != ServiceAction::Check)
            .map(service_verb)
    };
    CoreState {
        checking: working == Some(ServiceAction::Check),
        operation,
        ..local_observed(connection, service)
    }
}

/// How this account's server reads from what SpaceTerm last observed of it.
fn local_observed(
    connection: LocalCoreStatus,
    service: Option<(Option<ServicePhase>, Option<ServiceAction>, bool)>,
) -> CoreState {
    let connected = matches!(connection, LocalCoreStatus::Connected(_));
    if matches!(
        connection,
        LocalCoreStatus::Failed(CoreError::IncompatibleProtocol)
    ) {
        return CoreState::new("Needs update", "Needs update", Tone::Problem);
    }
    match service {
        Some((_, _, true)) => CoreState::new(
            "Needs authorization",
            "Needs authorization",
            Tone::Attention,
        ),
        Some((Some(ServicePhase::NotInstalled), _, _)) => {
            CoreState::new("Not installed", "Not installed", Tone::Idle)
        }
        Some((Some(ServicePhase::Stopped), _, _)) => {
            CoreState::new("Stopped", "Stopped", Tone::Idle)
        }
        Some((Some(ServicePhase::NotRunning), _, _)) => {
            CoreState::new("Not running", "Not running", Tone::Attention)
        }
        Some((Some(ServicePhase::Running), _, _)) | None if connected => {
            CoreState::new("Running", "Running \u{b7} this account", Tone::Live).live()
        }
        Some((Some(ServicePhase::Running), _, _)) => {
            CoreState::new("Running", "Running \u{b7} this account", Tone::Live)
        }
        _ => match connection {
            LocalCoreStatus::NotChecked | LocalCoreStatus::Checking => {
                CoreState::working("Checking\u{2026}", false)
            }
            LocalCoreStatus::Failed(CoreError::Stopped) => {
                CoreState::new("Stopped", "Stopped", Tone::Idle)
            }
            LocalCoreStatus::Failed(CoreError::NotInstalled | CoreError::NotSetUp) => {
                CoreState::new("Not installed", "Not installed", Tone::Idle)
            }
            LocalCoreStatus::Failed(CoreError::NotRunning | CoreError::StartupFailed) => {
                CoreState::new("Not running", "Not running", Tone::Attention)
            }
            LocalCoreStatus::Failed(_) => {
                CoreState::new("Unavailable", "Unavailable", Tone::Problem)
            }
            LocalCoreStatus::Connected(_) => {
                CoreState::new("Running", "Running \u{b7} this account", Tone::Live).live()
            }
        },
    }
}

/// How a paired server reads.
pub(super) fn direct_state(status: DirectRouteStatus) -> CoreState {
    match status {
        DirectRouteStatus::Connected => CoreState::new(
            "Connected over the network",
            "Connected \u{b7} Paired",
            Tone::Live,
        )
        .live(),
        DirectRouteStatus::Connecting => CoreState::working("Connecting\u{2026}", false),
        DirectRouteStatus::Reconnecting => CoreState::working("Reconnecting\u{2026}", false),
        DirectRouteStatus::Disconnected => {
            CoreState::new("Not connected", "Not connected", Tone::Idle)
        }
        DirectRouteStatus::NeedsInteraction => {
            CoreState::new("Waiting for you", "Waiting for you", Tone::Attention)
        }
        DirectRouteStatus::Refused(RefusalReason::Revoked) => {
            CoreState::new("Unpaired", "Unpaired", Tone::Problem)
        }
        DirectRouteStatus::Refused(RefusalReason::Incompatible) => {
            CoreState::new("Needs update", "Needs update", Tone::Problem)
        }
        DirectRouteStatus::Refused(_) => {
            CoreState::new("Not verified", "Not verified", Tone::Problem)
        }
        DirectRouteStatus::Unavailable(_) => {
            CoreState::new("Not reachable", "Not reachable", Tone::Attention)
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
    Direct(DirectMachineAction),
    /// A change to a paired server over its service lane.
    Remote(Change),
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
    pub(in crate::ui::settings_window) fn core_state(&self, choice: &MachineChoice) -> CoreState {
        match choice {
            MachineChoice::Local => {
                let connection = self
                    .local_core
                    .as_ref()
                    .map_or(LocalCoreStatus::NotChecked, |row| row.current_status());
                let service = self.core_service.as_ref().map(|row| {
                    let presentation = row.presentation();
                    (
                        row.state.status.map(ServicePhase::of),
                        presentation.working,
                        presentation.authorization.is_some(),
                    )
                });
                let update_step = self.core_update.as_ref().and_then(|row| row.state.working);
                let updating = update_step.is_some_and(|step| step.changes_service());
                let mut state = local_state(connection, service, updating);
                // Reading the connection or the version leaves the state as last observed too.
                state.checking |= self.local_core_working()
                    || update_step == Some(UpdateStep::Check)
                    || self.version_checking(choice);
                let failed = self.core_update_presentation().is_some_and(|update| {
                    update.state_selector == "settings-server-update-state-failed"
                });
                if failed && state.operation.is_none() {
                    state.state = "Update failed".into();
                    state.short = "Update failed".into();
                    state.tone = Tone::Problem;
                }
                state
            }
            MachineChoice::Direct(route) => {
                let retained = self.retained_status.get(&choice.key()).copied();
                let mut state = match self
                    .saved_direct_machines()
                    .into_iter()
                    .find(|(saved, _, _)| saved == route)
                {
                    Some((_, DirectRouteStatus::Disconnected, _))
                        if retained == Some(ObservedCoreStatus::NeedsUpdate) =>
                    {
                        CoreState::new("Needs update", "Needs update", Tone::Problem)
                    }
                    Some((_, status, _)) => direct_state(status),
                    None => CoreState::new("Not connected", "Not connected", Tone::Idle),
                };
                // A server SpaceTerm stopped reads as stopped rather than as a connection that
                // keeps retrying, until someone starts it on its own computer.
                if !state.live && self.remote_stopped(route) {
                    state = CoreState::new("Stopped", "Stopped", Tone::Idle);
                }
                state.operation = self.remote_operation(route);
                state.checking |= self.remote_reading(route);
                state
            }
        }
    }

    /// "machine · OS" for this computer, "address · OS" for paired servers. A
    /// part SpaceTerm hasn't observed is left out, and a server it knows nothing about says how
    /// it is reached.
    pub(in crate::ui::settings_window) fn core_detail(
        &self,
        choice: &MachineChoice,
        cx: &App,
    ) -> SharedString {
        let facts = machine_selection::facts(&choice.key(), cx);
        let observed_system = facts
            .as_ref()
            .and_then(|facts| facts.operating_system.clone())
            .map(|system| operating_system_name(&system));
        let (place, system) = match choice {
            MachineChoice::Local => (
                facts.as_ref().and_then(|facts| facts.machine.clone()),
                observed_system,
            ),
            MachineChoice::Direct(route) => (
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
                MachineChoice::Local => SharedString::new_static("This account"),
                MachineChoice::Direct(_) => SharedString::new_static("Paired over network"),
            },
        }
    }

    /// Whether SpaceTerm is connected to the selected server now.
    pub(in crate::ui::settings_window) fn selected_machine_live(&self) -> bool {
        self.selected_machine()
            .is_some_and(|choice| self.core_state(&choice).live)
    }

    /// The selected server's card footnote: what its state or update explains at length, and,
    /// while SpaceTerm isn't connected, how to change its settings and when the shown values were
    /// observed. The Server row itself keeps one line, so its height never changes.
    pub(super) fn machine_card_footnote(&self, cx: &App) -> Option<SharedString> {
        let choice = self.selected_machine()?;
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
    fn offline_guidance(&self, choice: &MachineChoice, cx: &App) -> Option<SharedString> {
        if self.core_state(choice).live {
            return None;
        }
        let name = self.machine_name(choice);
        // The stop SpaceTerm made explains itself, including where to start the server again.
        if let MachineChoice::Direct(route) = choice
            && self.remote_stopped(route)
        {
            return None;
        }
        if self.core_unreachable(choice) {
            let observed = machine_selection::facts(&choice.key(), cx)
                .map(|facts| facts.observed_at)
                .filter(|at| *at > 0)
                .map(|at| observed_time(at, cx));
            let reach = observed.map_or_else(
                || "SpaceTerm can\u{2019}t reach SpaceTerm Core right now.".to_owned(),
                |at| format!("SpaceTerm can\u{2019}t reach {name}. Last known settings: {at}."),
            );
            // A stopped server accepts no connection, so only its own computer starts it.
            return Some(match choice {
                MachineChoice::Direct(_) => {
                    format!("{reach} If it\u{2019}s stopped, start it on its own computer.").into()
                }
                MachineChoice::Local => reach.into(),
            });
        }
        Some(
            match choice {
                MachineChoice::Local if self.local_core_not_installed() => {
                    format!("Install SpaceTerm Core on {name} to change these settings.")
                }
                MachineChoice::Local => {
                    format!("Start SpaceTerm Core on {name} to change these settings.")
                }
                _ => format!("Connect to {name} to change these settings."),
            }
            .into(),
        )
    }

    /// Whether this computer has no installed server service, so only Install, not Start,
    /// brings its settings back.
    fn local_core_not_installed(&self) -> bool {
        let service = self
            .core_service
            .as_ref()
            .and_then(|row| row.state.status)
            .map(ServicePhase::of);
        let connection = self.local_core.as_ref().map(|row| row.current_status());
        service == Some(ServicePhase::NotInstalled)
            || matches!(
                connection,
                Some(LocalCoreStatus::Failed(CoreError::NotInstalled))
            )
    }

    /// Whether the last attempt to reach `choice` failed, rather than finding it stopped, not
    /// installed, or never asked.
    fn core_unreachable(&self, choice: &MachineChoice) -> bool {
        match choice {
            MachineChoice::Local => {
                let connection = self
                    .local_core
                    .as_ref()
                    .map_or(LocalCoreStatus::NotChecked, |row| row.current_status());
                let phase = self
                    .core_service
                    .as_ref()
                    .and_then(|row| row.state.status)
                    .map(ServicePhase::of);
                let LocalCoreStatus::Failed(error) = connection else {
                    return false;
                };
                // These name the server's own state, which its row and footnote already explain.
                let explained = matches!(
                    error,
                    CoreError::Stopped
                        | CoreError::NotInstalled
                        | CoreError::NotSetUp
                        | CoreError::NotRunning
                        | CoreError::StartupFailed
                        | CoreError::AuthorizationRequired
                        | CoreError::IncompatibleProtocol
                        | CoreError::ActivationUnresolved
                        | CoreError::AuthenticationRefused
                );
                !explained && matches!(phase, None | Some(ServicePhase::Running))
            }
            MachineChoice::Direct(route) => {
                self.saved_direct_machines()
                    .into_iter()
                    .any(|(saved, status, _)| {
                        saved == *route && matches!(status, DirectRouteStatus::Unavailable(_))
                    })
            }
        }
    }

    /// The facts the selected server's dimmed rows show while it isn't live.
    fn selected_facts(&self, cx: &App) -> Option<LastKnownFacts> {
        machine_selection::facts(&self.selected_machine()?.key(), cx)
    }

    /// What the selected server's state explains at length: a refusal or failure, or a result
    /// that waits for the person. Work underway names itself in the Server row instead, and a
    /// plain state needs no words.
    fn status_message(&self) -> Option<(SharedString, CaptionTone)> {
        match self.selected_machine()? {
            // The service explains the server whenever there is one; without it, as in a
            // Development run, the connection does.
            MachineChoice::Local => match &self.core_service {
                Some(service) => service.settled_explanation(),
                None => {
                    let local = self.local_core_presentation()?;
                    Some((local.explanation, local.tone))
                }
            },
            MachineChoice::Direct(route) => self
                .direct_problem()
                .map(|problem| (problem, CaptionTone::Error))
                .or_else(|| {
                    let (_, status, name) = self
                        .saved_direct_machines()
                        .into_iter()
                        .find(|(saved, _, _)| *saved == route)?;
                    let presentation = crate::ui::direct_machines::present(status, &name);
                    Some((
                        SharedString::from(presentation.explanation),
                        presentation.tone,
                    ))
                }),
        }
    }

    /// What the selected server's update explains at length when it waits for the person or
    /// didn't finish.
    fn version_message(&self) -> Option<(SharedString, CaptionTone)> {
        match self.selected_machine()? {
            MachineChoice::Local => {
                let update = self.core_update_presentation()?;
                (!update.working && update.tone != CaptionTone::Guidance)
                    .then_some((update.explanation, update.tone))
            }
            MachineChoice::Direct(route) => {
                self.remote_message(&route, &self.machine_name(&MachineChoice::Direct(route)))
            }
        }
    }

    /// The service and connection steps the Server row offers, in order, and the management
    /// steps its ⋯ menu always holds.
    fn status_steps(
        &self,
        choice: &MachineChoice,
        cx: &App,
    ) -> (Vec<(StatusStep, bool)>, Vec<MenuItem>) {
        match choice {
            MachineChoice::Local => self.local_status_steps(),
            MachineChoice::Direct(route) => self.direct_status_steps(*route, cx),
        }
    }

    fn local_status_steps(&self) -> (Vec<(StatusStep, bool)>, Vec<MenuItem>) {
        let Some(presentation) = self.core_service_presentation() else {
            // Without a service, such as in a Development run, the connection can still be
            // checked again.
            let check = self
                .local_core_presentation()
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
            .core_service
            .as_ref()
            .and_then(|row| row.state.status)
            .is_some_and(|status| status.installed);
        let menu = if installed {
            vec![
                MenuItem::new(
                    "Restart SpaceTerm Core\u{2026}",
                    StatusStep::Local(ServiceAction::Restart),
                    idle && offered(ServiceAction::Restart),
                ),
                MenuItem::new(
                    "Stop SpaceTerm Core\u{2026}",
                    StatusStep::Local(ServiceAction::Stop),
                    idle && offered(ServiceAction::Stop),
                ),
                MenuItem::new(
                    "Uninstall SpaceTerm Core\u{2026}",
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
        cx: &App,
    ) -> (Vec<(StatusStep, bool)>, Vec<MenuItem>) {
        let Some((_, status, _)) = self
            .saved_direct_machines()
            .into_iter()
            .find(|(saved, _, _)| *saved == route)
        else {
            return (Vec::new(), Vec::new());
        };
        let presentation = crate::ui::direct_machines::present(status, "");
        let mut buttons: Vec<(StatusStep, bool)> = presentation
            .actions
            .iter()
            .map(|action| (StatusStep::Direct(*action), false))
            .collect();
        // The server's own update check found a newer release, so Update leads the row.
        let ready = self.remote_ready(&route, cx);
        if self.remote_available(&route).is_some() && ready {
            buttons.push((StatusStep::Remote(Change::Update), false));
        }
        let reconnectable = !matches!(
            status,
            DirectRouteStatus::Connecting | DirectRouteStatus::Refused(RefusalReason::Revoked)
        );
        let mut menu = Vec::new();
        // Stop and Restart reach the server over its service lane while SpaceTerm holds a
        // connection to it. Start needs the server's own computer, so the menu offers none.
        if super::remote_admin::installed(cx).is_some() {
            menu.push(MenuItem::new(
                Change::Restart.label(),
                StatusStep::Remote(Change::Restart),
                ready,
            ));
            menu.push(MenuItem::new(
                Change::Stop.label(),
                StatusStep::Remote(Change::Stop),
                ready,
            ));
        }
        let reconnect = MenuItem::new(
            "Reconnect",
            StatusStep::Direct(DirectMachineAction::Connect),
            reconnectable,
        );
        menu.push(if menu.is_empty() {
            reconnect
        } else {
            reconnect.after_separator()
        });
        menu.push(
            MenuItem::new("Remove Machine\u{2026}", StatusStep::UnpairThisDevice, true)
                .destructive()
                .after_separator(),
        );
        (buttons, menu)
    }

    /// Takes one step from the Server row or its menu.
    fn take_status_step(&mut self, step: StatusStep, window: &mut Window, cx: &mut Context<Self>) {
        let Some(choice) = self.selected_machine() else {
            return;
        };
        match (step, choice) {
            (StatusStep::Version(step), _) => self.take_version_step(step, cx),
            (StatusStep::InstallBuild(build, directory), MachineChoice::Local) => {
                self.confirm_installing(build, directory, window, cx);
            }
            (StatusStep::Local(ServiceAction::Install), MachineChoice::Local) => {
                self.confirm_local_installation(window, cx);
            }
            (StatusStep::Local(ServiceAction::Check), MachineChoice::Local) => {
                self.check_local_core(cx);
                self.check_core_service(cx);
                self.check_core_update(cx);
            }
            (StatusStep::Local(action), MachineChoice::Local) => {
                self.request_core_service(action, window, cx);
            }
            (StatusStep::Direct(action), MachineChoice::Direct(route)) => {
                self.run_direct_action(route, action, cx);
            }
            (StatusStep::Remote(change), MachineChoice::Direct(route)) => {
                self.request_remote_change(route, change, window.window_handle(), cx);
            }
            (StatusStep::UnpairThisDevice, MachineChoice::Direct(_)) => {
                self.confirm_unpair_this_device(window, cx);
            }
            // A step offered for a server the selection has since left does nothing.
            _ => {}
        }
    }

    /// Asks before installing the one verified build this account's service would run. Several
    /// builds are chosen from the Install menu instead, which then asks the same question.
    fn confirm_local_installation(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(presentation) = self.core_service_presentation() else {
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
            "Install SpaceTerm Core?",
            "Install SpaceTerm Core?",
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
                    settings.install_core_service(directory, cx);
                });
            }
        });
        if result.is_err() {
            eprintln!("failed to present the SpaceTerm Core installation confirmation");
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
struct MachineRowView {
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
    since: Option<(MachineChoice, std::time::Instant)>,
    _reveal: Option<gpui::Task<()>>,
}

impl DelayedProgress {
    /// Follows the server with work underway, if any, and draws again once the work has run
    /// long enough to show. Presenting another server starts the delay over.
    fn track(&mut self, working: Option<MachineChoice>, cx: &mut Context<SettingsWindow>) {
        let Some(working) = working else {
            self.since = None;
            self._reveal = None;
            return;
        };
        if self
            .since
            .as_ref()
            .is_some_and(|(core, _)| *core == working)
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
    target: MachineChoice,
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
    /// focus stays on it when, say, Update SpaceTerm Core gives way to Finish Update.
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
    fn version_checking(&self, choice: &MachineChoice) -> bool {
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
        let Some(choice) = self.selected_machine() else {
            return VersionView {
                subtitle: SharedString::default(),
                actions: Vec::new(),
                restore: None,
            };
        };
        let needs_update = self.core_state(&choice).state.as_ref() == "Needs update";
        match choice {
            MachineChoice::Local => self.local_version_view(observed, needs_update),
            // Check Now checks SpaceTerm, reads the paired server's facts again over its
            // connection, and asks the server's lane for its version and newest release.
            MachineChoice::Direct(route) => {
                let version = version_text(self.remote_version(&route).or(observed).as_ref());
                let subtitle = match self.remote_available(&route) {
                    Some(available) => available_subtitle(&version, Some(&available)),
                    None => version,
                };
                VersionView {
                    subtitle: subtitle.into(),
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
        let Some(row) = self.core_update.as_ref() else {
            return VersionView {
                subtitle: SharedString::new_static(
                    "This run builds its own SpaceTerm Core, so it has no update.",
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
            (
                "Install to see the SpaceTerm Core version.".to_owned(),
                Vec::new(),
            )
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
    /// Start SpaceTerm Core for a stopped server.
    fn machine_row_view(&self, cx: &App) -> Option<MachineRowView> {
        let choice = self.selected_machine()?;
        let version = self.version_view(cx);
        let (buttons, menu) = self.status_steps(&choice, cx);
        let busy = self.core_progress(&choice).is_some();
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
            |step: &StatusStep| matches!(step, StatusStep::Direct(DirectMachineAction::Cancel));
        let install_choices = self
            .core_service_presentation()
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
                            super::machines::INSTALL_CHOICE_SELECTOR
                        ));
                    } else {
                        item.selector = super::machines::INSTALL_SELECTOR.into();
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
        Some(MachineRowView {
            button,
            busy,
            menu: items,
            restore,
        })
    }

    /// The operation underway on `choice` that changes it, as the Server row names it under its
    /// label. A read isn't one: the state says Checking instead.
    fn core_progress(&self, choice: &MachineChoice) -> Option<SharedString> {
        match choice {
            MachineChoice::Local => {
                if let Some(row) = self.core_update.as_ref()
                    && let Some(text) = row
                        .state
                        .working
                        .and_then(|step| update_progress(step, &row.state))
                {
                    return Some(text.into());
                }
                self.core_service
                    .as_ref()
                    .and_then(|row| row.state.working)
                    .filter(|action| *action != ServiceAction::Check)
                    .map(|action| SharedString::new_static(service_progress(action)))
            }
            MachineChoice::Direct(route) => self.remote_progress(route),
        }
    }

    /// Follows the work underway on the selected server, so the Server row shows it only once it
    /// runs longer than a second. Rendering the row calls this before reading its text.
    pub(super) fn track_core_progress(&mut self, cx: &mut Context<Self>) {
        let (checking, busy) = self.selected_machine().map_or((None, None), |choice| {
            (
                self.core_state(&choice).checking.then(|| choice.clone()),
                self.core_progress(&choice).is_some().then_some(choice),
            )
        });
        self.check_progress.track(checking, cx);
        self.operation_progress.track(busy, cx);
    }

    /// The Server row's line under its label: the work underway once it has run longer than a
    /// second, otherwise the version and any update.
    pub(super) fn core_subtitle(&self, cx: &App) -> SharedString {
        let Some(choice) = self.selected_machine() else {
            return SharedString::default();
        };
        if self
            .operation_progress
            .revealed(cx.background_executor().now())
            && let Some(progress) = self.core_progress(&choice)
        {
            return progress;
        }
        self.version_view(cx).subtitle
    }

    /// The spinner leading the Server row's subtitle while it names work underway.
    pub(super) fn core_progress_indicator(&self, cx: &App) -> Option<AnyElement> {
        let choice = self.selected_machine()?;
        if !self
            .operation_progress
            .revealed(cx.background_executor().now())
        {
            return None;
        }
        let progress = self.core_progress(&choice)?;
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
        let Some(target) = self.selected_machine() else {
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
                .update(cx, |settings, cx| settings.reread_core(&checked, cx))
                .ok()
                .flatten();
            if let Some(facts) = facts {
                facts.await;
            }
            while settings
                .update(cx, |settings, _| settings.core_reading(&checked))
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
    fn reread_core(
        &mut self,
        target: &MachineChoice,
        cx: &mut Context<Self>,
    ) -> Option<gpui::Task<()>> {
        match target {
            MachineChoice::Local => {
                // A read is always allowed, whichever steps the last read offered. The service is
                // read too, so a change made outside Settings shows.
                self.check_core_update(cx);
                self.check_local_core(cx);
                self.check_core_service(cx);
            }
            // The person asked, so the server's lane is read for its Status and update.
            MachineChoice::Direct(route) => self.read_remote(*route, cx),
        }
        self.reread_facts(target, cx)
    }

    /// Whether a read Check Now started for `target` is still underway.
    fn core_reading(&self, target: &MachineChoice) -> bool {
        match target {
            MachineChoice::Local => {
                self.core_update
                    .as_ref()
                    .is_some_and(|row| row.state.working.is_some())
                    || self.local_core_working()
            }
            MachineChoice::Direct(route) => self.remote_work(route) == Some(&Work::Reading),
        }
    }

    fn take_version_step(&mut self, step: VersionStep, cx: &mut Context<Self>) {
        match (step, self.selected_machine()) {
            (VersionStep::Local(UpdateAction::Check), _) | (VersionStep::Observe, _) => {
                self.check_versions(cx)
            }
            // An update asked for while a read runs starts once the read offers it again.
            (VersionStep::Local(UpdateAction::Update), Some(MachineChoice::Local)) => {
                self.request_core_update(cx)
            }
            (VersionStep::Local(action), Some(MachineChoice::Local)) => {
                self.request_update_action(action, cx)
            }
            _ => {}
        }
    }

    /// The Server row's control: the state, at most one button, and a ⋯ menu. A read running
    /// longer than a second shows as Checking in the state's place; the state never moves the
    /// row.
    pub(super) fn render_core_status(
        &mut self,
        appearance: &ChromeAppearance,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(choice) = self.selected_machine() else {
            return div().into_any_element();
        };
        let Some(view) = self.machine_row_view(cx) else {
            return div().into_any_element();
        };
        let name = self.machine_name(&choice);
        let state = self.core_state(&choice);
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
            .core_update
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
            // say, Start SpaceTerm Core gives way to Update.
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
        match self.selected_machine()? {
            MachineChoice::Local => self
                .core_service
                .as_ref()
                .and_then(|row| row.automatic_start_shown()),
            MachineChoice::Direct(_) => None,
        }
    }

    /// The Start automatically row's description, naming when the server starts.
    pub(super) fn auto_start_description(&self) -> SharedString {
        if self.this_computer == "This Mac" {
            SharedString::new_static("Start at login.")
        } else {
            SharedString::new_static("Start SpaceTerm Core when this computer starts.")
        }
    }

    /// Why the last change to automatic start didn't take effect, under its switch.
    pub(super) fn auto_start_error(&self) -> Option<SharedString> {
        match self.selected_machine()? {
            MachineChoice::Local => self
                .core_service
                .as_ref()?
                .automatic_start
                .failure
                .map(|error| SharedString::from(error.to_string())),
            MachineChoice::Direct(_) => None,
        }
    }

    /// Whether the selected server's automatic start can change now: this account's installed
    /// service unless another operation changes it. A change asked for during a read or an
    /// earlier change follows it. Only this computer's service offers it.
    fn automatic_start_changeable(&self) -> bool {
        if self.automatic_start().is_none() {
            return false;
        }
        match self.selected_machine() {
            // A failed read shows the confirmed setting without offering to change it.
            Some(MachineChoice::Local) => self.core_service.as_ref().is_some_and(|row| {
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
        if self.selected_machine() == Some(MachineChoice::Local) {
            self.set_local_automatic_start(enabled, window, cx);
        }
    }

    /// The Start automatically switch. It controls automatic start only; Uninstall stays in the
    /// Status menu.
    pub(super) fn render_auto_start(
        &mut self,
        appearance: &ChromeAppearance,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(value) = self.automatic_start() else {
            // Nothing says whether the server starts by itself, so no switch claims it's off, as
            // Network Access reads while unknown.
            let colors = appearance.host_colors(spaceterm_ui::ControlHost::Card);
            return div()
                .id(SharedString::from(format!("{AUTO_START_SELECTOR}-unknown")))
                .debug_selector(|| format!("{AUTO_START_SELECTOR}-unknown"))
                .role(gpui::accesskit::Role::Label)
                .aria_label("Start automatically")
                .aria_value("Unknown")
                .chrome_text(appearance.typography.style(TextRole::Body))
                .text_color(gpui_color(colors.text_secondary))
                .child("Unknown")
                .into_any_element();
        };
        let changeable = self.automatic_start_changeable();
        let owner = cx.weak_entity();
        Switch::new(AUTO_START_SELECTOR, "Start automatically", value)
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
        StatusStep::Remote(change) => change.selector(),
        StatusStep::UnpairThisDevice => "settings-server-unpair",
        StatusStep::Version(step) => step.selector(),
        StatusStep::InstallBuild(..) => super::machines::INSTALL_CHOICE_SELECTOR,
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
        StatusStep::Remote(Change::Stop) => Some(SharedString::from(format!(
            "Stops {machine} for every client. Start it again on its own computer."
        ))),
        StatusStep::Remote(Change::Restart) => Some(SharedString::from(format!(
            "Restarts {machine}, ending its Terminal Sessions for every client."
        ))),
        StatusStep::Remote(Change::Update) => Some(SharedString::from(format!(
            "Updates {machine}, ending its Terminal Sessions for every client."
        ))),
        _ => None,
    }
}

/// Whether the step leads as the primary button: one that updates the server.
fn status_step_primary(step: &StatusStep) -> bool {
    matches!(step, StatusStep::Version(step) if step.primary())
        || *step == StatusStep::Remote(Change::Update)
}

/// How strongly a step claims the Server row's one button, lowest first. `None` belongs in the
/// ⋯ menu.
fn button_rank(step: &StatusStep) -> Option<u8> {
    match step {
        StatusStep::Version(VersionStep::Local(UpdateAction::Return)) => Some(0),
        StatusStep::Version(VersionStep::Local(UpdateAction::Update))
        | StatusStep::Remote(Change::Update) => Some(1),
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
        || "Couldn\u{2019}t update".to_owned(),
        |candidate| format!("Couldn\u{2019}t update to {candidate}"),
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
        UpdateStep::Update { .. } => {
            named(included, "Updating to", "Updating SpaceTerm Core\u{2026}")
        }
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
        ServiceAction::Check => "Checking SpaceTerm Core\u{2026}",
        ServiceAction::Install => "Installing SpaceTerm Core\u{2026}",
        ServiceAction::Start => "Starting SpaceTerm Core\u{2026}",
        ServiceAction::Stop => "Stopping SpaceTerm Core\u{2026}",
        ServiceAction::Restart => "Restarting SpaceTerm Core\u{2026}",
        ServiceAction::Uninstall => "Uninstalling SpaceTerm Core\u{2026}",
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
        StatusStep::Remote(change) => change.label(),
        StatusStep::UnpairThisDevice => "Unpair\u{2026}",
        StatusStep::Version(step) => step.label(),
        StatusStep::InstallBuild(..) => "Install\u{2026}",
    }
}

#[cfg(test)]
#[path = "machine_card_tests.rs"]
mod tests;
