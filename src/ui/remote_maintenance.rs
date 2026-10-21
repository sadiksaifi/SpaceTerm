//! Remote SpaceTerm Server maintenance as a person asks for it and reads it afterwards.
//!
//! One owner holds every maintenance operation this client submitted for a selected SSH server,
//! together with the service facts its last validated read found. Both surfaces that offer these
//! operations, the sidebar machine heading menu and the per-server detail in Settings > Servers,
//! derive their steps and their words from this one presentation, so the two cannot disagree.
//!
//! Presenting reads a snapshot. It reaches no computer, starts nothing and sends nothing. Only
//! explicit Start starts a server, and a described change is performed only by confirming the
//! exact change the person reviewed.

use gpui::SharedString;

use crate::local_server::service::control::{
    Action, BuildName, UpdateChange, UpdateWording, Wording, build_label, build_labels,
};
use crate::local_server::service::maintenance::{
    MaintenanceFacts, MaintenanceRefusal, OperationKind, OperationOutcome, OperationResolution,
    PreparedChange, RemoteActivation, RemoteActivationPhase, RemoteBuild,
};
use crate::local_server::service::{RetainedInstallation, ServiceStatus};
use crate::ssh::server_connection::{
    ContinuityChange, IncompatibilityKind, InteractionReason, UnavailableKind,
};
use crate::ssh::server_maintenance::{MaintenancePhase, SshMaintenanceFailure, SubmittedKind};
use crate::ssh::server_operations::{RemoteMaintenanceRequest, RemoteOperationState};
use crate::ui::settings_window::server_update::UpdateAction;
use crate::ui::settings_window::servers::{ServiceAction, ServicePhase};
use crate::ui::sidebar_window::form::CaptionTone;

/// Which row of the selected server's detail reports a state and offers its steps.
///
/// The service row reports the server's own state. The update row offers the Update Server
/// journey and reports only what that journey is doing, so one row reports one thing.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum MaintenanceRow {
    Service,
    Update,
}

impl MaintenanceRow {
    /// The prefix each of the row's state badges is named by.
    const fn prefix(self) -> &'static str {
        match self {
            Self::Service => "settings-ssh-server-service-state",
            Self::Update => "settings-ssh-server-update-state",
        }
    }
}

/// Names one row's state, so a test tells the rows and their states apart by geometry.
fn selector(row: MaintenanceRow, state: &str) -> SharedString {
    SharedString::from(format!("{}-{state}", row.prefix()))
}

/// One step the selected server's observed state allows.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RemoteAction {
    /// Reads the service facts on the bound route. Starts, stages and stops nothing.
    Check,
    /// The only step that reverses a deliberate Stop. It ends no work, so it asks nothing.
    Start,
    Stop,
    Restart,
    Uninstall,
    /// Transfers the exact verified server version this SpaceTerm holds, then asks before it
    /// replaces the approved one.
    Update,
    /// Returns to the previous runtime of an update that never became ready.
    Return,
    /// Resolves a submitted update or return the service already received.
    Finish,
    /// Resolves a submitted service operation the service already received.
    CheckAgain,
    /// Abandons a described change, or stops waiting for the work underway.
    Cancel,
    /// Retires a validated result the person has read.
    Acknowledge,
}

impl RemoteAction {
    /// The button and menu label. Every step a local server offers reads the same here.
    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Check => ServiceAction::Check.label(),
            Self::Start => ServiceAction::Start.label(),
            Self::Stop => ServiceAction::Stop.label(),
            Self::Restart => ServiceAction::Restart.label(),
            Self::Uninstall => ServiceAction::Uninstall.label(),
            Self::Update => UpdateAction::Update.label(),
            Self::Return => UpdateAction::Return.label(),
            Self::Finish => UpdateAction::Finish.label(),
            Self::CheckAgain => "Check Again",
            Self::Cancel => crate::ui::ssh_servers::SshServerAction::Cancel.label(),
            Self::Acknowledge => "OK",
        }
    }

    pub(crate) const fn selector(self) -> &'static str {
        match self {
            Self::Check => "settings-ssh-server-service-check",
            Self::Start => "settings-ssh-server-service-start",
            Self::Stop => "settings-ssh-server-service-stop",
            Self::Restart => "settings-ssh-server-service-restart",
            Self::Uninstall => "settings-ssh-server-service-uninstall",
            Self::Update => "settings-ssh-server-update",
            Self::Return => "settings-ssh-server-update-return",
            Self::Finish => "settings-ssh-server-update-finish",
            Self::CheckAgain => "settings-ssh-server-service-again",
            Self::Cancel => "settings-ssh-server-maintenance-cancel",
            Self::Acknowledge => "settings-ssh-server-maintenance-acknowledge",
        }
    }

    /// The slot the button occupies within its row. The steps that replace one another share a
    /// slot, so keyboard focus stays on it as Stop Server gives way to Start Server.
    pub(crate) const fn slot(self) -> &'static str {
        match self {
            Self::Start
            | Self::Stop
            | Self::CheckAgain
            | Self::Acknowledge
            | Self::Update
            | Self::Return
            | Self::Finish => "primary",
            // Cancel appears beside a described step, so it keeps a slot of its own.
            Self::Cancel => "cancel",
            Self::Restart => "secondary",
            Self::Uninstall => "removal",
            Self::Check => "read",
        }
    }

    /// What the step does beyond its name, for assistive technology. `machine` names the computer
    /// every step acts on.
    pub(crate) fn consequence(self, machine: &str) -> SharedString {
        let text = match self {
            Self::Check => {
                format!("Signs in to {machine} and reads its service. It starts nothing.")
            }
            Self::Start => format!("Starts the server on {machine}."),
            Self::Stop | Self::Restart => format!(
                "Ends the Terminal Sessions on {machine} for every client. SpaceTerm asks first."
            ),
            Self::Uninstall => {
                format!("Removes the service on {machine}. SpaceTerm asks first.")
            }
            Self::Update => format!(
                "Transfers the verified server version SpaceTerm includes to {machine}, then asks \
                 before it replaces the approved one."
            ),
            Self::Return => format!(
                "Returns the server on {machine} to its previous version. SpaceTerm asks first."
            ),
            Self::Finish => format!("Completes the update you confirmed on {machine}."),
            Self::CheckAgain => format!(
                "Asks {machine} for the result of the operation it already received. It is not \
                 asked to do it again."
            ),
            Self::Cancel => format!("Stops waiting for {machine}."),
            Self::Acknowledge => format!("Dismisses this result for {machine}."),
        };
        SharedString::from(text)
    }

    /// The row that carries this step, for a surface that offers the steps outside the rows.
    pub(crate) const fn row(self) -> MaintenanceRow {
        match self {
            Self::Update | Self::Return | Self::Finish => MaintenanceRow::Update,
            _ => MaintenanceRow::Service,
        }
    }

    /// The step's name in a menu's debug selector.
    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::Check => "check",
            Self::Start => "start",
            Self::Stop => "stop",
            Self::Restart => "restart",
            Self::Uninstall => "uninstall",
            Self::Update => "update",
            Self::Return => "return",
            Self::Finish => "finish",
            Self::CheckAgain => "check-again",
            Self::Cancel => "cancel",
            Self::Acknowledge => "acknowledge",
        }
    }

    /// What this step asks the owner to describe. A step that describes nothing performs itself,
    /// so only a step with a request asks the person first.
    pub(crate) const fn request(self) -> Option<RemoteMaintenanceRequest> {
        match self {
            Self::Stop => Some(RemoteMaintenanceRequest::Service(Action::Stop)),
            Self::Restart => Some(RemoteMaintenanceRequest::Service(Action::Restart)),
            Self::Uninstall => Some(RemoteMaintenanceRequest::Service(Action::Uninstall)),
            Self::Update => Some(RemoteMaintenanceRequest::Update),
            Self::Return => Some(RemoteMaintenanceRequest::Return),
            _ => None,
        }
    }
}

/// One row's complete presentation, derived from the owner's snapshot alone.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct MaintenancePresentation {
    /// The state badge, or the progress label while [`Self::working`]. `None` for a row with
    /// nothing of its own to report.
    pub(crate) state: Option<&'static str>,
    /// Names the state badge, so a test can tell each state apart by geometry.
    pub(crate) state_selector: SharedString,
    pub(crate) explanation: SharedString,
    pub(crate) tone: CaptionTone,
    /// The steps the row offers, in the order it shows them.
    pub(crate) actions: Vec<RemoteAction>,
    /// Whether work is underway for this row. Exactly one row reports it.
    pub(crate) working: bool,
}

/// The presentation of one server's maintenance state in one row.
///
/// `machine` names the selected computer. `asked` is the change this surface asked the owner to
/// describe, which tells the rows apart while a description is still underway.
pub(crate) fn present(
    machine: &str,
    state: &RemoteOperationState,
    row: MaintenanceRow,
    asked: Option<RemoteMaintenanceRequest>,
) -> MaintenancePresentation {
    match state {
        RemoteOperationState::Unread => unread(machine, row),
        RemoteOperationState::Read(facts) => read(machine, facts, row),
        state => {
            if owning_row(state, asked) == row {
                submitted(machine, state, row)
            } else {
                bystander(machine, state, row)
            }
        }
    }
}

/// The steps a machine heading's menu offers, in menu order. The menu carries the operations
/// themselves, never the everyday acknowledgement or the cancellation an inline row offers.
pub(crate) fn menu_actions(
    state: &RemoteOperationState,
    asked: Option<RemoteMaintenanceRequest>,
) -> Vec<RemoteAction> {
    let offered = |row| present("", state, row, asked).actions;
    let mut actions: Vec<_> = offered(MaintenanceRow::Service)
        .into_iter()
        .chain(offered(MaintenanceRow::Update))
        .filter(|action| {
            !matches!(
                action,
                RemoteAction::Check | RemoteAction::Cancel | RemoteAction::Acknowledge
            )
        })
        .collect();
    // Removing the service reads last, after the steps that keep it.
    actions.sort_by_key(|action| u8::from(*action == RemoteAction::Uninstall));
    actions
}

/// The steps a server whose saved connection was removed offers: resolving the original it still
/// holds and acknowledging a validated result. Nothing here reconnects or reads the machine.
pub(crate) fn removed_actions(
    state: &RemoteOperationState,
    row: MaintenanceRow,
) -> Vec<RemoteAction> {
    present("", state, row, None)
        .actions
        .into_iter()
        .filter(|action| {
            matches!(
                action,
                RemoteAction::Finish | RemoteAction::CheckAgain | RemoteAction::Acknowledge
            )
        })
        .collect()
}

/// The row that reports a submitted operation: the one whose journey the person asked for.
fn owning_row(
    state: &RemoteOperationState,
    asked: Option<RemoteMaintenanceRequest>,
) -> MaintenanceRow {
    let of_kind = |kind: &SubmittedKind| match kind {
        SubmittedKind::Update { .. } | SubmittedKind::Return => MaintenanceRow::Update,
        SubmittedKind::Start | SubmittedKind::Service(_) => MaintenanceRow::Service,
    };
    let of_request = |request: RemoteMaintenanceRequest| match request {
        RemoteMaintenanceRequest::Update | RemoteMaintenanceRequest::Return => {
            MaintenanceRow::Update
        }
        RemoteMaintenanceRequest::Service(_) => MaintenanceRow::Service,
    };
    let submitted = match state {
        RemoteOperationState::Working { kind, .. }
        | RemoteOperationState::NeedsPerson { kind, .. }
        | RemoteOperationState::Paused { kind, .. }
        | RemoteOperationState::Failed { kind, .. } => kind.as_ref().map(of_kind),
        RemoteOperationState::Resolved { kind, .. }
        | RemoteOperationState::Unconfirmed { kind, .. } => Some(of_kind(kind)),
        RemoteOperationState::Described { request, .. } => Some(of_request(*request)),
        RemoteOperationState::Unread | RemoteOperationState::Read(_) => None,
    };
    submitted
        .or_else(|| asked.map(of_request))
        // Only an update transfers an artifact, so transferring names its journey on its own.
        .unwrap_or(match state {
            RemoteOperationState::Working {
                phase: MaintenancePhase::Transferring,
                ..
            } => MaintenanceRow::Update,
            _ => MaintenanceRow::Service,
        })
}

fn guidance(text: String) -> (SharedString, CaptionTone) {
    (SharedString::from(text), CaptionTone::Guidance)
}

fn warning(text: String) -> (SharedString, CaptionTone) {
    (SharedString::from(text), CaptionTone::Warning)
}

fn failure(text: String) -> (SharedString, CaptionTone) {
    (SharedString::from(text), CaptionTone::Error)
}

/// A row with nothing of its own to report while the other row reports the operation.
fn bystander(
    machine: &str,
    state: &RemoteOperationState,
    row: MaintenanceRow,
) -> MaintenancePresentation {
    let (explanation, tone) = match (row, state) {
        (_, RemoteOperationState::Working { .. } | RemoteOperationState::Described { .. }) => {
            guidance(format!("An operation on {machine} is underway."))
        }
        (MaintenanceRow::Service, _) => guidance(format!(
            "An update you confirmed on {machine} hasn\u{2019}t finished. Choose Finish Update \
             under Server update."
        )),
        (MaintenanceRow::Update, _) => guidance(format!(
            "An operation you confirmed on {machine} hasn\u{2019}t finished. Choose Check Again \
             under Background service."
        )),
    };
    MaintenancePresentation {
        state: None,
        state_selector: selector(row, "other"),
        explanation,
        tone,
        actions: Vec::new(),
        working: false,
    }
}

fn unread(machine: &str, row: MaintenanceRow) -> MaintenancePresentation {
    let (state, explanation, actions) = match row {
        MaintenanceRow::Service => (
            Some("Not Checked"),
            format!(
                "SpaceTerm hasn\u{2019}t read the service on {machine}. Choose Check Now to sign \
                 in and read it. Reading starts nothing."
            ),
            vec![RemoteAction::Check],
        ),
        MaintenanceRow::Update => (
            None,
            format!("SpaceTerm reads the server version on {machine} when you choose Check Now."),
            Vec::new(),
        ),
    };
    let (explanation, tone) = guidance(explanation);
    MaintenancePresentation {
        state,
        state_selector: selector(row, "not-checked"),
        explanation,
        tone,
        actions,
        working: false,
    }
}

/// What the last validated read found, in the row that reports it.
fn read(machine: &str, facts: &MaintenanceFacts, row: MaintenanceRow) -> MaintenancePresentation {
    match row {
        MaintenanceRow::Service => read_service(machine, facts),
        MaintenanceRow::Update => read_update(machine, facts),
    }
}

fn read_service(machine: &str, facts: &MaintenanceFacts) -> MaintenancePresentation {
    let phase = ServicePhase::of(facts.status);
    let (state, state_selector, (explanation, tone)) = match phase {
        ServicePhase::NotInstalled => (
            "Not Installed",
            selector(MaintenanceRow::Service, "not-installed"),
            guidance(format!(
                "{machine} has no SpaceTerm server for this account. Connect offers to install a \
                 verified one."
            )),
        ),
        ServicePhase::Running => (
            "Running",
            selector(MaintenanceRow::Service, "running"),
            guidance(format!(
                "The server on {machine} is running, and it keeps running after SpaceTerm quits."
            )),
        ),
        ServicePhase::Stopped => (
            "Stopped",
            selector(MaintenanceRow::Service, "stopped"),
            guidance(format!(
                "The server on {machine} was stopped. It stays stopped, even after {machine} \
                 restarts, until you choose Start Server."
            )),
        ),
        ServicePhase::NotRunning => (
            "Not Running",
            selector(MaintenanceRow::Service, "not-running"),
            warning(format!(
                "The server on {machine} is installed but not running. Choose Start Server to \
                 start it."
            )),
        ),
    };
    // Installing a first server belongs to Connect, which offers the verified build and explains
    // what it does on that computer.
    let mut actions: Vec<_> = phase
        .actions()
        .iter()
        .filter_map(|action| match action {
            ServiceAction::Start => Some(RemoteAction::Start),
            ServiceAction::Stop => Some(RemoteAction::Stop),
            ServiceAction::Restart => Some(RemoteAction::Restart),
            ServiceAction::Uninstall => Some(RemoteAction::Uninstall),
            _ => None,
        })
        .collect();
    actions.push(RemoteAction::Check);
    MaintenancePresentation {
        state: Some(state),
        state_selector,
        explanation,
        tone,
        actions,
        working: false,
    }
}

fn read_update(machine: &str, facts: &MaintenanceFacts) -> MaintenancePresentation {
    if let Some(activation) = facts
        .activation
        .as_ref()
        .filter(|activation| !activation.returned)
        && let Some(presentation) = unresolved_activation(machine, activation)
    {
        return presentation;
    }
    if !facts.status.installed {
        let (explanation, tone) = guidance(format!(
            "{machine} has no SpaceTerm server for this account, so there is nothing to update."
        ));
        return MaintenancePresentation {
            state: None,
            state_selector: selector(MaintenanceRow::Update, "available"),
            explanation,
            tone,
            actions: Vec::new(),
            working: false,
        };
    }
    let approved = facts
        .approved
        .as_ref()
        .map(|approved| format!("Version {} is approved on {machine}. ", build(approved)))
        .unwrap_or_default();
    let effect = if facts.status.running {
        format!(
            "Updating ends the Terminal Sessions on {machine} for every client, so SpaceTerm asks \
             first."
        )
    } else if facts.status.stopped {
        "Updating keeps the server stopped until you choose Start Server.".to_owned()
    } else {
        "The server isn\u{2019}t running, so updating ends no Terminal Sessions.".to_owned()
    };
    let (explanation, tone) = guidance(format!(
        "{approved}Update Server transfers the exact verified server version SpaceTerm includes, \
         then asks before it replaces the approved one. {effect}"
    ));
    MaintenancePresentation {
        state: None,
        state_selector: selector(MaintenanceRow::Update, "available"),
        explanation,
        tone,
        actions: vec![RemoteAction::Update],
        working: false,
    }
}

/// An update the service records and has not resolved, or `None` once it completed.
fn unresolved_activation(
    machine: &str,
    activation: &RemoteActivation,
) -> Option<MaintenancePresentation> {
    let (previous, candidate) = builds(&activation.previous, &activation.candidate);
    let (state, state_selector, (explanation, tone), actions) = match activation.phase {
        RemoteActivationPhase::Completed { .. } => return None,
        RemoteActivationPhase::Activating => (
            "Update Unfinished",
            selector(MaintenanceRow::Update, "unfinished"),
            warning(format!(
                "The update of {machine} from version {previous} to version {candidate} \
                 hasn\u{2019}t finished. Choose Finish Update to complete the update you \
                 confirmed. SpaceTerm doesn\u{2019}t ask again."
            )),
            vec![RemoteAction::Finish],
        ),
        RemoteActivationPhase::Uncertain => (
            "Update Uncertain",
            selector(MaintenanceRow::Update, "uncertain"),
            failure(format!(
                "SpaceTerm couldn\u{2019}t confirm whether the server on {machine} changed from \
                 version {previous} to version {candidate}. Choose Finish Update to check the \
                 same update again."
            )),
            vec![RemoteAction::Finish],
        ),
        RemoteActivationPhase::Failed { can_return: true } => (
            "Update Failed",
            selector(MaintenanceRow::Update, "failed"),
            failure(format!(
                "Version {candidate} never became ready on {machine}, and the server\u{2019}s \
                 Terminal Sessions ended. Version {previous} and its saved Workspaces are \
                 unchanged, so you can return to version {previous}. Terminal Sessions that ended \
                 don\u{2019}t return."
            )),
            vec![RemoteAction::Return],
        ),
        RemoteActivationPhase::Failed { can_return: false } => (
            "Update Failed",
            selector(MaintenanceRow::Update, "failed"),
            failure(format!(
                "Version {candidate} never became ready on {machine}, and the server\u{2019}s \
                 Terminal Sessions ended. SpaceTerm couldn\u{2019}t confirm that its saved \
                 Workspaces are unchanged, so it can\u{2019}t return to version {previous}. \
                 Terminal Sessions that ended don\u{2019}t return."
            )),
            Vec::new(),
        ),
    };
    Some(MaintenancePresentation {
        state: Some(state),
        state_selector,
        explanation,
        tone,
        actions,
        working: false,
    })
}

/// A submitted, described or failed operation in the row that reports it.
fn submitted(
    machine: &str,
    state: &RemoteOperationState,
    row: MaintenanceRow,
) -> MaintenancePresentation {
    let (badge, state_selector, (explanation, tone), actions, working) = match state {
        RemoteOperationState::Unread | RemoteOperationState::Read(_) => unreachable!(),
        RemoteOperationState::Working {
            phase,
            kind,
            cancellable,
        } => (
            Some(working_badge(*phase, kind.as_ref())),
            selector(row, "working"),
            guidance(working_detail(machine, *phase, kind.as_ref())),
            if *cancellable {
                vec![RemoteAction::Cancel]
            } else {
                Vec::new()
            },
            true,
        ),
        RemoteOperationState::Described { request, .. } => {
            let step = request_action(*request);
            (
                Some("Waiting for You"),
                selector(row, "confirm"),
                guidance(format!(
                    "Nothing has been sent to {machine}. Choose {} to review what it affects.",
                    plain(step.label())
                )),
                vec![step, RemoteAction::Cancel],
                false,
            )
        }
        RemoteOperationState::Resolved { kind, outcome, .. } => {
            resolved(machine, kind, outcome, row)
        }
        RemoteOperationState::Unconfirmed { kind, .. } => (
            Some("Unconfirmed"),
            selector(row, "unconfirmed"),
            warning(format!(
                "SpaceTerm didn\u{2019}t hear whether {machine} {}. Choose {} to ask it for the \
                 answer; it isn\u{2019}t asked to do it again.",
                did(kind),
                plain(resolve_action(kind).label())
            )),
            vec![resolve_action(kind)],
            false,
        ),
        RemoteOperationState::NeedsPerson { reason, kind } => {
            let step = kind.as_ref().map_or(RemoteAction::Check, resolve_action);
            (
                Some("Waiting for You"),
                selector(row, "waiting"),
                guidance(format!(
                    "{} Choose {} to continue; SpaceTerm doesn\u{2019}t ask in the background.",
                    interaction(*reason, machine),
                    plain(step.label())
                )),
                vec![step],
                false,
            )
        }
        RemoteOperationState::Paused { reason, kind } => (
            Some("Paused"),
            selector(row, "paused"),
            failure(format!(
                "{} Nothing was sent to it.",
                continuity(*reason, machine)
            )),
            vec![kind.as_ref().map_or(RemoteAction::Check, resolve_action)],
            false,
        ),
        RemoteOperationState::Failed { failure, kind } => failed(machine, *failure, kind, row),
    };
    MaintenancePresentation {
        state: badge,
        state_selector,
        explanation,
        tone,
        actions,
        working,
    }
}

fn resolved(
    machine: &str,
    kind: &SubmittedKind,
    outcome: &OperationOutcome,
    row: MaintenanceRow,
) -> Parts {
    match outcome.resolution {
        OperationResolution::Completed => (
            Some(completed_badge(kind, outcome.status)),
            selector(row, "done"),
            guidance(completed_detail(machine, &outcome.kind, outcome.status)),
            vec![RemoteAction::Acknowledge],
            false,
        ),
        OperationResolution::Admitted => (
            Some("Underway"),
            selector(row, "underway"),
            guidance(format!(
                "{machine} accepted the operation and it is still underway. Choose {} to ask for \
                 its result.",
                plain(resolve_action(kind).label())
            )),
            vec![resolve_action(kind)],
            false,
        ),
        OperationResolution::Refused(reason) => (
            Some("Refused"),
            selector(row, "refused"),
            failure(format!(
                "{} Nothing changed on {machine}.",
                refusal(reason, machine)
            )),
            vec![RemoteAction::Acknowledge],
            false,
        ),
        OperationResolution::Unconfirmed => (
            Some("Unconfirmed"),
            selector(row, "unconfirmed"),
            warning(format!(
                "SpaceTerm didn\u{2019}t hear whether {machine} {}. Choose {} to ask it for the \
                 answer; it isn\u{2019}t asked to do it again.",
                did(kind),
                plain(resolve_action(kind).label())
            )),
            vec![resolve_action(kind)],
            false,
        ),
        OperationResolution::Failed { can_return } => {
            let mut actions = Vec::new();
            if can_return {
                actions.push(RemoteAction::Return);
            }
            actions.push(RemoteAction::Acknowledge);
            let detail = if can_return {
                format!(
                    "The new version never became ready on {machine}, and the server\u{2019}s \
                     Terminal Sessions ended. Its previous version and saved Workspaces are \
                     unchanged, so you can return to it. Terminal Sessions that ended \
                     don\u{2019}t return."
                )
            } else {
                format!(
                    "The new version never became ready on {machine}, and the server\u{2019}s \
                     Terminal Sessions ended. SpaceTerm couldn\u{2019}t confirm that its saved \
                     Workspaces are unchanged, so it can\u{2019}t return to the previous version."
                )
            };
            (
                Some("Update Failed"),
                selector(row, "failed"),
                failure(detail),
                actions,
                false,
            )
        }
    }
}

fn failed(
    machine: &str,
    reason: SshMaintenanceFailure,
    kind: &Option<SubmittedKind>,
    row: MaintenanceRow,
) -> Parts {
    let resolve = kind.as_ref().map(resolve_action);
    let again = |step: Option<RemoteAction>| step.map_or_else(Vec::new, |step| vec![step]);
    match reason {
        SshMaintenanceFailure::Cancelled => (
            Some("Cancelled"),
            selector(row, "cancelled"),
            warning(match &resolve {
                Some(step) => format!(
                    "You stopped waiting for {machine}. A step it already accepted may still take \
                     effect. Choose {} to ask for the result.",
                    plain(step.label())
                ),
                None => format!("You stopped waiting for {machine}. Nothing was sent to it."),
            }),
            again(resolve),
            false,
        ),
        SshMaintenanceFailure::NeedsPerson(reason) => (
            Some("Waiting for You"),
            selector(row, "waiting"),
            guidance(format!(
                "{} Choose {} to continue; SpaceTerm doesn\u{2019}t ask in the background.",
                interaction(reason, machine),
                plain(resolve.unwrap_or(RemoteAction::Check).label())
            )),
            vec![resolve.unwrap_or(RemoteAction::Check)],
            false,
        ),
        SshMaintenanceFailure::Changed(change) => (
            Some("Paused"),
            selector(row, "paused"),
            failure(format!(
                "{} Nothing was sent to it.",
                continuity(change, machine)
            )),
            vec![resolve.unwrap_or(RemoteAction::Check)],
            false,
        ),
        SshMaintenanceFailure::Refused(reason) => (
            Some("Refused"),
            selector(row, "refused"),
            failure(format!(
                "{} Nothing changed on {machine}.",
                refusal(reason, machine)
            )),
            vec![RemoteAction::Check],
            false,
        ),
        SshMaintenanceFailure::Incompatible(kind) => (
            Some("Incompatible"),
            selector(row, "incompatible"),
            failure(incompatibility(kind, machine)),
            vec![RemoteAction::Check],
            false,
        ),
        SshMaintenanceFailure::Unavailable(kind) => (
            Some("Unavailable"),
            selector(row, "unavailable"),
            failure(format!(
                "{} {}",
                unavailable(kind, machine),
                match &resolve {
                    Some(step) => format!("Choose {} when it answers again.", plain(step.label())),
                    None => "Choose Check Now when it answers again.".to_owned(),
                }
            )),
            vec![resolve.unwrap_or(RemoteAction::Check)],
            false,
        ),
        SshMaintenanceFailure::Unconfirmed => (
            Some("Unconfirmed"),
            selector(row, "unconfirmed"),
            warning(match (&resolve, kind) {
                (Some(step), Some(kind)) => format!(
                    "SpaceTerm didn\u{2019}t hear whether {machine} {}. Choose {} to ask it for \
                     the answer; it isn\u{2019}t asked to do it again.",
                    did(kind),
                    plain(step.label())
                ),
                _ => format!("SpaceTerm didn\u{2019}t hear from {machine}."),
            }),
            again(resolve),
            false,
        ),
        SshMaintenanceFailure::Forgotten => (
            Some("No Record"),
            selector(row, "forgotten"),
            warning(format!(
                "{machine} has no record of the operation SpaceTerm submitted, so SpaceTerm \
                 can\u{2019}t tell whether it took effect. Nothing was asked again."
            )),
            vec![RemoteAction::Check],
            false,
        ),
    }
}

/// A badge, its selector, its caption, its steps and whether work is underway.
type Parts = (
    Option<&'static str>,
    SharedString,
    (SharedString, CaptionTone),
    Vec<RemoteAction>,
    bool,
);

/// The step that performs the change a request describes.
const fn request_action(request: RemoteMaintenanceRequest) -> RemoteAction {
    match request {
        RemoteMaintenanceRequest::Service(Action::Stop) => RemoteAction::Stop,
        RemoteMaintenanceRequest::Service(Action::Restart) => RemoteAction::Restart,
        RemoteMaintenanceRequest::Service(Action::Uninstall) => RemoteAction::Uninstall,
        RemoteMaintenanceRequest::Update => RemoteAction::Update,
        RemoteMaintenanceRequest::Return => RemoteAction::Return,
    }
}

/// The step that resolves one submitted operation. An update keeps the name the local journey
/// uses; a service operation asks the machine again.
const fn resolve_action(kind: &SubmittedKind) -> RemoteAction {
    match kind {
        SubmittedKind::Update { .. } | SubmittedKind::Return => RemoteAction::Finish,
        SubmittedKind::Start | SubmittedKind::Service(_) => RemoteAction::CheckAgain,
    }
}

/// A label without its trailing ellipsis, for use inside a sentence.
fn plain(label: &'static str) -> &'static str {
    label.trim_end_matches('\u{2026}')
}

/// What the submitted operation asked the machine to do, as a sentence completes it.
const fn did(kind: &SubmittedKind) -> &'static str {
    match kind {
        SubmittedKind::Start => "started its server",
        SubmittedKind::Service(Action::Stop) => "stopped its server",
        SubmittedKind::Service(Action::Restart) => "restarted its server",
        SubmittedKind::Service(Action::Uninstall) => "removed its service",
        SubmittedKind::Update { .. } => "updated its server",
        SubmittedKind::Return => "returned its server to the previous version",
    }
}

/// The progress badge, in the words the local surfaces use for the same work.
const fn working_badge(phase: MaintenancePhase, kind: Option<&SubmittedKind>) -> &'static str {
    match (phase, kind) {
        (MaintenancePhase::Authenticating, _) => "Signing In",
        (MaintenancePhase::Validating, _) => "Checking",
        (MaintenancePhase::Transferring, _) => "Transferring",
        (MaintenancePhase::Resolving, _) => "Checking Result",
        (MaintenancePhase::Dispatching, None) => "Preparing",
        (MaintenancePhase::Dispatching, Some(SubmittedKind::Start)) => {
            ServiceAction::Start.progress(false).0
        }
        (MaintenancePhase::Dispatching, Some(SubmittedKind::Service(Action::Stop))) => {
            ServiceAction::Stop.progress(false).0
        }
        (MaintenancePhase::Dispatching, Some(SubmittedKind::Service(Action::Restart))) => {
            ServiceAction::Restart.progress(false).0
        }
        (MaintenancePhase::Dispatching, Some(SubmittedKind::Service(Action::Uninstall))) => {
            ServiceAction::Uninstall.progress(false).0
        }
        (MaintenancePhase::Dispatching, Some(SubmittedKind::Update { .. })) => "Updating",
        (MaintenancePhase::Dispatching, Some(SubmittedKind::Return)) => "Returning",
    }
}

fn working_detail(machine: &str, phase: MaintenancePhase, kind: Option<&SubmittedKind>) -> String {
    match (phase, kind) {
        (MaintenancePhase::Authenticating, _) => format!(
            "Signing in to {machine} on the route this operation uses. Answer any request from \
             SSH to continue."
        ),
        (MaintenancePhase::Validating, _) => format!(
            "Checking that {machine} is the same computer, account and server as before\u{2026}"
        ),
        (MaintenancePhase::Transferring, _) => format!(
            "Transferring the verified server version SpaceTerm includes to {machine}\u{2026}"
        ),
        (MaintenancePhase::Resolving, _) => {
            format!("Asking {machine} for the result of the operation it already received\u{2026}")
        }
        (MaintenancePhase::Dispatching, None) => {
            format!("Asking {machine} what the change affects\u{2026}")
        }
        (MaintenancePhase::Dispatching, Some(SubmittedKind::Start)) => {
            format!("Starting the server on {machine}\u{2026}")
        }
        (MaintenancePhase::Dispatching, Some(SubmittedKind::Service(Action::Stop))) => {
            format!("Stopping the server on {machine}. Its Terminal Sessions end\u{2026}")
        }
        (MaintenancePhase::Dispatching, Some(SubmittedKind::Service(Action::Restart))) => {
            format!("Restarting the server on {machine}. Its Terminal Sessions end\u{2026}")
        }
        (MaintenancePhase::Dispatching, Some(SubmittedKind::Service(Action::Uninstall))) => {
            format!("Removing the service on {machine}\u{2026}")
        }
        (MaintenancePhase::Dispatching, Some(SubmittedKind::Update { .. })) => {
            format!("Updating the server on {machine}. Its Terminal Sessions end\u{2026}")
        }
        (MaintenancePhase::Dispatching, Some(SubmittedKind::Return)) => {
            format!("Returning the server on {machine} to its previous version\u{2026}")
        }
    }
}

/// The badge of a completed operation: what the machine's service is now.
fn completed_badge(kind: &SubmittedKind, status: Option<ServiceStatus>) -> &'static str {
    match (status.map(ServicePhase::of), kind) {
        (Some(ServicePhase::Running), _) => "Running",
        (Some(ServicePhase::Stopped), _) => "Stopped",
        (Some(ServicePhase::NotRunning), _) => "Not Running",
        (Some(ServicePhase::NotInstalled), _) => "Not Installed",
        (None, SubmittedKind::Update { .. }) => "Updated",
        (None, SubmittedKind::Return) => "Returned",
        (None, _) => "Done",
    }
}

fn completed_detail(machine: &str, kind: &OperationKind, status: Option<ServiceStatus>) -> String {
    let kept = "Its Workspaces and each client\u{2019}s attachments and views are kept, and each \
                Pane starts a new shell when you next use it.";
    let stays_stopped = status.is_some_and(|status| status.stopped && !status.running);
    let detail = match kind {
        OperationKind::Start => format!("The server on {machine} started."),
        OperationKind::Service {
            action: Action::Stop,
        } => format!(
            "The server on {machine} stopped, and its Terminal Sessions ended for every client. \
             Its Workspaces are kept. It stays stopped until you choose Start Server."
        ),
        OperationKind::Service {
            action: Action::Restart,
        } => format!(
            "The server on {machine} restarted, and its Terminal Sessions ended for every client. \
             {kept}"
        ),
        OperationKind::Service {
            action: Action::Uninstall,
        } => format!(
            "The service on {machine} was removed, and its Terminal Sessions ended for every \
             client. The server\u{2019}s Workspaces and verified installations are kept, so it \
             can be installed again."
        ),
        OperationKind::Update { candidate } => format!(
            "The server on {machine} now runs version {}, and its Terminal Sessions ended for \
             every client. {kept}",
            build(candidate)
        ),
        OperationKind::Return { previous } => format!(
            "The server on {machine} returned to version {}. Terminal Sessions that ended \
             don\u{2019}t return.",
            build(previous)
        ),
    };
    let stopped = match kind {
        OperationKind::Update { .. } | OperationKind::Return { .. } if stays_stopped => {
            format!(" The server on {machine} stays stopped until you choose Start Server.")
        }
        _ => String::new(),
    };
    format!("{detail}{stopped}")
}

fn interaction(reason: InteractionReason, machine: &str) -> String {
    match reason {
        InteractionReason::Authentication => {
            format!("SSH needs you to sign in to {machine} again.")
        }
        InteractionReason::HostVerification => {
            format!("SSH needs you to confirm {machine}\u{2019}s identity.")
        }
        InteractionReason::CredentialProvider => {
            format!("Your SSH key or credential provider needs your approval to reach {machine}.")
        }
    }
}

fn continuity(change: ContinuityChange, machine: &str) -> String {
    match change {
        ContinuityChange::HostChanged => {
            format!(
                "\u{201C}{machine}\u{201D} now reaches a different computer, so SpaceTerm stopped."
            )
        }
        ContinuityChange::AccountChanged => format!(
            "\u{201C}{machine}\u{201D} now signs in to a different account, so SpaceTerm stopped."
        ),
        ContinuityChange::ServerChanged => format!(
            "The SpaceTerm server for this account on {machine} is a different server, so \
             SpaceTerm stopped."
        ),
        ContinuityChange::BuildChanged => format!(
            "The SpaceTerm server for this account on {machine} is a different version, so \
             SpaceTerm stopped."
        ),
    }
}

fn refusal(reason: MaintenanceRefusal, machine: &str) -> String {
    match reason {
        MaintenanceRefusal::NotInstalled => {
            format!("{machine} has no SpaceTerm server for this account.")
        }
        MaintenanceRefusal::NotRunning => {
            format!("The server on {machine} isn\u{2019}t running, and this needed it running.")
        }
        MaintenanceRefusal::Stopped => {
            format!("The server on {machine} was stopped. Only Start Server reverses that.")
        }
        MaintenanceRefusal::Changed => format!(
            "The server on {machine} changed after SpaceTerm described the change, so it refused \
             it. Review it and confirm again."
        ),
        MaintenanceRefusal::AuthorizationRequired => format!(
            "{machine} needs an authorization its service doesn\u{2019}t have. Grant it on that \
             computer, then try again."
        ),
        MaintenanceRefusal::CandidateUnavailable => {
            "SpaceTerm has no verified server version for that computer.".to_owned()
        }
        MaintenanceRefusal::CandidateChanged => format!(
            "The server version SpaceTerm offered doesn\u{2019}t verify for {machine}, so nothing \
             was installed."
        ),
        MaintenanceRefusal::ReturnUnavailable => format!(
            "SpaceTerm couldn\u{2019}t prove that returning the server on {machine} to its \
             previous version is safe."
        ),
        MaintenanceRefusal::StorageUnavailable => {
            format!("The service storage on {machine} is unavailable or unsafely owned.")
        }
        MaintenanceRefusal::Protocol => {
            format!("The service on {machine} answered something SpaceTerm couldn\u{2019}t read.")
        }
        MaintenanceRefusal::Unsupported => format!(
            "The server on {machine} doesn\u{2019}t offer this operation. Update it to use this."
        ),
    }
}

fn incompatibility(kind: IncompatibilityKind, machine: &str) -> String {
    match kind {
        IncompatibilityKind::Protocol => format!(
            "SpaceTerm and the server for this account on {machine} have no protocol version in \
             common, so SpaceTerm sent nothing. The server and its Terminal Sessions keep running."
        ),
        IncompatibilityKind::Capability => format!(
            "The server for this account on {machine} doesn\u{2019}t support a feature this \
             SpaceTerm requires, so SpaceTerm sent nothing. The server and its Terminal Sessions \
             keep running."
        ),
        IncompatibilityKind::Platform => format!(
            "SpaceTerm has no verified server for {machine}\u{2019}s operating system or \
             processor. Nothing was installed."
        ),
    }
}

fn unavailable(kind: UnavailableKind, machine: &str) -> String {
    match kind {
        UnavailableKind::LocalIpcUnavailable => format!(
            "SpaceTerm couldn\u{2019}t prepare its SSH connection on this computer, so it \
             didn\u{2019}t reach {machine}. Nothing changed there."
        ),
        UnavailableKind::Network => "The network is unavailable.".to_owned(),
        UnavailableKind::HostUnreachable => format!("SpaceTerm couldn\u{2019}t reach {machine}."),
        UnavailableKind::SshFailed => format!(
            "SSH couldn\u{2019}t connect to \u{201C}{machine}\u{201D}. Check your SSH \
             configuration."
        ),
        UnavailableKind::ServiceUnavailable => {
            format!("The server for this account on {machine} didn\u{2019}t answer.")
        }
        UnavailableKind::CandidateUnavailable => {
            "This SpaceTerm has no verified server version for that computer.".to_owned()
        }
    }
}

/// Names one build by version and abbreviated commit, as every control surface does.
fn build(build: &RemoteBuild) -> String {
    build_label(&build.version, &build.commit)
}

/// Names two builds so they read apart, as every control surface does.
fn builds(previous: &RemoteBuild, candidate: &RemoteBuild) -> (String, String) {
    build_labels(name(previous), name(candidate))
}

fn name(build: &RemoteBuild) -> BuildName<'_> {
    BuildName {
        version: build.version.as_str(),
        commit: build.commit.as_str(),
        package: build.digest.as_str(),
    }
}

/// The confirmation copy for one described change, in the wording every control surface shows.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct MaintenanceConfirmation {
    pub(crate) title: &'static str,
    pub(crate) message: SharedString,
    pub(crate) detail: SharedString,
    pub(crate) confirm: &'static str,
}

impl MaintenanceConfirmation {
    /// The confirmation for `request` as `change` describes it, naming `machine`. `None` for a
    /// request that ends no work and so asks nothing.
    pub(crate) fn of(
        machine: &str,
        request: RemoteMaintenanceRequest,
        change: &PreparedChange,
    ) -> Option<Self> {
        match request {
            RemoteMaintenanceRequest::Service(action) => {
                let wording = Wording::new(machine, action, change.running, change.startup);
                Some(Self {
                    title: wording.title(),
                    message: wording.message().into(),
                    detail: wording.detail().into(),
                    confirm: wording.confirm_label(),
                })
            }
            RemoteMaintenanceRequest::Update | RemoteMaintenanceRequest::Return => {
                let previous = change.previous.as_ref().map(installation)?;
                let candidate = change.candidate.as_ref().map(installation)?;
                let update = match request {
                    RemoteMaintenanceRequest::Return => UpdateChange::Return,
                    _ => UpdateChange::Update,
                };
                let wording = UpdateWording::new(
                    machine,
                    update,
                    &previous,
                    &candidate,
                    change.running,
                    change.stopped,
                );
                Some(Self {
                    title: wording.title(),
                    message: wording.message().into(),
                    detail: wording.detail().into(),
                    confirm: wording.confirm_label(),
                })
            }
        }
    }
}

/// What a server whose saved connection was removed still has to tell the person, and the one
/// step it offers. `None` once nothing is left to tell, so a removed connection disappears as
/// soon as its operation is settled.
pub(crate) fn removed_presentation(
    machine: &str,
    state: &RemoteOperationState,
) -> Option<MaintenancePresentation> {
    [MaintenanceRow::Service, MaintenanceRow::Update]
        .into_iter()
        .find_map(|row| {
            let actions = removed_actions(state, row);
            (!actions.is_empty()).then(|| MaintenancePresentation {
                actions,
                ..present(machine, state, row, None)
            })
        })
}

/// The progress badge a machine heading shows while an operation on that machine is underway.
pub(crate) fn working_state(machine: &str, state: &RemoteOperationState) -> Option<&'static str> {
    [MaintenanceRow::Service, MaintenanceRow::Update]
        .into_iter()
        .find_map(|row| {
            let presentation = present(machine, state, row, None);
            presentation.working.then_some(presentation.state).flatten()
        })
}

/// Continues the operations this client submitted and never resolved, quietly and once, after a
/// Workspace Window exists. It uses available noninteractive authority only.
pub(crate) fn resume_retained_once(cx: &mut gpui::App) {
    if cx.has_global::<ResumedRetained>() {
        return;
    }
    let Some(operations) = crate::ssh::server_operations::installed(cx) else {
        return;
    };
    cx.set_global(ResumedRetained);
    operations.resume_retained();
}

struct ResumedRetained;

impl gpui::Global for ResumedRetained {}

/// One remote build as the shared update wording names builds. The wording names a build by its
/// version, commit and package digest, which is exactly what a remote build carries.
fn installation(build: &RemoteBuild) -> RetainedInstallation {
    RetainedInstallation {
        directory: std::path::PathBuf::from(&build.digest),
        version: build.version.clone(),
        commit: build.commit.clone(),
    }
}

#[cfg(test)]
#[path = "remote_maintenance_tests.rs"]
mod tests;
