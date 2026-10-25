//! The local server's version and its one "Update Server..." journey. Reading stages nothing:
//! the server package this SpaceTerm includes is verified and retained only after the person
//! chooses Update Server, and only the change they then confirm replaces the approved runtime.

use std::sync::Arc;

use gpui::prelude::*;
use gpui::{AnyWindowHandle, Context, SharedString, Task};
use spaceterm_ui::{
    Alert, AlertIntent, AlertOutcome, ModalAction, ModalActionEmphasis, ModalActionIntent,
    ModalActionRole, ModalId,
};

use super::SettingsWindow;
use super::local_operations::UpdateCompletion;
use crate::application_identity::ApplicationIdentity;
use crate::local_startup::CandidateError;
use crate::server_connection::ServerError;
use crate::server_connection::service::activation::{
    ActivationPhase, ActivationStatus, is_released_downgrade,
};
use crate::server_connection::service::control::{
    BuildName, UpdateChange, UpdateWording, build_labels,
};
use crate::server_connection::service::{RetainedInstallation, Service, ServiceStatus};
use crate::ui::sidebar_window::form::CaptionTone;

pub(super) const UPDATE_SELECTOR: &str = "settings-server-update";
pub(super) const UPDATE_CHECK_SELECTOR: &str = "settings-server-update-check";
pub(super) const FINISH_SELECTOR: &str = "settings-server-update-finish";
pub(super) const RETURN_SELECTOR: &str = "settings-server-update-return";
pub(super) const UPDATE_CONFIRM_SELECTOR: &str = "settings-server-update-confirm";
pub(super) const UPDATE_CANCEL_SELECTOR: &str = "settings-server-update-cancel";
pub(super) const RETURN_CONFIRM_SELECTOR: &str = "settings-server-return-confirm";
pub(super) const RETURN_CANCEL_SELECTOR: &str = "settings-server-return-cancel";

/// The server build this SpaceTerm includes, as its signed manifest declares it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct IncludedServer {
    pub(super) version: String,
    pub(super) commit: String,
    /// The whole signed package's digest.
    pub(super) package: String,
    /// Whether it is byte-for-byte the approved build, so there is nothing to update.
    pub(super) approved: bool,
    /// Whether it is an earlier release than the approved build, which an update never installs.
    pub(super) earlier: bool,
}

/// What one read of the server's version found.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct UpdateFacts {
    /// The service's status, or why it refused while an update it records is unresolved.
    pub(super) status: Result<ServiceStatus, ServerError>,
    /// The durably approved build. `None` while no service is installed.
    pub(super) approved: Option<RetainedInstallation>,
    /// The server build this SpaceTerm includes, or why it offers none.
    pub(super) included: Result<IncludedServer, CandidateError>,
    /// The last update or return the service records.
    pub(super) activation: Option<ActivationStatus>,
}

/// A change the service prepared and bound to the observation it made. Only the person's
/// confirmation runs it, once.
pub(super) struct PreparedChange {
    pub(super) change: UpdateChange,
    pub(super) previous: RetainedInstallation,
    pub(super) candidate: RetainedInstallation,
    pub(super) running: bool,
    pub(super) stopped: bool,
    /// Performs exactly the prepared change. The service rechecks its observation first and
    /// refuses with [`ServerError::Changed`] when it differs.
    pub(super) commit: Box<dyn FnOnce() -> Result<ActivationStatus, ServerError> + Send>,
}

/// The blocking update operations the row runs on the background executor.
pub(super) trait ServerUpdateControl: Send + Sync {
    /// Reads the service, its approved build, the included build, and any recorded update.
    /// Verifies the included build's signed manifest without retaining or running it.
    fn read(&self) -> Result<UpdateFacts, ServerError>;
    /// Verifies and retains the included build, then prepares its activation. `None` when it is
    /// already the approved build.
    fn prepare_update(&self) -> Result<Option<PreparedChange>, ServerError>;
    /// Prepares a return to the previous build of an update that never became ready.
    fn prepare_return(&self) -> Result<PreparedChange, ServerError>;
    /// Resolves the recorded update the person already confirmed, without new consent.
    fn finish(&self) -> Result<Option<ActivationStatus>, ServerError>;
}

impl ServerUpdateControl for Service {
    fn read(&self) -> Result<UpdateFacts, ServerError> {
        read_facts(self, self.activation_status()?, Service::status)
    }

    fn prepare_update(&self) -> Result<Option<PreparedChange>, ServerError> {
        let candidate = crate::local_startup::discover_candidate().map_err(ServerError::from)?;
        let installation = candidate.stage()?;
        let Some(approval) = self.prepare_activation(&installation)? else {
            return Ok(None);
        };
        let service = self.clone();
        Ok(Some(PreparedChange {
            change: UpdateChange::Update,
            previous: approval.previous().clone(),
            candidate: approval.candidate().clone(),
            running: approval.running(),
            stopped: approval.stopped(),
            commit: Box::new(move || service.activate(approval)),
        }))
    }

    fn prepare_return(&self) -> Result<PreparedChange, ServerError> {
        let approval = Service::prepare_return(self)?;
        let service = self.clone();
        Ok(PreparedChange {
            change: UpdateChange::Return,
            previous: approval.previous().clone(),
            candidate: approval.candidate().clone(),
            running: false,
            stopped: approval.stopped(),
            commit: Box::new(move || service.return_to_previous(approval)),
        })
    }

    fn finish(&self) -> Result<Option<ActivationStatus>, ServerError> {
        self.recover_activation()
    }
}

/// The dropdown observes the same facts and policy as Settings without resuming an activation.
/// Blocking local reads and signature verification only; it retains no candidate. It returns the
/// row's state badge, because the explanation refers to the Settings layout around it.
pub(crate) fn passive_update(service: &Service) -> Result<(SharedString, bool), ServerError> {
    // activation_status can prove recovery eligibility and clean a stale endpoint while doing
    // so. A passive read instead refuses unresolved originals and leaves their review to Settings.
    let facts = read_facts(service, None, Service::passive_status)?;
    let presentation = present_update(&UpdateState {
        facts: Some(facts),
        ..UpdateState::default()
    });
    Ok((
        presentation.state.into(),
        presentation.actions.contains(&UpdateAction::Update),
    ))
}

fn read_facts(
    service: &Service,
    activation: Option<ActivationStatus>,
    status: fn(&Service) -> Result<ServiceStatus, ServerError>,
) -> Result<UpdateFacts, ServerError> {
    // Keep the recorded original visible even when the status read refuses an unresolved update.
    // Settings may recover that original; the dropdown supplies the passive status reader.
    let status = match (status(service), &activation) {
        (Ok(status), _) => Ok(status),
        (Err(error), Some(_)) => Err(error),
        (Err(error), None) => return Err(error),
    };
    // An installed service lists only its approved build.
    let approved = match status {
        Ok(status) if status.installed => service.retained_installations()?.into_iter().next(),
        _ => None,
    };
    let included = crate::local_startup::discover_candidate().map(|candidate| IncludedServer {
        version: candidate.version().to_owned(),
        commit: candidate.commit().to_owned(),
        package: candidate.digest().to_owned(),
        approved: approved
            .as_ref()
            .is_some_and(|approved| candidate.is_approved(approved)),
        // The service's own policy, so the row never offers what activation refuses.
        earlier: approved
            .as_ref()
            .is_some_and(|approved| is_released_downgrade(candidate.version(), &approved.version)),
    });
    Ok(UpdateFacts {
        status,
        approved,
        included,
        activation,
    })
}

/// Whether the Server row offers Update Server for `service`, for a surface that opens
/// the journey from elsewhere. Reads only, so it stages, installs, and starts nothing. Blocking.
pub(crate) fn update_offered(service: &Service) -> bool {
    ServerUpdateControl::read(service).is_ok_and(|facts| {
        present_update(&UpdateState {
            facts: Some(facts),
            ..UpdateState::default()
        })
        .actions
        .contains(&UpdateAction::Update)
    })
}

/// A step the person can take from the row.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum UpdateAction {
    Check,
    /// Verifies the included build, then asks before replacing the approved runtime.
    Update,
    /// Resolves the update the person already confirmed. It asks nothing again.
    Finish,
    /// Asks before returning to the previous build of an update that never became ready.
    Return,
}

impl UpdateAction {
    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Check => "Check Now",
            Self::Update => "Update Server\u{2026}",
            Self::Finish => "Finish Update",
            Self::Return => "Return to Previous Version\u{2026}",
        }
    }

    pub(super) const fn selector(self) -> &'static str {
        match self {
            Self::Check => UPDATE_CHECK_SELECTOR,
            Self::Update => UPDATE_SELECTOR,
            Self::Finish => FINISH_SELECTOR,
            Self::Return => RETURN_SELECTOR,
        }
    }

    /// Update, Finish, and Return share the leading slot, so keyboard focus stays on it when
    /// one replaces another.
    pub(super) const fn slot(self) -> &'static str {
        match self {
            Self::Update | Self::Finish | Self::Return => "settings-server-update-primary",
            Self::Check => "settings-server-update-secondary",
        }
    }

    pub(super) const fn consequence(self) -> Option<&'static str> {
        match self {
            Self::Update => Some(
                "Update the server after confirmation. This ends its Terminal Sessions for every client.",
            ),
            Self::Finish => Some("Completes the confirmed update."),
            Self::Return => Some("SpaceTerm asks first."),
            Self::Check => None,
        }
    }
}

/// What the row is doing.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum UpdateStep {
    Check,
    /// Verifying and retaining the included build, then preparing its confirmation.
    Prepare,
    /// The confirmed update runs. `running` is whether it ends live Terminal Sessions.
    Update {
        running: bool,
    },
    PrepareReturn,
    Return,
    Finish,
}

impl UpdateStep {
    const fn progress(self) -> (&'static str, &'static str) {
        match self {
            Self::Check => ("Checking", "Reading the server version\u{2026}"),
            Self::Prepare => (
                "Verifying",
                "Verifying the server version included with SpaceTerm\u{2026}",
            ),
            Self::Update { running: true } => (
                "Updating",
                "Updating the server. Its Terminal Sessions end\u{2026}",
            ),
            Self::Update { running: false } => ("Updating", "Updating the server\u{2026}"),
            Self::PrepareReturn => ("Checking", "Checking the previous version\u{2026}"),
            Self::Return => (
                "Returning",
                "Returning the server to its previous version\u{2026}",
            ),
            Self::Finish => ("Finishing", "Finishing the update you confirmed\u{2026}"),
        }
    }

    /// Whether the step may change the service. Reading and preparing leave it as it is:
    /// preparing only verifies and retains a build beside the approved one.
    pub(super) const fn changes_service(self) -> bool {
        matches!(self, Self::Update { .. } | Self::Return | Self::Finish)
    }
}

/// What the row last learned and what it is doing.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(super) struct UpdateState {
    /// The last read. `None` until the first read succeeds.
    pub(super) facts: Option<UpdateFacts>,
    /// The read failure, or the admitted failure retained until the next change.
    pub(super) failure: Option<ServerError>,
    pub(super) working: Option<UpdateStep>,
}

/// The row's complete presentation, derived from its state alone.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct UpdatePresentation {
    pub(super) state: &'static str,
    /// Names the state badge, so a test can tell each state apart by geometry.
    pub(super) state_selector: &'static str,
    pub(super) explanation: SharedString,
    pub(super) tone: CaptionTone,
    pub(super) actions: Vec<UpdateAction>,
    pub(super) working: bool,
}

/// Names an update's two builds so they read apart.
fn labels(previous: &RetainedInstallation, candidate: &RetainedInstallation) -> (String, String) {
    build_labels(BuildName::of(previous), BuildName::of(candidate))
}

/// The presentation of one state. A failed operation keeps the badge and steps of the read after
/// it, so the row says what is true now and why the operation did not finish.
pub(super) fn present_update(state: &UpdateState) -> UpdatePresentation {
    let (badge, state_selector, explanation, tone, actions) = match &state.facts {
        None => match state.failure {
            None => (
                "Not Checked",
                "settings-server-update-state-not-checked",
                "SpaceTerm reads the server version when you open this section.".into(),
                CaptionTone::Guidance,
                vec![UpdateAction::Check],
            ),
            Some(error) => (
                "Unknown",
                "settings-server-update-state-unknown",
                error.to_string(),
                CaptionTone::Error,
                vec![UpdateAction::Check],
            ),
        },
        Some(facts) => present_facts(facts),
    };
    // An unresolved update is what is true now, so its caption outlasts the failure that led
    // to it.
    let unresolved = state
        .facts
        .as_ref()
        .and_then(|facts| facts.activation.as_ref())
        .is_some_and(|activation| !activation.returned && present_activation(activation).is_some());
    let failure = state
        .failure
        .filter(|_| state.facts.is_some() && !unresolved);
    let (explanation, tone) = match failure {
        Some(error) => (failure_caption(error), CaptionTone::Error),
        None => (explanation, tone),
    };
    let (label, explanation, tone) = match state.working {
        Some(step) => {
            let (label, explanation) = step.progress();
            (label, explanation.to_owned(), CaptionTone::Guidance)
        }
        None => (badge, explanation, tone),
    };
    UpdatePresentation {
        state: label,
        state_selector,
        explanation: explanation.into(),
        tone,
        actions,
        working: state.working.is_some(),
    }
}

/// Why an update operation stopped, in words that say what to do next.
fn failure_caption(error: ServerError) -> String {
    match error {
        ServerError::CandidateChanged => RESTART_CLIENT.to_owned(),
        ServerError::Changed => "The server changed. Review the update again.".to_owned(),
        error => error.to_string(),
    }
}

const RESTART_CLIENT: &str = "Quit and reopen SpaceTerm to update the server.";

type Facts = (
    &'static str,
    &'static str,
    String,
    CaptionTone,
    Vec<UpdateAction>,
);

fn present_facts(facts: &UpdateFacts) -> Facts {
    if let Some(activation) = facts
        .activation
        .as_ref()
        .filter(|activation| !activation.returned)
        && let Some(unresolved) = present_activation(activation)
    {
        return unresolved;
    }
    let status = match facts.status {
        Ok(status) => status,
        Err(error) => {
            return (
                "Unknown",
                "settings-server-update-state-unknown",
                error.to_string(),
                CaptionTone::Error,
                vec![UpdateAction::Check],
            );
        }
    };
    if !status.installed {
        return (
            "Not Installed",
            "settings-server-update-state-not-installed",
            "Install the background service above to see the server version.".into(),
            CaptionTone::Guidance,
            Vec::new(),
        );
    }
    let Some(approved) = &facts.approved else {
        return (
            "Unknown",
            "settings-server-update-state-unknown",
            "Couldn't read installed server version.".into(),
            CaptionTone::Error,
            vec![UpdateAction::Check],
        );
    };
    // The included build is named beside the approved one, so both read apart.
    let included_name = facts.included.as_ref().ok().map(|included| BuildName {
        version: &included.version,
        commit: &included.commit,
        package: &included.package,
    });
    let (approved, included_label) = build_labels(
        BuildName::of(approved),
        included_name.unwrap_or(BuildName::of(approved)),
    );
    let now = if status.running {
        format!("Version {approved} is running.")
    } else if status.stopped {
        format!("Version {approved} is installed, and you stopped the server.")
    } else {
        format!("Version {approved} is installed but not running.")
    };
    let returned = facts
        .activation
        .as_ref()
        .filter(|activation| activation.returned)
        .map_or(String::new(), |activation| {
            let (_, candidate) = labels(&activation.previous, &activation.candidate);
            format!(" You returned from version {candidate}.")
        });
    match &facts.included {
        Ok(included) if included.approved => (
            "Up to Date",
            "settings-server-update-state-current",
            format!("{now}{returned} It\u{2019}s the server version included with SpaceTerm."),
            CaptionTone::Guidance,
            Vec::new(),
        ),
        Ok(included) if included.earlier => (
            "No Update",
            "settings-server-update-state-earlier",
            format!(
                "{now}{returned} SpaceTerm includes server version {included_label}, an earlier \
                 release, so it offers no server update."
            ),
            CaptionTone::Guidance,
            Vec::new(),
        ),
        Ok(_) => {
            let included = included_label;
            let effect = if status.running {
                "Updating ends the server\u{2019}s Terminal Sessions for every client, so \
                 SpaceTerm asks first."
            } else if status.stopped {
                "Updating keeps the server stopped until you start it."
            } else {
                "The server isn\u{2019}t running, so updating ends no Terminal Sessions."
            };
            (
                "Update Available",
                "settings-server-update-state-available",
                format!("{now}{returned} SpaceTerm includes server version {included}. {effect}"),
                CaptionTone::Guidance,
                vec![UpdateAction::Update],
            )
        }
        Err(CandidateError::Unavailable) => (
            "No Update",
            "settings-server-update-state-unavailable",
            format!(
                "{now}{returned} This SpaceTerm doesn\u{2019}t include a server package, so it \
                 offers no server update."
            ),
            CaptionTone::Guidance,
            Vec::new(),
        ),
        Err(CandidateError::ClientRestartRequired) => (
            "Restart SpaceTerm",
            "settings-server-update-state-restart",
            RESTART_CLIENT.to_owned(),
            CaptionTone::Warning,
            Vec::new(),
        ),
        Err(CandidateError::Verification(error)) => (
            "Not Verified",
            "settings-server-update-state-not-verified",
            format!("Couldn't verify the server package. {error}"),
            CaptionTone::Error,
            vec![UpdateAction::Check],
        ),
    }
}

/// An update the service has not resolved, or `None` once it completed.
fn present_activation(activation: &ActivationStatus) -> Option<Facts> {
    let (previous, candidate) = labels(&activation.previous, &activation.candidate);
    Some(match activation.phase {
        ActivationPhase::Completed { .. } => return None,
        ActivationPhase::Activating => (
            "Update Unfinished",
            "settings-server-update-state-unfinished",
            "Choose Finish Update to complete the update.".to_owned(),
            CaptionTone::Warning,
            vec![UpdateAction::Finish],
        ),
        ActivationPhase::Uncertain => (
            "Update Uncertain",
            "settings-server-update-state-uncertain",
            format!("Couldn't confirm the update to {candidate}. Choose Finish Update."),
            CaptionTone::Error,
            vec![UpdateAction::Finish],
        ),
        ActivationPhase::Failed { can_return: true } => (
            "Update Failed",
            "settings-server-update-state-failed",
            format!("Version {candidate} didn't start, and Terminal Sessions ended."),
            CaptionTone::Error,
            vec![UpdateAction::Return, UpdateAction::Check],
        ),
        ActivationPhase::Failed { can_return: false } => (
            "Update Failed",
            "settings-server-update-state-failed",
            format!(
                "Version {candidate} didn't start, and Terminal Sessions ended. SpaceTerm can't safely restore {previous}."
            ),
            CaptionTone::Error,
            vec![UpdateAction::Check],
        ),
    })
}

/// The confirmation for one prepared change, in the wording every control surface shows.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct UpdateConfirmation {
    pub(super) title: &'static str,
    pub(super) message: SharedString,
    pub(super) detail: SharedString,
    pub(super) confirm: String,
}

impl UpdateConfirmation {
    /// Uses the shared confirmation for this application's server.
    pub(super) fn of(prepared: &PreparedChange) -> Self {
        let identity = ApplicationIdentity::current();
        let wording = UpdateWording::new(
            identity.directory_name(),
            prepared.change,
            &prepared.previous,
            &prepared.candidate,
            prepared.running,
            prepared.stopped,
        );
        Self {
            title: wording.title(),
            message: wording.message().into(),
            detail: wording.detail().into(),
            confirm: wording.confirm_label(),
        }
    }
}

/// Presents update state and owns preparation, confirmation, and passive reads.
pub(super) struct ServerUpdateRow {
    control: Arc<dyn ServerUpdateControl>,
    pub(super) state: UpdateState,
    /// The Settings window, which presents the confirmation once a background step prepared it.
    window: AnyWindowHandle,
    /// Another surface asked for the update journey. It starts once the row is idle.
    requested: bool,
    /// A confirmation is open. The row starts nothing else until the person answers it.
    asking: bool,
    _operation: Option<Task<()>>,
}

impl ServerUpdateRow {
    pub(super) fn new(control: Arc<dyn ServerUpdateControl>, window: AnyWindowHandle) -> Self {
        Self {
            control,
            state: UpdateState::default(),
            window,
            requested: false,
            asking: false,
            _operation: None,
        }
    }

    pub(super) fn presentation(&self) -> UpdatePresentation {
        present_update(&self.state)
    }

    /// Whether a step that may change the service is running.
    pub(super) fn changing(&self) -> bool {
        self.state.working.is_some_and(UpdateStep::changes_service)
    }
}

impl SettingsWindow {
    pub(super) fn server_update_presentation(&self) -> Option<UpdatePresentation> {
        self.server_update
            .as_ref()
            .map(ServerUpdateRow::presentation)
    }

    /// Whether the row can start `step` now. One server operation runs at a time across the
    /// Servers rows, so the service and connection rows must be idle for a change.
    fn server_update_ready(&self, step: UpdateStep) -> bool {
        // A connection check only reads, and the change checks the connection again when it
        // finishes, so a confirmed change never waits on one.
        let service_busy = self.server_service_working();
        self.server_update.as_ref().is_some_and(|row| {
            row.state.working.is_none() && !row.asking && !(step.changes_service() && service_busy)
        })
    }

    /// Reads the server version as the section comes into view, and after another row changed
    /// the service.
    pub(super) fn check_server_update(&mut self, cx: &mut Context<Self>) {
        self.run_update_step(UpdateStep::Check, cx);
    }

    /// Another surface, such as the incompatible-server screen or the status item, asked for the
    /// update journey. It starts once nothing else runs, and only if a read still offers it.
    pub(crate) fn request_server_update(&mut self, cx: &mut Context<Self>) {
        let Some(row) = &mut self.server_update else {
            return;
        };
        row.requested = true;
        self.continue_requested_update(cx);
    }

    /// Starts a requested update once a fresh read offers it. A read that no longer offers it
    /// drops the request, and the row says why.
    pub(super) fn continue_requested_update(&mut self, cx: &mut Context<Self>) {
        let Some(row) = &self.server_update else {
            return;
        };
        if !row.requested || row.state.working.is_some() || row.asking {
            return;
        }
        // A row that has not read yet reads first. Its result continues the request.
        if row.state.facts.is_none() && row.state.failure.is_none() {
            self.check_server_update(cx);
            return;
        }
        let offered = row.presentation().actions.contains(&UpdateAction::Update);
        if let Some(row) = &mut self.server_update {
            row.requested = false;
        }
        if offered {
            self.run_update_step(UpdateStep::Prepare, cx);
        }
    }

    pub(super) fn request_update_action(&mut self, action: UpdateAction, cx: &mut Context<Self>) {
        let offered = self
            .server_update_presentation()
            .is_some_and(|presentation| presentation.actions.contains(&action));
        if !offered {
            return;
        }
        let step = match action {
            UpdateAction::Check => UpdateStep::Check,
            UpdateAction::Update => UpdateStep::Prepare,
            UpdateAction::Finish => UpdateStep::Finish,
            UpdateAction::Return => UpdateStep::PrepareReturn,
        };
        self.run_update_step(step, cx);
    }

    /// Runs one step on the background executor, then reads again so the row presents what is
    /// true afterwards. A prepared change waits for the person's confirmation.
    fn run_update_step(&mut self, step: UpdateStep, cx: &mut Context<Self>) {
        debug_assert!(!matches!(
            step,
            UpdateStep::Update { .. } | UpdateStep::Return
        ));
        if !self.server_update_ready(step) {
            return;
        }
        if step == UpdateStep::Finish {
            let Some(control) = self
                .server_update
                .as_ref()
                .map(|row| Arc::clone(&row.control))
            else {
                return;
            };
            self.run_admitted_update(step, move || control.finish(), cx);
            return;
        }
        let Some(row) = &mut self.server_update else {
            return;
        };
        let control = Arc::clone(&row.control);
        row.state.working = Some(step);
        let prior_failure = if step == UpdateStep::Check {
            self.local_operations.read(cx).update_failure
        } else {
            self.local_operations
                .update(cx, |owner, _| owner.update_failure = None);
            None
        };
        row.state.failure = None;
        let result = cx.background_spawn(async move {
            let outcome = match step {
                UpdateStep::Prepare => control.prepare_update(),
                UpdateStep::PrepareReturn => control.prepare_return().map(Some),
                _ => Ok(None),
            };
            let (prepared, failure) = match outcome {
                Ok(prepared) => (prepared, prior_failure),
                Err(error) => (None, Some(error)),
            };
            let read = control.read();
            (prepared, failure, read)
        });
        row._operation = Some(cx.spawn(async move |settings, cx| {
            let (prepared, failure, read) = result.await;
            let _ = settings.update(cx, |settings, cx| {
                settings.update_step_finished(prepared, failure, read, cx);
            });
        }));
        cx.notify();
    }

    fn update_step_finished(
        &mut self,
        prepared: Option<PreparedChange>,
        failure: Option<ServerError>,
        read: Result<UpdateFacts, ServerError>,
        cx: &mut Context<Self>,
    ) {
        let Some(row) = &mut self.server_update else {
            return;
        };
        row.state.working = None;
        match read {
            Ok(facts) => {
                row.state.facts = Some(facts);
                row.state.failure = failure;
            }
            Err(error) => {
                row.state.facts = None;
                row.state.failure = failure.or(Some(error));
            }
        }
        let state = row.state.clone();
        self.local_operations
            .update(cx, |owner, _| owner.update = Some(state));
        if let Some(prepared) = prepared {
            self.confirm_change(prepared, cx);
        }
        self.continue_requested_update(cx);
        self.continue_requested_start(cx);
        cx.notify();
    }

    /// Asks before the prepared change. Cancel is the default, so Return and Escape both leave
    /// the server as it is. Cancelling keeps a verified build retained for a later update.
    fn confirm_change(&mut self, prepared: PreparedChange, cx: &mut Context<Self>) {
        let Some(handle) = self.server_update.as_ref().map(|row| row.window) else {
            return;
        };
        let confirmation = UpdateConfirmation::of(&prepared);
        let (id, confirm_selector, cancel_selector) = match prepared.change {
            UpdateChange::Update => (
                "settings-server-update",
                UPDATE_CONFIRM_SELECTOR,
                UPDATE_CANCEL_SELECTOR,
            ),
            UpdateChange::Return => (
                "settings-server-return",
                RETURN_CONFIRM_SELECTOR,
                RETURN_CANCEL_SELECTOR,
            ),
        };
        if let Some(row) = &mut self.server_update {
            row.asking = true;
        }
        let owner = cx.weak_entity();
        let entity = cx.entity();
        // The background step finished outside any window update, so the alert is presented
        // through the Settings window it belongs to.
        cx.defer(move |cx| {
            let presented = handle.update(cx, |_, window, cx| {
                entity.update(cx, |_, cx| {
                    let alert = Alert::new(
                        ModalId::new(id),
                        confirmation.title,
                        confirmation.message.clone(),
                        confirmation.detail.clone(),
                        vec![
                            ModalAction::new(
                                true,
                                confirmation.confirm.clone(),
                                ModalActionRole::Affirmative,
                                confirm_selector,
                            )
                            .with_intent(ModalActionIntent::Destructive)
                            .with_emphasis(ModalActionEmphasis::Prominent),
                            ModalAction::new(
                                false,
                                "Cancel",
                                ModalActionRole::Cancel,
                                cancel_selector,
                            ),
                        ],
                    )
                    .intent(AlertIntent::Critical);
                    let result = alert.present(window, cx, move |outcome, cx| {
                        let confirmed = matches!(
                            outcome,
                            AlertOutcome::Activated {
                                action_id: true,
                                ..
                            }
                        );
                        let _ = owner.update(cx, |settings, cx| {
                            if let Some(row) = &mut settings.server_update {
                                row.asking = false;
                            }
                            if confirmed {
                                settings.commit_change(prepared, cx);
                            }
                            cx.notify();
                        });
                    });
                    if result.is_err() {
                        eprintln!("failed to present the SpaceTerm server update confirmation");
                    }
                    result.is_ok()
                })
            });
            // An alert that could not be presented asks nothing, so the row is free again.
            if !presented.unwrap_or(false) {
                entity.update(cx, |settings, cx| {
                    if let Some(row) = &mut settings.server_update {
                        row.asking = false;
                    }
                    cx.notify();
                });
            }
        });
    }

    /// Runs the change the person confirmed. The service refuses it if its observation changed,
    /// and the person then reviews a fresh confirmation.
    fn commit_change(&mut self, prepared: PreparedChange, cx: &mut Context<Self>) {
        let step = match prepared.change {
            UpdateChange::Update => UpdateStep::Update {
                running: prepared.running,
            },
            UpdateChange::Return => UpdateStep::Return,
        };
        if !self.server_update_ready(step) {
            return;
        }
        self.run_admitted_update(step, move || (prepared.commit)().map(Some), cx);
    }

    fn run_admitted_update(
        &mut self,
        step: UpdateStep,
        change: impl FnOnce() -> Result<Option<ActivationStatus>, ServerError> + Send + 'static,
        cx: &mut Context<Self>,
    ) {
        let Some(row) = &mut self.server_update else {
            return;
        };
        let control = Arc::clone(&row.control);
        row.state.working = Some(step);
        row.state.failure = None;
        let state = row.state.clone();
        let result = cx.background_spawn(async move {
            let outcome = change();
            let failure = outcome.as_ref().err().copied();
            let state = match control.read() {
                Ok(facts) => UpdateState {
                    facts: Some(facts),
                    failure,
                    working: None,
                },
                Err(error) => UpdateState {
                    facts: None,
                    failure: failure.or(Some(error)),
                    working: None,
                },
            };
            UpdateCompletion { state, outcome }
        });
        self.local_operations
            .update(cx, |owner, cx| owner.run_update(state, result, cx));
        cx.notify();
    }
}

/// An update control whose answers a test sets and whose operations it records.
#[cfg(test)]
pub(super) mod testing {
    use std::sync::{Arc, Mutex};

    use super::{IncludedServer, PreparedChange, ServerUpdateControl, UpdateFacts};
    use crate::server_connection::ServerError;
    use crate::server_connection::service::RetainedInstallation;
    use crate::server_connection::service::activation::{ActivationPhase, ActivationStatus};
    use crate::server_connection::service::control::UpdateChange;
    use crate::ui::settings_window::servers::testing::{retained, running, stopped};

    /// One operation a [`ScriptedUpdate`] received.
    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    pub(in crate::ui::settings_window) enum UpdateCall {
        Read,
        PrepareUpdate,
        Activate,
        PrepareReturn,
        Return,
        Finish,
    }

    /// What the confirmed activation does.
    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    pub(in crate::ui::settings_window) enum Outcome {
        /// The candidate becomes the approved build. A stopped server stays stopped.
        Ready,
        /// The service's observation changed after the confirmation was prepared.
        Changed,
        /// The candidate never became ready, and the service records it.
        Failed { can_return: bool },
    }

    /// The approved build the scripted service starts with.
    pub(in crate::ui::settings_window) fn approved() -> RetainedInstallation {
        retained("1.4.0", 'a')
    }

    /// The build this SpaceTerm includes.
    pub(in crate::ui::settings_window) fn included() -> RetainedInstallation {
        retained("1.5.0", 'b')
    }

    /// The package digest a retained installation is named by.
    pub(in crate::ui::settings_window) fn package(installation: &RetainedInstallation) -> String {
        installation
            .directory
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default()
            .to_owned()
    }

    /// The facts of a running server whose approved build differs from the included one.
    pub(in crate::ui::settings_window) fn available() -> UpdateFacts {
        UpdateFacts {
            status: Ok(running()),
            approved: Some(approved()),
            included: Ok(IncludedServer {
                version: included().version,
                commit: included().commit,
                package: package(&included()),
                approved: false,
                earlier: false,
            }),
            activation: None,
        }
    }

    /// The scripted service's shared state, which a prepared change updates when it runs.
    #[derive(Default)]
    struct Shared {
        facts: Option<UpdateFacts>,
        outcome: Option<Outcome>,
        calls: Vec<UpdateCall>,
    }

    impl Shared {
        fn facts(&mut self) -> &mut UpdateFacts {
            self.facts.as_mut().expect("facts")
        }

        fn record(&self, phase: ActivationPhase, returned: bool) -> ActivationStatus {
            ActivationStatus {
                operation: [7; 32],
                previous: approved(),
                candidate: included(),
                stopped: self
                    .facts
                    .as_ref()
                    .and_then(|facts| facts.status.ok())
                    .is_some_and(|status| status.stopped),
                phase,
                returned,
            }
        }

        /// The included build becomes the approved one, as a completed activation leaves it.
        fn approve_included(&mut self, running: bool) -> ActivationStatus {
            let status = self.record(ActivationPhase::Completed { running }, false);
            let facts = self.facts();
            facts.approved = Some(included());
            if let Ok(included) = &mut facts.included {
                included.approved = true;
            }
            facts.activation = Some(status.clone());
            status
        }

        fn activate(&mut self) -> Result<ActivationStatus, ServerError> {
            self.calls.push(UpdateCall::Activate);
            match self.outcome.unwrap_or(Outcome::Ready) {
                Outcome::Changed => Err(ServerError::Changed),
                Outcome::Ready => {
                    let running = self.facts().status.is_ok_and(|status| status.running);
                    Ok(self.approve_included(running))
                }
                Outcome::Failed { can_return } => {
                    let status = self.record(ActivationPhase::Failed { can_return }, false);
                    let facts = self.facts();
                    facts.status = Ok(stopped());
                    facts.activation = Some(status);
                    Err(ServerError::StartupFailed)
                }
            }
        }
    }

    pub(in crate::ui::settings_window) struct ScriptedUpdate {
        shared: Arc<Mutex<Shared>>,
        /// The refusal the next preparation returns.
        refusal: Mutex<Option<ServerError>>,
        read_failure: Mutex<Option<ServerError>>,
    }

    impl ScriptedUpdate {
        pub(in crate::ui::settings_window) fn new(facts: UpdateFacts) -> Arc<Self> {
            Arc::new(Self {
                shared: Arc::new(Mutex::new(Shared {
                    facts: Some(facts),
                    ..Shared::default()
                })),
                refusal: Mutex::new(None),
                read_failure: Mutex::new(None),
            })
        }

        fn shared(&self) -> std::sync::MutexGuard<'_, Shared> {
            self.shared.lock().expect("shared")
        }

        /// Changes what the service reports, as another surface would.
        pub(in crate::ui::settings_window) fn change(&self, change: impl FnOnce(&mut UpdateFacts)) {
            change(self.shared().facts());
        }

        pub(in crate::ui::settings_window) fn refuse_next(&self, error: ServerError) {
            *self.refusal.lock().expect("refusal") = Some(error);
        }

        pub(in crate::ui::settings_window) fn refuse_next_read(&self, error: ServerError) {
            *self.read_failure.lock().expect("read failure") = Some(error);
        }

        pub(in crate::ui::settings_window) fn answer(&self, outcome: Outcome) {
            self.shared().outcome = Some(outcome);
        }

        /// How many times the version was read.
        pub(in crate::ui::settings_window) fn reads(&self) -> usize {
            self.shared()
                .calls
                .iter()
                .filter(|call| **call == UpdateCall::Read)
                .count()
        }

        /// The operations other than reads.
        pub(in crate::ui::settings_window) fn changes(&self) -> Vec<UpdateCall> {
            self.shared()
                .calls
                .iter()
                .copied()
                .filter(|call| *call != UpdateCall::Read)
                .collect()
        }
    }

    impl ServerUpdateControl for ScriptedUpdate {
        fn read(&self) -> Result<UpdateFacts, ServerError> {
            let mut shared = self.shared();
            shared.calls.push(UpdateCall::Read);
            if let Some(error) = self.read_failure.lock().expect("read failure").take() {
                return Err(error);
            }
            Ok(shared.facts().clone())
        }

        fn prepare_update(&self) -> Result<Option<PreparedChange>, ServerError> {
            let mut shared = self.shared();
            shared.calls.push(UpdateCall::PrepareUpdate);
            if let Some(error) = self.refusal.lock().expect("refusal").take() {
                return Err(error);
            }
            let facts = shared.facts().clone();
            if facts
                .included
                .as_ref()
                .is_ok_and(|included| included.approved)
            {
                return Ok(None);
            }
            let commit = Arc::clone(&self.shared);
            Ok(Some(PreparedChange {
                change: UpdateChange::Update,
                previous: approved(),
                candidate: included(),
                running: facts.status.is_ok_and(|status| status.running),
                stopped: facts.status.is_ok_and(|status| status.stopped),
                commit: Box::new(move || commit.lock().expect("shared").activate()),
            }))
        }

        fn prepare_return(&self) -> Result<PreparedChange, ServerError> {
            self.shared().calls.push(UpdateCall::PrepareReturn);
            let commit = Arc::clone(&self.shared);
            Ok(PreparedChange {
                change: UpdateChange::Return,
                previous: approved(),
                candidate: included(),
                running: false,
                stopped: true,
                commit: Box::new(move || {
                    let mut shared = commit.lock().expect("shared");
                    shared.calls.push(UpdateCall::Return);
                    let status = shared.record(ActivationPhase::Completed { running: false }, true);
                    shared.facts().activation = Some(status.clone());
                    Ok(status)
                }),
            })
        }

        fn finish(&self) -> Result<Option<ActivationStatus>, ServerError> {
            let mut shared = self.shared();
            shared.calls.push(UpdateCall::Finish);
            let running = shared.facts().status.is_ok_and(|status| status.running);
            Ok(Some(shared.approve_included(running)))
        }
    }
}

#[cfg(test)]
#[path = "server_update_tests.rs"]
mod tests;
