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

use crate::local_server::service::ServiceStatus;
use crate::local_server::service::control::{
    Action, BuildName, UpdateChange, UpdateWording, Wording,
};
use crate::local_server::service::maintenance::{
    MaintenanceFacts, MaintenanceRefusal, PreparedChange, RemoteActivation, RemoteActivationPhase,
    RemoteBuild,
};
use crate::ssh::server_connection::{
    ContinuityChange, IncompatibilityKind, InteractionReason, RemoteConnectionStatus,
    ServerAvailability, SshIntentStoreError, UnavailableKind,
};
use crate::ssh::server_maintenance::{MaintenancePhase, SshMaintenanceFailure, SubmittedKind};
use crate::ssh::server_operations::{
    RemoteMaintenanceRequest, RemoteOperationEntry, RemoteOperationState, RemoteUpdateOffer,
    RetainedOutcome,
};
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
    /// Discards an original this client can no longer resolve, because the destination no longer
    /// reaches the computer, account or server it bound. Nothing is sent.
    Discard,
    /// Clears an entry whose original the service has no record of. There is nothing left to
    /// resolve and nothing left to retire, so it gives up nothing and asks nothing.
    Dismiss,
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
            Self::Acknowledge | Self::Dismiss => "OK",
            Self::Discard => "Discard\u{2026}",
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
            Self::Discard => "settings-ssh-server-maintenance-discard",
            Self::Dismiss => "settings-ssh-server-maintenance-dismiss",
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
            | Self::Dismiss
            | Self::Update
            | Self::Return
            | Self::Finish => "primary",
            // Cancel appears beside a described step, so it keeps a slot of its own.
            Self::Cancel => "cancel",
            // Discard appears beside the step that would resolve the original it discards.
            Self::Discard => "discard",
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
                format!("Checks the server status on {machine}.")
            }
            Self::Start => format!("Starts the server on {machine}."),
            Self::Stop | Self::Restart => {
                format!("Ends the Terminal Sessions on {machine} for every client.")
            }
            Self::Uninstall => {
                format!("Removes the service on {machine}.")
            }
            Self::Update => {
                format!("Prepares the server update on {machine}, then asks you to confirm.")
            }
            Self::Return => format!("Returns the server on {machine} to its previous version."),
            Self::Finish => format!("Completes the update on {machine}."),
            Self::CheckAgain => format!("Checks the result on {machine}."),
            Self::Cancel => format!("Stops waiting for {machine}."),
            Self::Acknowledge => format!("Dismisses this result for {machine}."),
            Self::Discard => {
                format!("Stops checking the result on {machine}. SpaceTerm asks first.")
            }
            Self::Dismiss => format!("Dismisses this result for {machine}."),
        };
        SharedString::from(text)
    }

    /// The preferred row for a step offered outside Settings. Shared steps use whichever row
    /// currently offers them.
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
            Self::Discard => "discard",
            Self::Dismiss => "dismiss",
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
    present_offered(machine, state, row, asked, &RemoteUpdateOffer::Unchecked)
}

/// One row's presentation, with what the read of `state` found about updating: `offer` decides
/// whether a read offers Update Server, and names the candidate it would install.
pub(crate) fn present_offered(
    machine: &str,
    state: &RemoteOperationState,
    row: MaintenanceRow,
    asked: Option<RemoteMaintenanceRequest>,
    offer: &RemoteUpdateOffer,
) -> MaintenancePresentation {
    match state {
        RemoteOperationState::Unread => unread(machine, row),
        RemoteOperationState::Read(facts) => read(machine, facts, row, offer),
        state => {
            if owning_row(state, asked) == row {
                submitted(machine, state, row)
            } else {
                bystander(machine, state, row)
            }
        }
    }
}

/// One row's presentation for a machine the owner holds more than one state for.
///
/// `states` are that machine's states in the order the owner reports them. One machine can hold
/// two originals, neither a duplicate of the other: a read of this client's retained records
/// that failed and later landed leaves it following the original it recovered beside the one the
/// person submitted while it could not read.
///
/// A row reports one state: one badge, one caption, one set of steps. Two badges and two sets of
/// steps in one row would leave the person unable to tell which step settles which operation,
/// and a step here retires the only record that could still say what a remote server did. So the
/// row reports one original and names the ones it has still to report, every step it offers acts
/// on the one it reports, and settling that one brings the next to the row.
pub(crate) fn present_machine(
    machine: &str,
    states: &[RemoteOperationState],
    row: MaintenanceRow,
    asked: Option<RemoteMaintenanceRequest>,
    offer: &RemoteUpdateOffer,
) -> MaintenancePresentation {
    // Each original is reported by the row whose journey the person asked for, as a single
    // original is, so two originals on different journeys each read in their own row.
    let owned: Vec<&RemoteOperationState> = states
        .iter()
        .filter(|state| owning_row(state, asked) == row)
        .collect();
    // The original the person is waiting on is what the row reports. A read is the fact a row
    // reports when nothing is submitted, so it gives way to an original that is.
    let reported = owned
        .iter()
        .position(|state| held(state).is_some())
        .or_else(|| (!owned.is_empty()).then_some(0));
    let Some(reported) = reported else {
        let state = states.first().unwrap_or(&RemoteOperationState::Unread);
        return present_offered(machine, state, row, asked, offer);
    };
    let waiting: Vec<&SubmittedKind> = owned
        .iter()
        .enumerate()
        .filter(|(index, _)| *index != reported)
        .filter_map(|(_, state)| held(state))
        .collect();
    let presentation = present_offered(machine, owned[reported], row, asked, offer);
    if waiting.is_empty() {
        return presentation;
    }
    MaintenancePresentation {
        explanation: SharedString::from(format!(
            "{} {}",
            presentation.explanation,
            awaiting(machine, &waiting)
        )),
        ..presentation
    }
}

/// The original one state holds: the operation this client sent and has not retired. A read or a
/// description holds none, because nothing was sent for it.
fn held(state: &RemoteOperationState) -> Option<&SubmittedKind> {
    match state {
        RemoteOperationState::Working { kind, .. }
        | RemoteOperationState::NeedsPerson { kind, .. }
        | RemoteOperationState::Paused { kind, .. }
        | RemoteOperationState::Failed { kind, .. } => kind.as_ref(),
        RemoteOperationState::Resolved { kind, .. }
        | RemoteOperationState::Unconfirmed { kind } => Some(kind),
        RemoteOperationState::Unread
        | RemoteOperationState::Read(_)
        | RemoteOperationState::Described { .. } => None,
    }
}

/// Names the originals a row has still to report and promises each one its turn. It claims
/// nothing about what any of them did, which this client cannot know until the machine answers.
fn awaiting(machine: &str, kinds: &[&SubmittedKind]) -> String {
    let named = listed(kinds);
    if kinds.len() == 1 {
        format!("Another requested action on {machine} is waiting: {named}.")
    } else {
        format!(
            "{} requested actions on {machine} are waiting: {named}.",
            kinds.len()
        )
    }
}

/// Names the operations a row has still to report, as a sentence lists them.
fn listed(kinds: &[&SubmittedKind]) -> String {
    let named: Vec<_> = kinds
        .iter()
        .map(|kind| format!("the {}", sent(kind)))
        .collect();
    match named.split_last() {
        Some((last, [])) => last.clone(),
        Some((last, earlier)) => format!("{} and {last}", earlier.join(", ")),
        None => String::new(),
    }
}

/// The steps a machine heading offers for the originals Settings currently reports. Each row
/// selects one original before its actions are read, so a waiting original cannot offer a step
/// Settings would reject for the reported one.
pub(crate) fn menu_actions_for(
    states: &[RemoteOperationState],
    asked: Option<RemoteMaintenanceRequest>,
    offer: &RemoteUpdateOffer,
) -> Vec<RemoteAction> {
    let offered = |row| present_machine("", states, row, asked, offer).actions;
    let mut actions = Vec::new();
    for action in menu_order(
        offered(MaintenanceRow::Service)
            .into_iter()
            .chain(offered(MaintenanceRow::Update)),
    ) {
        if !actions.contains(&action) {
            actions.push(action);
        }
    }
    actions
}

/// What the read among `entries` found about updating. The offer belongs to the read it came
/// with, so any other state knows of none.
pub(crate) fn read_update_offer<'a>(
    entries: impl IntoIterator<Item = &'a RemoteOperationEntry>,
) -> RemoteUpdateOffer {
    entries
        .into_iter()
        .find(|entry| matches!(entry.state, RemoteOperationState::Read(_)))
        .map_or(RemoteUpdateOffer::Unchecked, |entry| {
            entry.update_offer.clone()
        })
}

fn menu_order(actions: impl Iterator<Item = RemoteAction>) -> Vec<RemoteAction> {
    let mut actions: Vec<_> = actions
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
    // Only the steps are read here, so the machine the entry names is not needed.
    present("", state, row, None)
        .actions
        .into_iter()
        .filter(|action| {
            matches!(
                action,
                RemoteAction::Finish
                    | RemoteAction::CheckAgain
                    | RemoteAction::Acknowledge
                    | RemoteAction::Discard
                    | RemoteAction::Dismiss
            )
        })
        .collect()
}

/// The row that reports a submitted operation: the one whose journey the person asked for.
pub(crate) fn owning_row(
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
    // The row that reports the operation names its own step, so this one names only where to
    // find it.
    let reported = match row {
        MaintenanceRow::Service => "Server update",
        MaintenanceRow::Update => "Background service",
    };
    let (explanation, tone) = match state {
        RemoteOperationState::Working { .. } | RemoteOperationState::Described { .. } => {
            guidance(format!("An operation on {machine} is underway."))
        }
        RemoteOperationState::Resolved { .. } => {
            guidance(format!("See {reported} for the result on {machine}."))
        }
        _ => guidance(format!(
            "See {reported} for the unfinished action on {machine}."
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
            format!("Choose Check Now to check the server on {machine}."),
            vec![RemoteAction::Check],
        ),
        MaintenanceRow::Update => (
            None,
            format!("Choose Check Now to read the server version on {machine}."),
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
fn read(
    machine: &str,
    facts: &MaintenanceFacts,
    row: MaintenanceRow,
    offer: &RemoteUpdateOffer,
) -> MaintenancePresentation {
    match row {
        MaintenanceRow::Service => read_service(machine, facts),
        MaintenanceRow::Update => read_update(machine, facts, offer),
    }
}

fn read_service(machine: &str, facts: &MaintenanceFacts) -> MaintenancePresentation {
    let phase = ServicePhase::of(facts.status);
    let (state, state_selector, (explanation, tone)) = match phase {
        ServicePhase::NotInstalled => (
            "Not Installed",
            selector(MaintenanceRow::Service, "not-installed"),
            guidance(format!(
                "Choose Connect to install SpaceTerm Server for this account on {machine}."
            )),
        ),
        ServicePhase::Running => (
            "Running",
            selector(MaintenanceRow::Service, "running"),
            guidance(format!(
                "The server on {machine} keeps running after SpaceTerm quits."
            )),
        ),
        ServicePhase::Stopped => (
            "Stopped",
            selector(MaintenanceRow::Service, "stopped"),
            guidance("Choose Start Server to start it.".to_owned()),
        ),
        ServicePhase::NotRunning => (
            "Not Running",
            selector(MaintenanceRow::Service, "not-running"),
            warning(format!(
                "Choose Start Server to start the server on {machine}."
            )),
        ),
    };
    MaintenancePresentation {
        state: Some(state),
        state_selector,
        explanation,
        tone,
        actions: service_actions(facts.status),
        working: false,
    }
}

/// The steps one service state allows, from the phase table the local service row uses, with the
/// read every state offers. Installing a first server belongs to Connect, which offers the
/// verified build and explains what it does on that computer.
fn service_actions(status: ServiceStatus) -> Vec<RemoteAction> {
    let mut actions: Vec<_> = ServicePhase::of(status)
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
    actions
}

fn read_update(
    machine: &str,
    facts: &MaintenanceFacts,
    offer: &RemoteUpdateOffer,
) -> MaintenancePresentation {
    if let Some(activation) = facts
        .activation
        .as_ref()
        .filter(|activation| !activation.returned)
        && let Some(presentation) = unresolved_activation(machine, activation)
    {
        return presentation;
    }
    update_offer(machine, facts.status, facts.approved.as_ref(), offer)
}

/// The Update Server offer for one observed service state. Only a verified candidate that differs
/// from the approved build is an update; SpaceTerm claims none it hasn't verified.
fn update_offer(
    machine: &str,
    status: ServiceStatus,
    approved: Option<&RemoteBuild>,
    offer: &RemoteUpdateOffer,
) -> MaintenancePresentation {
    let quiet = |state: Option<&'static str>, name: &str, explanation: String| {
        let (explanation, tone) = guidance(explanation);
        MaintenancePresentation {
            state,
            state_selector: selector(MaintenanceRow::Update, name),
            explanation,
            tone,
            actions: Vec::new(),
            working: false,
        }
    };
    if !status.installed {
        return quiet(
            None,
            "available",
            format!("Install SpaceTerm Server on {machine} before updating it."),
        );
    }
    let candidate = match offer {
        RemoteUpdateOffer::Available(candidate) => candidate,
        RemoteUpdateOffer::UpToDate => {
            return quiet(
                Some("Up to Date"),
                "current",
                format!("{machine} runs the server version SpaceTerm includes."),
            );
        }
        RemoteUpdateOffer::Unavailable => {
            return quiet(
                None,
                "unavailable",
                format!(
                    "No server update is available for {machine} from this SpaceTerm installation."
                ),
            );
        }
        RemoteUpdateOffer::Unchecked => {
            return quiet(
                None,
                "unchecked",
                format!("Choose Check Now to check for a server update on {machine}."),
            );
        }
    };
    let candidate_version = &candidate.version;
    let explanation = match approved {
        Some(approved) => {
            let current_version = build(approved);
            if status.running {
                format!(
                    "Update {current_version} to {candidate_version}. This ends its Terminal Sessions for every client."
                )
            } else if status.stopped {
                format!(
                    "Update {current_version} to {candidate_version} on {machine}. The server stays stopped until you choose Start Server."
                )
            } else {
                format!(
                    "Update {current_version} to {candidate_version} on {machine}. No Terminal Sessions are running."
                )
            }
        }
        None if status.running => {
            format!("Updating ends the Terminal Sessions on {machine} for every client.")
        }
        None if status.stopped => {
            "Updating keeps the server stopped until you choose Start Server.".to_owned()
        }
        None => format!("Update Server transfers version {candidate_version} to {machine}."),
    };
    let (explanation, tone) = guidance(explanation);
    MaintenancePresentation {
        state: None,
        state_selector: selector(MaintenanceRow::Update, "available"),
        explanation,
        tone,
        actions: vec![RemoteAction::Update],
        working: false,
    }
}

/// What a server allows as this client last observed it through its connection, before any
/// maintenance read. A connection or a Check Status already observed whether that account's
/// server runs, so a surface offers what that observation allows and says when it was made.
/// `None` when nothing has been observed.
///
/// It uses the same phase table a validated read uses, so the two cannot disagree. Every step it
/// offers still describes its change against fresh facts before anything is sent.
pub(crate) fn observed(
    machine: &str,
    connection: &RemoteConnectionStatus,
    row: MaintenanceRow,
) -> Option<MaintenancePresentation> {
    let status = observed_status(connection)?;
    let phase = ServicePhase::of(status);
    Some(match row {
        MaintenanceRow::Service => {
            let (explanation, tone) = match phase {
                ServicePhase::NotInstalled => {
                    guidance("Not installed when last connected. Connect to install it.".to_owned())
                }
                ServicePhase::Running => guidance("Running when last connected.".to_owned()),
                ServicePhase::Stopped => guidance("Stopped when last connected.".to_owned()),
                ServicePhase::NotRunning => warning("Not running when last connected.".to_owned()),
            };
            MaintenancePresentation {
                state: Some(phase_badge(phase)),
                state_selector: selector(row, "observed"),
                explanation,
                tone,
                actions: service_actions(status),
                working: false,
            }
        }
        // A connection verifies no candidate, so only a read can find an update.
        MaintenanceRow::Update => {
            update_offer(machine, status, None, &RemoteUpdateOffer::Unchecked)
        }
    })
}

/// The steps a machine heading's menu offers for a server this client has only observed through
/// its connection.
pub(crate) fn observed_menu_actions(connection: &RemoteConnectionStatus) -> Vec<RemoteAction> {
    let offered = |row| {
        observed("", connection, row).map_or_else(Vec::new, |presentation| presentation.actions)
    };
    menu_order(
        offered(MaintenanceRow::Service)
            .into_iter()
            .chain(offered(MaintenanceRow::Update)),
    )
}

/// The service state one connection observation establishes, or `None` when it observed none.
/// Only a connection or an explicit Check Status observes this; nothing here reaches a computer.
fn observed_status(connection: &RemoteConnectionStatus) -> Option<ServiceStatus> {
    let installed = |running| ServiceStatus {
        installed: true,
        enabled: running,
        running,
        stopped: false,
    };
    Some(match connection {
        RemoteConnectionStatus::Connected { .. } => installed(true),
        RemoteConnectionStatus::Checked { availability, .. } => match availability {
            ServerAvailability::Running => installed(true),
            ServerAvailability::NotRunning => installed(false),
            ServerAvailability::NotInstalled => ServiceStatus::default(),
        },
        RemoteConnectionStatus::Stopped => ServiceStatus {
            installed: true,
            enabled: false,
            running: false,
            stopped: true,
        },
        _ => return None,
    })
}

/// The badge one service state carries, in the words every control surface uses.
const fn phase_badge(phase: ServicePhase) -> &'static str {
    match phase {
        ServicePhase::NotInstalled => "Not Installed",
        ServicePhase::Running => "Running",
        ServicePhase::Stopped => "Stopped",
        ServicePhase::NotRunning => "Not Running",
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
                "The update on {machine} from {previous} to {candidate} is unfinished. Choose Finish Update to continue."
            )),
            vec![RemoteAction::Finish],
        ),
        RemoteActivationPhase::Uncertain => (
            "Update Uncertain",
            selector(MaintenanceRow::Update, "uncertain"),
            failure(format!(
                "Couldn't confirm the update on {machine}. Choose Finish Update."
            )),
            vec![RemoteAction::Finish],
        ),
        RemoteActivationPhase::Failed { can_return: true } => (
            "Update Failed",
            selector(MaintenanceRow::Update, "failed"),
            failure(format!(
                "Version {candidate} didn't start on {machine}, and Terminal Sessions ended. You can return to {previous}."
            )),
            vec![RemoteAction::Return],
        ),
        RemoteActivationPhase::Failed { can_return: false } => (
            "Update Failed",
            selector(MaintenanceRow::Update, "failed"),
            failure(format!(
                "Version {candidate} didn't start on {machine}, and Terminal Sessions ended. SpaceTerm can't safely return to {previous}."
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
        // Only the states a submitted operation reaches arrive here; a read belongs to the rows.
        RemoteOperationState::Unread | RemoteOperationState::Read(_) => {
            return unread(machine, row);
        }
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
                    "Choose {} to review this change on {machine}.",
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
                "SpaceTerm couldn't confirm whether {machine} {}. Choose {} to check.",
                did(kind),
                plain(resolve_action(kind).label())
            )),
            // Asking may never produce an answer, so the person can also stop following it.
            vec![resolve_action(kind), RemoteAction::Discard],
            false,
        ),
        RemoteOperationState::NeedsPerson { reason, kind } => {
            let step = kind.as_ref().map_or(RemoteAction::Check, resolve_action);
            (
                Some("Waiting for You"),
                selector(row, "waiting"),
                guidance(if *reason == InteractionReason::FirstSignIn {
                    format!("Choose {} to sign in to {machine}.", plain(step.label()))
                } else {
                    format!(
                        "{} Choose {} to continue.",
                        interaction(*reason, machine),
                        plain(step.label())
                    )
                }),
                vec![step],
                false,
            )
        }
        RemoteOperationState::Paused { reason, kind } => (
            Some("Paused"),
            // A pause holding an operation it can no longer ask about tells a different story
            // from one that sent nothing, and offers different steps, so it reads as its own
            // state.
            selector(
                row,
                if kind.is_some() {
                    "paused-unresolved"
                } else {
                    "paused"
                },
            ),
            failure(match kind {
                // An original bound to an identity the destination no longer reaches can never be
                // resolved, so the person can stop following it instead of carrying it forever.
                Some(kind) => format!(
                    "{} SpaceTerm can't check the result of the earlier {}.",
                    continuity(*reason, machine),
                    sent(kind)
                ),
                None => continuity(*reason, machine),
            }),
            match kind {
                Some(kind) => vec![resolve_action(kind), RemoteAction::Discard],
                None => vec![RemoteAction::Check],
            },
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
    outcome: &RetainedOutcome,
    row: MaintenanceRow,
) -> Parts {
    match outcome {
        RetainedOutcome::Completed { status, build } => (
            Some(completed_badge(kind, *status)),
            selector(row, "done"),
            guidance(completed_detail(machine, kind, build.as_ref(), *status)),
            vec![RemoteAction::Acknowledge],
            false,
        ),
        RetainedOutcome::Refused(reason) => (
            Some("Refused"),
            selector(row, "refused"),
            failure(refusal(*reason, machine)),
            vec![RemoteAction::Acknowledge],
            false,
        ),
        RetainedOutcome::Failed { can_return } => {
            let can_return = *can_return;
            let mut actions = Vec::new();
            if can_return {
                actions.push(RemoteAction::Return);
            }
            actions.push(RemoteAction::Acknowledge);
            let detail = if can_return {
                format!(
                    "The update didn't start on {machine}, and Terminal Sessions ended. You can return to the previous version."
                )
            } else {
                format!(
                    "The update didn't start on {machine}, and Terminal Sessions ended. SpaceTerm can't safely return to the previous version."
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
                    "Stopped waiting. The action may still finish; choose {} to check.",
                    plain(step.label())
                ),
                None => format!("SpaceTerm cancelled before sending the request to {machine}."),
            }),
            again(resolve),
            false,
        ),
        SshMaintenanceFailure::NeedsPerson(reason) => (
            Some("Waiting for You"),
            selector(row, "waiting"),
            guidance(if reason == InteractionReason::FirstSignIn {
                format!(
                    "Choose {} to sign in to {machine}.",
                    plain(resolve.unwrap_or(RemoteAction::Check).label())
                )
            } else {
                format!(
                    "{} Choose {} to continue.",
                    interaction(reason, machine),
                    plain(resolve.unwrap_or(RemoteAction::Check).label())
                )
            }),
            vec![resolve.unwrap_or(RemoteAction::Check)],
            false,
        ),
        SshMaintenanceFailure::Changed(change) => (
            Some("Paused"),
            selector(row, "paused"),
            failure(continuity(change, machine)),
            vec![resolve.unwrap_or(RemoteAction::Check)],
            false,
        ),
        // A refusal or an incompatibility that ended an attempt to resolve a submitted operation
        // says nothing about what that operation did on the machine, so it keeps the one step
        // that settles it and claims nothing about its effect.
        SshMaintenanceFailure::Refused(reason) => match kind {
            Some(submitted) => (
                Some("Refused"),
                selector(row, "refused-unresolved"),
                failure(unsettled_check(machine, submitted)),
                vec![resolve_action(submitted)],
                false,
            ),
            None => (
                Some("Refused"),
                selector(row, "refused"),
                failure(refusal(reason, machine)),
                vec![RemoteAction::Check],
                false,
            ),
        },
        SshMaintenanceFailure::Incompatible(incompatible) => match kind {
            Some(submitted) => (
                Some("Incompatible"),
                selector(row, "incompatible-unresolved"),
                failure(unsettled_check(machine, submitted)),
                vec![resolve_action(submitted)],
                false,
            ),
            None => (
                Some("Incompatible"),
                selector(row, "incompatible"),
                failure(incompatibility(incompatible, machine)),
                vec![RemoteAction::Check],
                false,
            ),
        },
        SshMaintenanceFailure::Unavailable(kind) => (
            Some("Unavailable"),
            selector(row, "unavailable"),
            failure(match (kind, resolve) {
                (UnavailableKind::LocalIpcUnavailable, Some(step)) => format!(
                    "Couldn't prepare the SSH connection. Choose {} when it answers again.",
                    plain(step.label())
                ),
                _ => unavailable(kind, machine),
            }),
            vec![resolve.unwrap_or(RemoteAction::Check)],
            false,
        ),
        SshMaintenanceFailure::Unconfirmed => (
            Some("Unconfirmed"),
            selector(row, "unconfirmed"),
            warning(match (&resolve, kind) {
                (Some(step), Some(kind)) => format!(
                    "SpaceTerm couldn't confirm whether {machine} {}. Choose {} to check.",
                    did(kind),
                    plain(step.label())
                ),
                _ => format!("Didn't hear from {machine}."),
            }),
            again(resolve),
            false,
        ),
        SshMaintenanceFailure::Forgotten => (
            Some("No Record"),
            selector(row, "forgotten"),
            warning(format!(
                "SpaceTerm couldn't confirm the result because {machine} has no record of this action."
            )),
            vec![RemoteAction::Check],
            false,
        ),
        // This client keeps one record per server, and an unresolved original's record is the
        // only thing that could still say what it did. So a step chosen while one is held is
        // refused rather than retained over it: nothing was sent, and the original it refused
        // for is still outstanding.
        SshMaintenanceFailure::NotRetained => match kind {
            Some(submitted) => (
                Some("Not Started"),
                selector(row, "not-retained-unresolved"),
                failure(format!(
                    "Finish checking the earlier {} on {machine} before starting another action. {}",
                    sent(submitted),
                    unsettled(machine, submitted)
                )),
                vec![resolve_action(submitted)],
                false,
            ),
            None => (
                Some("Not Started"),
                selector(row, "not-retained"),
                failure(format!(
                    "SpaceTerm couldn't save this request, so it did not start the action on {machine}."
                )),
                vec![RemoteAction::Check],
                false,
            ),
        },
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

/// Says that a submitted operation is still unsettled, and names the one step that settles it.
/// It asserts nothing about what that operation did, which this client cannot know until the
/// machine answers for it.
fn unsettled(machine: &str, kind: &SubmittedKind) -> String {
    format!(
        "SpaceTerm couldn't confirm whether {machine} {}. Choose {} to check.",
        did(kind),
        plain(resolve_action(kind).label())
    )
}

fn unsettled_check(machine: &str, kind: &SubmittedKind) -> String {
    format!(
        "Choose {} to check whether {machine} {}.",
        plain(resolve_action(kind).label()),
        did(kind)
    )
}

/// A label without its trailing ellipsis, for use inside a sentence.
fn plain(label: &'static str) -> &'static str {
    label.trim_end_matches('\u{2026}')
}

/// What this client sent, as a noun a sentence can name.
const fn sent(kind: &SubmittedKind) -> &'static str {
    match kind {
        SubmittedKind::Start => "start",
        SubmittedKind::Service(Action::Stop) => "stop",
        SubmittedKind::Service(Action::Restart) => "restart",
        SubmittedKind::Service(Action::Uninstall) => "removal",
        SubmittedKind::Update { .. } => "update",
        SubmittedKind::Return => "return",
    }
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
        (MaintenancePhase::Authenticating, _) => {
            format!("Answer the SSH prompt to sign in to {machine}.")
        }
        (MaintenancePhase::Validating, _) => format!("Verifying the connection to {machine}…"),
        (MaintenancePhase::Transferring, _) => format!("Preparing the server update on {machine}…"),
        (MaintenancePhase::Resolving, _) => {
            format!("Checking the result on {machine}…")
        }
        (MaintenancePhase::Dispatching, None) => {
            format!("Preparing the change on {machine}…")
        }
        (MaintenancePhase::Dispatching, Some(SubmittedKind::Start)) => {
            format!("Starting the server on {machine}\u{2026}")
        }
        (MaintenancePhase::Dispatching, Some(SubmittedKind::Service(Action::Stop))) => {
            format!("Stopping the server on {machine}…")
        }
        (MaintenancePhase::Dispatching, Some(SubmittedKind::Service(Action::Restart))) => {
            format!("Restarting the server on {machine}…")
        }
        (MaintenancePhase::Dispatching, Some(SubmittedKind::Service(Action::Uninstall))) => {
            format!("Removing the service on {machine}\u{2026}")
        }
        (MaintenancePhase::Dispatching, Some(SubmittedKind::Update { .. })) => {
            format!("Updating the server on {machine}…")
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

fn completed_detail(
    machine: &str,
    kind: &SubmittedKind,
    active: Option<&RemoteBuild>,
    status: Option<ServiceStatus>,
) -> String {
    let stays_stopped = status.is_some_and(|status| status.stopped && !status.running);
    let detail = match kind {
        SubmittedKind::Start => format!("The server on {machine} started."),
        SubmittedKind::Service(Action::Stop) => format!("The server on {machine} stopped."),
        SubmittedKind::Service(Action::Restart) => format!("The server on {machine} restarted."),
        SubmittedKind::Service(Action::Uninstall) => {
            format!("The server on {machine} was uninstalled.")
        }
        SubmittedKind::Update { .. } => match active {
            Some(active) => format!("The server on {machine} now runs {}.", build(active)),
            None => format!("The server on {machine} was updated."),
        },
        SubmittedKind::Return => match active {
            Some(active) if stays_stopped => {
                format!("The server on {machine} returned to {}.", build(active))
            }
            Some(active) => format!(
                "The server on {machine} returned to version {}.",
                build(active)
            ),
            None => format!("The server on {machine} returned to its previous version."),
        },
    };
    let stopped = match kind {
        SubmittedKind::Update { .. } | SubmittedKind::Return if stays_stopped => {
            " It stays stopped until you choose Start Server."
        }
        _ => "",
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
        InteractionReason::FirstSignIn => {
            format!("SpaceTerm hasn\u{2019}t signed in to {machine} since it opened.")
        }
    }
}

fn continuity(change: ContinuityChange, machine: &str) -> String {
    match change {
        ContinuityChange::HostChanged => format!("\"{machine}\" now reaches a different computer."),
        ContinuityChange::AccountChanged => {
            format!("\"{machine}\" now signs in to a different account.")
        }
        ContinuityChange::ServerChanged => format!(
            "SpaceTerm paused because this account on {machine} now has a different server."
        ),
        ContinuityChange::BuildChanged => {
            format!("SpaceTerm paused because the server version on {machine} changed.")
        }
    }
}

fn refusal(reason: MaintenanceRefusal, machine: &str) -> String {
    match reason {
        MaintenanceRefusal::NotInstalled => {
            format!("{machine} has no SpaceTerm Server for this account.")
        }
        MaintenanceRefusal::NotRunning => format!("Start the server on {machine}, then try again."),
        MaintenanceRefusal::Stopped => {
            format!("Choose Start Server to start the server on {machine}.")
        }
        MaintenanceRefusal::Changed => {
            format!("The server on {machine} changed. Review the action and confirm again.")
        }
        MaintenanceRefusal::AuthorizationRequired => {
            format!("Authorize this action on {machine}, then try again.")
        }
        MaintenanceRefusal::CandidateUnavailable => {
            "SpaceTerm has no verified server version for that computer.".to_owned()
        }
        MaintenanceRefusal::CandidateChanged => {
            format!("SpaceTerm couldn't verify the server update for {machine}.")
        }
        MaintenanceRefusal::ReturnUnavailable => format!(
            "SpaceTerm can't safely return the server on {machine} to its previous version."
        ),
        MaintenanceRefusal::StorageUnavailable => {
            format!("SpaceTerm can't safely access the saved server data on {machine}.")
        }
        MaintenanceRefusal::Protocol => {
            format!("SpaceTerm couldn't read the response from the server on {machine}.")
        }
        MaintenanceRefusal::Unsupported => {
            format!("Update the server on {machine} to use this operation.")
        }
    }
}

/// Why SpaceTerm and the server for this account cannot work together. It says nothing about what
/// that server is doing, so a sentence about an operation already dispatched can use it.
fn incompatible_cause(kind: IncompatibilityKind, machine: &str) -> String {
    match kind {
        IncompatibilityKind::Protocol => {
            format!("SpaceTerm and the server on {machine} are incompatible.")
        }
        IncompatibilityKind::Capability => format!("The server on {machine} needs an update."),
        IncompatibilityKind::Platform => format!(
            "SpaceTerm has no verified server for {machine}'s operating system or processor."
        ),
    }
}

/// The incompatibility of a server nothing was submitted to.
fn incompatibility(kind: IncompatibilityKind, machine: &str) -> String {
    incompatible_cause(kind, machine)
}

fn unavailable(kind: UnavailableKind, machine: &str) -> String {
    match kind {
        UnavailableKind::LocalIpcUnavailable => format!(
            "SpaceTerm couldn't prepare the SSH connection to {machine}. Choose Check Now to try again."
        ),
        UnavailableKind::Network => "The network is unavailable.".to_owned(),
        UnavailableKind::HostUnreachable => format!("Couldn't reach {machine}."),
        UnavailableKind::SshFailed => "Check your SSH configuration.".to_owned(),
        UnavailableKind::ServiceUnavailable => format!("The server on {machine} did not answer."),
    }
}

/// Names one build by version and abbreviated commit.
fn build(build: &RemoteBuild) -> String {
    let commit = build.commit.get(..7).unwrap_or(&build.commit);
    format!("{} ({commit})", build.version)
}

/// Adds the package prefix only when version and commit do not distinguish the builds.
fn builds(previous: &RemoteBuild, candidate: &RemoteBuild) -> (String, String) {
    let ambiguous = build(previous) == build(candidate) && previous.digest != candidate.digest;
    let label = |value: &RemoteBuild| {
        if ambiguous && let Some(package) = value.digest.get(..8) {
            let commit = value.commit.get(..7).unwrap_or(&value.commit);
            format!("{} ({commit}, {package})", value.version)
        } else {
            build(value)
        }
    };
    (label(previous), label(candidate))
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
    pub(crate) confirm: SharedString,
}

impl MaintenanceConfirmation {
    /// The confirmation for `request` as `change` describes it, naming `machine`. `None` for a
    /// request that ends no work and so asks nothing.
    ///
    /// The copy is the shared destructive copy every control surface shows, so a remote
    /// confirmation reads like the local one with the selected machine in place of this
    /// computer.
    pub(crate) fn of(
        machine: &str,
        request: RemoteMaintenanceRequest,
        change: &PreparedChange,
    ) -> Option<Self> {
        match request {
            RemoteMaintenanceRequest::Service(action) => {
                let wording = Wording::for_server(machine, action, change.running, change.startup);
                Some(Self {
                    title: wording.title(),
                    message: wording.message().into(),
                    detail: wording.detail().into(),
                    confirm: wording.confirm_label().into(),
                })
            }
            RemoteMaintenanceRequest::Update | RemoteMaintenanceRequest::Return => {
                let previous = change.previous.as_ref()?;
                let candidate = change.candidate.as_ref()?;
                let update = match request {
                    RemoteMaintenanceRequest::Return => UpdateChange::Return,
                    _ => UpdateChange::Update,
                };
                let wording = UpdateWording::for_server(
                    machine,
                    update,
                    name(previous),
                    name(candidate),
                    change.running,
                    change.stopped,
                );
                Some(Self {
                    title: wording.title(),
                    message: wording.message().into(),
                    detail: wording.detail().into(),
                    confirm: wording.confirm_label().into(),
                })
            }
        }
    }

    /// The confirmation for discarding the submitted operation `kind` on `machine`.
    ///
    /// Discarding retires the only record that could still say what that operation did on that
    /// computer, which no later read recovers, so it asks first and says what is given up.
    /// Nothing is sent, so the copy promises nothing about the machine.
    pub(crate) fn for_discard(machine: &str, kind: &SubmittedKind) -> Self {
        let sent = sent(kind);
        Self {
            title: "Discard",
            message: SharedString::from(format!("Discard the {sent} SpaceTerm sent to {machine}?")),
            detail: SharedString::from(format!(
                "SpaceTerm will stop checking this {sent}. You will lose the only remaining way to confirm its result on {machine}."
            )),
            confirm: "Discard".into(),
        }
    }
}

/// The submitted operation a Discard here would stop following, or `None` for a state that offers
/// no Discard. The step is offered only where the destination can no longer answer for it.
/// Whether this entry is one a Dismiss clears: an original the service has no record of, which
/// is as settled as that original can become.
pub(crate) const fn dismissed_operation(state: &RemoteOperationState) -> bool {
    matches!(
        state,
        RemoteOperationState::Failed {
            failure: SshMaintenanceFailure::Forgotten,
            ..
        }
    )
}

pub(crate) const fn discarded_operation(state: &RemoteOperationState) -> Option<&SubmittedKind> {
    match state {
        RemoteOperationState::Paused { kind, .. } => kind.as_ref(),
        RemoteOperationState::Unconfirmed { kind } => Some(kind),
        _ => None,
    }
}

/// Where an original the service has no record of ends, for an entry that can no longer reach
/// the machine to ask about it.
///
/// A service with no record of an original either never received it, or admitted it and dropped
/// it without validating a result. This client cannot tell those apart, so the caption names both
/// and claims neither, and it claims nothing about what the operation did on that computer.
fn forgotten_final_word(machine: &str) -> MaintenancePresentation {
    let (explanation, tone) = warning(format!(
        "SpaceTerm couldn't confirm the result on {machine}. Choose OK to dismiss this entry."
    ));
    MaintenancePresentation {
        state: Some("No Record"),
        state_selector: selector(MaintenanceRow::Service, "forgotten"),
        explanation,
        tone,
        actions: vec![RemoteAction::Dismiss],
        working: false,
    }
}

/// What a server whose saved connection was removed still has to tell the person, and the one
/// step it offers. `None` once nothing is left to tell, so a removed connection disappears as
/// soon as its operation is settled.
/// Which of a removed connection's states its entry reports. A step offered there acts on this
/// one, so the selection and the presentation must read the same rule.
pub(crate) fn removed_subject(states: &[RemoteOperationState]) -> Option<usize> {
    states
        .iter()
        .position(|state| removed_word("", state).is_some())
}

pub(crate) fn removed_presentation(
    machine: &str,
    states: &[RemoteOperationState],
) -> Option<MaintenancePresentation> {
    // A removed connection can hold two originals for the same reason a saved one can, so its
    // entry reports one of them and names the ones it has still to report.
    let reported = removed_subject(states)?;
    let presentation = removed_word(machine, states.get(reported)?)?;
    let waiting: Vec<&SubmittedKind> = states
        .iter()
        .enumerate()
        .filter(|(index, _)| *index != reported)
        .filter_map(|(_, state)| held(state))
        .collect();
    if waiting.is_empty() {
        return Some(presentation);
    }
    Some(MaintenancePresentation {
        explanation: SharedString::from(format!(
            "{} {}",
            presentation.explanation,
            awaiting(machine, &waiting)
        )),
        ..presentation
    })
}

/// What one state of a removed connection's server still has to tell the person, and the one step
/// it offers.
fn removed_word(machine: &str, state: &RemoteOperationState) -> Option<MaintenancePresentation> {
    // An original the service has no record of can never be resolved, and this client retains
    // nothing for it, so the entry ends there instead of offering a step that would reach the
    // connection the person removed.
    if matches!(
        state,
        RemoteOperationState::Failed {
            failure: SshMaintenanceFailure::Forgotten,
            ..
        }
    ) {
        return Some(forgotten_final_word(machine));
    }
    [MaintenanceRow::Service, MaintenanceRow::Update]
        .into_iter()
        .find_map(|row| {
            let presentation = present(machine, state, row, None);
            let actions = removed_actions(state, row);
            // Work underway on a removed connection's own original is still something to say,
            // so the entry reports its progress instead of disappearing until it settles.
            (!actions.is_empty() || presentation.working).then_some(MaintenancePresentation {
                actions,
                ..presentation
            })
        })
}

/// Why clearing a finished operation on an SSH server does not stick, and why a result on screen
/// now is not there after a relaunch.
///
/// This client's record of the operations it submitted is its own, separate from saved
/// connections and from the Settings Document, so this says only what that record changes. It
/// names no file, no path and no reason the operating system gave, which the person could not act
/// on.
pub(crate) const fn operations_persistence_warning(error: SshIntentStoreError) -> &'static str {
    match error {
        SshIntentStoreError::Unavailable => {
            "SpaceTerm couldn't save these results. They're lost when SpaceTerm quits."
        }
        SshIntentStoreError::Malformed => {
            "SpaceTerm's saved results are damaged. They're lost when SpaceTerm quits."
        }
    }
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

#[cfg(test)]
#[path = "remote_maintenance_tests.rs"]
mod tests;
