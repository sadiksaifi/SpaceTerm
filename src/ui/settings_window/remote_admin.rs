//! Status, Stop, Restart and Update of a paired server from its Servers card.
//!
//! The card takes the CLI's steps over the server's service lane:
//! [`spaceterm_server::service::lane::admin`] settles what this desktop left pending, previews
//! each change, and follows a confirmed one until it settles. A confirmation binds the server's
//! own preview, so the server refuses a change whose Terminal Sessions or service changed since.
//! Start stays on the server's own computer, because a stopped server accepts no connection.
//!
//! Nothing here connects on its own: Check Now, Stop, Restart and Update each open a lane
//! connection only when the person asks. Each route keeps its work and its last outcome while
//! this window is open, so selecting another server and back shows them again.

use std::collections::HashMap;
use std::sync::Arc;

use gpui::{AnyWindowHandle, App, AppContext as _, Context, Global, SharedString, Task};
use spaceterm_server::service::control::{self, BuildName, UpdateChange, UpdateWording, Wording};
use spaceterm_server::service::lane::admin::{self, Preview, UpdatePlan, Verdict};
use spaceterm_server::service::lane::client::{LaneError, Outcome};
use spaceterm_server::service::lane::{
    LaneAction, LaneCheck, LaneReceipt, LaneStatus, LaneUpdateKind, LaneVersion,
};
use spaceterm_ui::{
    Alert, AlertIntent, AlertOutcome, ModalAction, ModalActionEmphasis, ModalActionIntent,
    ModalActionRole, ModalId,
};

use super::SettingsWindow;
use crate::direct_connections::{DirectRouteId, DirectRouteStatus, RefusalReason};
use crate::server_connection::{ServerConnection, ServerError};
use crate::ui::sidebar_window::form::CaptionTone;

pub(super) const STOP_SELECTOR: &str = "settings-remote-server-stop";
pub(super) const RESTART_SELECTOR: &str = "settings-remote-server-restart";
pub(super) const UPDATE_SELECTOR: &str = "settings-remote-server-update";
pub(super) const CONFIRM_SELECTOR: &str = "settings-remote-server-confirm";
pub(super) const CANCEL_SELECTOR: &str = "settings-remote-server-cancel";

/// A change the person can ask of a paired server.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Change {
    Stop,
    Restart,
    /// Updates to the server's newest signed release, or returns to its previous version when
    /// the server offers that instead.
    Update,
}

impl Change {
    pub(super) const fn label(self) -> &'static str {
        match self {
            Self::Stop => "Stop Server",
            Self::Restart => "Restart Server",
            Self::Update => "Update\u{2026}",
        }
    }

    pub(super) const fn selector(self) -> &'static str {
        match self {
            Self::Stop => STOP_SELECTOR,
            Self::Restart => RESTART_SELECTOR,
            Self::Update => UPDATE_SELECTOR,
        }
    }
}

/// What the lane read of a paired server returned.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Reading {
    pub(crate) status: LaneStatus,
    pub(crate) check: LaneCheck,
    /// Operations this desktop admitted earlier that this connection settled.
    pub(crate) settled: Vec<LaneReceipt>,
}

/// A preview the server issued, and what this connection settled first.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Prepared {
    pub(crate) preview: Preview,
    pub(crate) settled: Vec<LaneReceipt>,
}

/// How a confirmed change ended, and what its connection settled first.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Performed {
    pub(crate) outcome: Outcome,
    pub(crate) settled: Vec<LaneReceipt>,
}

/// Why a lane step didn't do what was asked. Each names a classification only.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RemoteFailure {
    /// SpaceTerm couldn't reach the server's service lane.
    Unreachable,
    /// The server's service or Terminal Sessions changed since its preview.
    Changed,
    /// Another service operation is underway on the server.
    Busy,
    /// The server holds as many unfinished operations as it can.
    Capacity,
    /// The server found no newer release for itself.
    NoUpdate,
    /// The server couldn't verify the release it fetched.
    Unverified,
    /// An earlier update on the server hasn't finished, so it takes no other.
    ActivationUnresolved,
    /// The server didn't start after the change.
    StartupFailed,
    /// The server refused this device's pairing.
    Refused,
    /// The server answered in a way this SpaceTerm doesn't read.
    Protocol,
    /// Any other refusal or failure.
    Failed,
}

impl RemoteFailure {
    /// The classification of a CLI contract code the lane or its ledger carries.
    pub(crate) fn of_code(code: &str) -> Self {
        match code {
            "unreachable" | "not_running" | "stopped" | "network_off" => Self::Unreachable,
            "changed" | "candidate_changed" => Self::Changed,
            "service_busy" => Self::Busy,
            "capacity" => Self::Capacity,
            "no_candidate" => Self::NoUpdate,
            "candidate_unverified" => Self::Unverified,
            "activation_unresolved" => Self::ActivationUnresolved,
            "startup_failed" => Self::StartupFailed,
            "authentication_refused"
            | "revoked"
            | "server_identity_changed"
            | "wrong_server"
            | "credential_unavailable"
            | "credential_interaction_required" => Self::Refused,
            "protocol_error" | "incompatible_protocol" => Self::Protocol,
            _ => Self::Failed,
        }
    }

    pub(crate) fn of_server(error: ServerError) -> Self {
        Self::of_code(spaceterm_server::service::lane_codes::code(&error))
    }

    pub(crate) fn of_lane(error: LaneError) -> Self {
        match error {
            LaneError::Code(code) => Self::of_code(&code),
            LaneError::Server(error) => Self::of_server(error),
            LaneError::Interrupted => Self::Failed,
        }
    }

    /// What the card says about this failure of `name`.
    fn explanation(self, name: &str) -> String {
        match self {
            Self::Unreachable => {
                format!(
                    "SpaceTerm couldn\u{2019}t reach {name}. Check that it\u{2019}s running, then try again."
                )
            }
            Self::Changed => {
                format!("{name} changed after SpaceTerm asked, so nothing changed. Try again.")
            }
            Self::Busy => {
                format!("{name} is finishing another change. Try again once it\u{2019}s done.")
            }
            Self::Capacity => {
                format!("{name} holds too many unfinished changes. Try again later.")
            }
            Self::NoUpdate => format!("{name} found no newer release to install."),
            Self::Unverified => {
                format!("{name} couldn\u{2019}t verify the update, so nothing changed.")
            }
            Self::ActivationUnresolved => format!(
                "An earlier update of {name} hasn\u{2019}t finished. Finish it on its own computer with \u{201C}spaceterm server update\u{201D}."
            ),
            Self::StartupFailed => format!("{name} didn\u{2019}t start after the change."),
            Self::Refused => {
                format!("{name} didn\u{2019}t accept this device. Reconnect, then try again.")
            }
            Self::Protocol => {
                format!("{name} answered in a way SpaceTerm doesn\u{2019}t understand.")
            }
            Self::Failed => format!("{name} couldn\u{2019}t finish the change."),
        }
    }
}

/// The blocking service-lane steps for one paired server, which the card runs on the background
/// executor. Each step opens its own lane connection and first settles what this desktop left
/// pending on it.
pub(crate) trait RemoteServerControl: Send + Sync {
    /// Reads the server's Status and its update check. Changes nothing.
    fn read(&self) -> Result<Reading, RemoteFailure>;
    /// Asks the server to preview `change`. Update first has the server stage its newest signed
    /// release, unless it offers a return to its previous version instead.
    fn prepare(&self, change: Change) -> Result<Prepared, RemoteFailure>;
    /// Executes a confirmed preview and follows it until it settles or its wait passes.
    fn perform(&self, preview: Preview) -> Result<Performed, RemoteFailure>;
}

/// The steps over a paired server's own lane, with its pending operations kept beside this
/// desktop's other records for the route.
struct LaneControl {
    server: ServerConnection,
}

impl LaneControl {
    fn open(
        &self,
    ) -> Result<
        (
            spaceterm_server::service::lane::client::Lane,
            admin::PendingOperations,
            spaceterm_server::service::lane::client::Session,
            Vec<LaneReceipt>,
        ),
        RemoteFailure,
    > {
        let lane = self
            .server
            .service_lane()
            .map_err(RemoteFailure::of_server)?;
        let pending = self
            .server
            .service_operations()
            .map_err(RemoteFailure::of_server)?;
        let (session, settled) = admin::open(&lane, &pending).map_err(RemoteFailure::of_lane)?;
        Ok((lane, pending, session, settled))
    }
}

impl RemoteServerControl for LaneControl {
    fn read(&self) -> Result<Reading, RemoteFailure> {
        let (_, _, mut session, settled) = self.open()?;
        let status = admin::status(&mut session).map_err(RemoteFailure::of_lane)?;
        let check = admin::check(&mut session).map_err(RemoteFailure::of_lane)?;
        Ok(Reading {
            status,
            check,
            settled,
        })
    }

    fn prepare(&self, change: Change) -> Result<Prepared, RemoteFailure> {
        let (_, _, mut session, settled) = self.open()?;
        let preview = match change {
            Change::Stop => admin::preview(&mut session, LaneAction::Stop),
            Change::Restart => admin::preview(&mut session, LaneAction::Restart),
            Change::Update => admin::plan_update(&mut session).map(|plan| match plan {
                UpdatePlan::Return(preview) | UpdatePlan::Update { preview, .. } => preview,
            }),
        }
        .map_err(RemoteFailure::of_lane)?;
        Ok(Prepared { preview, settled })
    }

    fn perform(&self, preview: Preview) -> Result<Performed, RemoteFailure> {
        let (lane, pending, session, settled) = self.open()?;
        let wait = admin::wait(&preview.action);
        let outcome = admin::execute(
            &lane,
            session,
            preview.action,
            preview.token,
            wait,
            &pending,
            &mut || Ok(()),
            // A window that closes leaves the flow to finish on its own thread; nothing
            // interrupts it.
            &|| None,
        )
        .map_err(RemoteFailure::of_lane)?;
        Ok(Performed { outcome, settled })
    }
}

/// Builds the lane steps for a paired server's connection.
pub(crate) type ControlSource =
    Arc<dyn Fn(ServerConnection) -> Arc<dyn RemoteServerControl> + Send + Sync>;

struct InstalledSource(ControlSource);
impl Global for InstalledSource {}

/// Makes `source` the desktop's way to reach a paired server's service lane. Without it, paired
/// server cards offer no Stop, Restart or Update.
pub(crate) fn install(source: ControlSource, cx: &mut App) {
    cx.set_global(InstalledSource(source));
}

/// The lane steps the desktop takes in production.
pub(crate) fn lane_source() -> ControlSource {
    Arc::new(|server| Arc::new(LaneControl { server }) as Arc<dyn RemoteServerControl>)
}

pub(super) fn installed(cx: &App) -> Option<ControlSource> {
    cx.try_global::<InstalledSource>()
        .map(|installed| Arc::clone(&installed.0))
}

/// The change a confirmed preview makes, named by the version it changes to.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum Act {
    Stop,
    Restart,
    Update(Option<SharedString>),
    Return(Option<SharedString>),
}

impl Act {
    fn of(action: &LaneAction, to: Option<&LaneVersion>) -> Self {
        let to = to.map(|to| SharedString::from(to.version.clone()));
        match action {
            LaneAction::Stop => Self::Stop,
            LaneAction::Restart => Self::Restart,
            LaneAction::Update { .. } => Self::Update(to),
            LaneAction::Return => Self::Return(to),
        }
    }

    /// The noun an earlier operation is named by.
    const fn noun(&self) -> &'static str {
        match self {
            Self::Stop => "stop",
            Self::Restart => "restart",
            Self::Update(_) => "update",
            Self::Return(_) => "return to the previous version",
        }
    }

    /// The verb a failure is named by.
    const fn verb(&self) -> &'static str {
        match self {
            Self::Stop => "stop",
            Self::Restart => "restart",
            Self::Update(_) => "update",
            Self::Return(_) => "return to its previous version",
        }
    }
}

/// What the client observed of a dispatched change.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum Settlement {
    Done,
    Failed(RemoteFailure),
    NotPerformed,
    /// Nothing proves whether the change happened.
    Uncertain,
    /// The server closed the lane after Stop was admitted, or may have been: it is stopping or
    /// stopped, and only its own computer can read the outcome.
    Shutdown,
}

impl Settlement {
    fn of(verdict: Verdict) -> Self {
        match verdict {
            Verdict::Done => Self::Done,
            Verdict::Failed(code) => Self::Failed(
                code.as_deref()
                    .map_or(RemoteFailure::Failed, RemoteFailure::of_code),
            ),
            Verdict::NotPerformed => Self::NotPerformed,
            Verdict::Uncertain => Self::Uncertain,
            Verdict::Shutdown => Self::Shutdown,
        }
    }
}

/// What the card says until the person asks something else of the server.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum Ended {
    /// Nothing was dispatched: the read, preview or execution was refused.
    Refused(RemoteFailure),
    /// The confirmed change ran this far.
    Changed(Act, Settlement),
}

/// The step underway on one paired server.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum Work {
    Reading,
    Preparing(Change),
    /// The confirmation is open.
    Confirming,
    Performing(Act),
}

#[derive(Default)]
struct RouteAdmin {
    status: Option<LaneStatus>,
    check: Option<LaneCheck>,
    /// The connection and its link generation when `status` and `check` were read. A reconnect
    /// may reach a server that restarted or updated since, so a reading belongs to that link.
    read_over: Option<(ServerConnection, u64)>,
    work: Option<Work>,
    ended: Option<Ended>,
    /// The last earlier operation a connection settled, until the person asks something else.
    earlier: Option<(Act, Settlement)>,
    /// SpaceTerm's connection to a server it stopped has ended, so connecting again means
    /// someone started it on its own computer.
    left: bool,
    _task: Option<Task<()>>,
}

/// Each paired server's lane work and its last outcome in this window.
#[derive(Default)]
pub(super) struct RemoteAdministration {
    routes: HashMap<DirectRouteId, RouteAdmin>,
}

/// The confirmation of one preview. Its message states what ends and that every client loses
/// the server while it changes.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct RemoteConfirmation {
    pub(super) title: &'static str,
    pub(super) message: String,
    pub(super) detail: String,
    pub(super) confirm: String,
}

/// What ends for every client: the exact count the server's preview named.
fn sessions_end(count: u32) -> String {
    match count {
        0 => "No Terminal Sessions are running on it.".to_owned(),
        1 => "Its 1 Terminal Session ends for every client.".to_owned(),
        count => format!("Its {count} Terminal Sessions end for every client."),
    }
}

/// How a person tells a lane build apart from another.
fn build(version: &LaneVersion) -> BuildName<'_> {
    BuildName {
        version: &version.version,
        commit: &version.commit,
        package: &version.sha256,
    }
}

/// The confirmation for `preview` of the server named `name`.
pub(super) fn confirmation(name: &str, preview: &Preview) -> RemoteConfirmation {
    let effects = &preview.effects;
    let ends = sessions_end(effects.ends_sessions);
    // A server stopped or restarted from here starts again on its own computer, so the shared
    // wording's startup clause never shows.
    let startup = spaceterm_server::service::ServiceStartup::AtLogin;
    match &preview.action {
        LaneAction::Stop | LaneAction::Restart => {
            let (action, detail) = if preview.action == LaneAction::Stop {
                (
                    control::Action::Stop,
                    format!(
                        "{ends} Every client connected to it loses access, and Workspaces are \
                         kept. It stays stopped until someone starts it on its own computer."
                    ),
                )
            } else {
                (
                    control::Action::Restart,
                    format!(
                        "{ends} Every client connected to it disconnects until it starts again, \
                         and Workspaces are kept."
                    ),
                )
            };
            let wording = Wording::for_server(name, action, true, startup);
            RemoteConfirmation {
                title: wording.title(),
                message: wording.message(),
                detail,
                confirm: wording.confirm_label().to_owned(),
            }
        }
        LaneAction::Update { .. } | LaneAction::Return => {
            let unnamed = LaneVersion {
                version: String::new(),
                commit: String::new(),
                sha256: String::new(),
            };
            let from = effects.from.as_ref().unwrap_or(&unnamed);
            let to = effects.to.as_ref().unwrap_or(&unnamed);
            let returning = effects.kind == Some(LaneUpdateKind::Return)
                || preview.action == LaneAction::Return;
            // A return names the version it returns to as the previous one.
            let wording = if returning {
                UpdateWording::for_server(
                    name,
                    UpdateChange::Return,
                    build(to),
                    build(from),
                    true,
                    false,
                )
            } else {
                UpdateWording::for_server(
                    name,
                    UpdateChange::Update,
                    build(from),
                    build(to),
                    true,
                    false,
                )
            };
            let (previous, candidate) = control::build_labels(build(from), build(to));
            let mut detail = format!(
                "It changes from {previous} to {candidate}, replacing the running server. {ends} \
                 Every client connected to it disconnects until it starts again, and Workspaces \
                 are kept."
            );
            if effects
                .to_major
                .is_some_and(|major| major != spaceterm_server::protocol::MAJOR)
            {
                detail.push_str(&format!(
                    " Afterward, SpaceTerm here needs its own update to open {name}\u{2019}s \
                     Workspaces."
                ));
            }
            RemoteConfirmation {
                title: wording.title(),
                message: wording.message(),
                detail,
                confirm: wording.confirm_label(),
            }
        }
    }
}

/// What the card says about `ended` on the server named `name`, and in which tone.
pub(super) fn ended_message(name: &str, ended: &Ended) -> (String, CaptionTone) {
    match ended {
        Ended::Refused(failure) => (failure.explanation(name), CaptionTone::Error),
        Ended::Changed(Act::Stop, Settlement::Done) => (
            format!(
                "{name} stopped, and its Terminal Sessions ended for every client. Start it again \
                 on its own computer."
            ),
            CaptionTone::Guidance,
        ),
        // The server closes the lane as it stops, so its own record of the stop is readable
        // only on its computer. A Stop that ran out of time with the lane open reads as any
        // other uncertain change.
        Ended::Changed(_, Settlement::Shutdown) => (
            format!(
                "{name} closed its connection while stopping, so SpaceTerm can\u{2019}t confirm \
                 the stop finished. Check on its own computer with \u{201C}spaceterm server \
                 status\u{201D}, and start it again there."
            ),
            CaptionTone::Warning,
        ),
        Ended::Changed(Act::Restart, Settlement::Done) => {
            (format!("{name} restarted."), CaptionTone::Guidance)
        }
        Ended::Changed(Act::Update(to), Settlement::Done) => (
            to.as_ref().map_or_else(
                || format!("{name} updated."),
                |to| format!("{name} updated to {to}."),
            ),
            CaptionTone::Guidance,
        ),
        Ended::Changed(Act::Return(to), Settlement::Done) => (
            to.as_ref().map_or_else(
                || format!("{name} returned to its previous version."),
                |to| format!("{name} returned to {to}."),
            ),
            CaptionTone::Guidance,
        ),
        Ended::Changed(act, Settlement::Uncertain) => (
            format!(
                "SpaceTerm couldn\u{2019}t confirm whether {name} finished its {}. Check Now asks \
                 {name} again.",
                act.noun()
            ),
            CaptionTone::Warning,
        ),
        Ended::Changed(act, Settlement::NotPerformed) => (
            format!("{name} didn\u{2019}t {}, so nothing changed.", act.verb()),
            CaptionTone::Warning,
        ),
        Ended::Changed(act, Settlement::Failed(failure)) => {
            let reason = match failure {
                RemoteFailure::Failed => String::new(),
                failure => format!(" {}", failure.explanation(name)),
            };
            (
                format!("{name} couldn\u{2019}t {}.{reason}", act.verb()),
                CaptionTone::Error,
            )
        }
    }
}

/// What the card says about an earlier operation a connection settled.
fn earlier_message(name: &str, act: &Act, settlement: &Settlement) -> (String, CaptionTone) {
    let noun = act.noun();
    match settlement {
        Settlement::Done => (
            format!("SpaceTerm confirmed that an earlier {noun} of {name} finished."),
            CaptionTone::Guidance,
        ),
        // A settled receipt is never a shutdown, which only a lost lane reports.
        Settlement::Uncertain | Settlement::Shutdown => (
            format!("{name} couldn\u{2019}t confirm whether an earlier {noun} finished."),
            CaptionTone::Warning,
        ),
        Settlement::NotPerformed => (
            format!("An earlier {noun} of {name} didn\u{2019}t run."),
            CaptionTone::Warning,
        ),
        Settlement::Failed(_) => (
            format!("An earlier {noun} of {name} didn\u{2019}t finish."),
            CaptionTone::Error,
        ),
    }
}

/// The last settled receipt, as the card names it.
fn earlier(settled: &[LaneReceipt]) -> Option<(Act, Settlement)> {
    let receipt = settled.last()?;
    Some((
        Act::of(&receipt.action, None),
        Settlement::of(admin::receipt_verdict(receipt)),
    ))
}

/// What a finished flow established about `act`.
fn settlement(outcome: &Outcome) -> Settlement {
    Settlement::of(admin::verdict(outcome))
}

impl SettingsWindow {
    fn remote(&self, route: &DirectRouteId) -> Option<&RouteAdmin> {
        self.remote_admin.routes.get(route)
    }

    fn remote_mut(&mut self, route: DirectRouteId) -> &mut RouteAdmin {
        self.remote_admin.routes.entry(route).or_default()
    }

    /// The lane steps for `route`'s current connection, when the desktop reaches paired
    /// servers' lanes and holds a connection to this one.
    fn remote_control(
        &self,
        route: &DirectRouteId,
        cx: &App,
    ) -> Option<(Arc<dyn RemoteServerControl>, ServerConnection)> {
        let source = installed(cx)?;
        let server = self.direct_routes.as_ref()?.connections.server(route)?;
        Some((source(server.clone()), server))
    }

    /// Whether `route` can take a lane step now: SpaceTerm holds a connection to it that is
    /// connected, or that the server refused only for want of a protocol in common, which the
    /// lane still speaks.
    pub(super) fn remote_ready(&self, route: &DirectRouteId, cx: &App) -> bool {
        let status = self
            .saved_direct_servers()
            .into_iter()
            .find(|(saved, _, _)| saved == route)
            .map(|(_, status, _)| status);
        let reachable = matches!(
            status,
            Some(
                DirectRouteStatus::Connected
                    | DirectRouteStatus::Refused(RefusalReason::Incompatible)
            )
        );
        reachable
            && self.remote(route).is_none_or(|admin| admin.work.is_none())
            && installed(cx).is_some()
            && self
                .direct_routes
                .as_ref()
                .is_some_and(|rows| rows.connections.server(route).is_some())
    }

    /// The step underway on `route`, if any.
    pub(super) fn remote_work(&self, route: &DirectRouteId) -> Option<&Work> {
        self.remote(route)?.work.as_ref()
    }

    /// `route`'s lane work, with a reading only while SpaceTerm's connection is still the link
    /// it was read over. Once the connection is replaced or reconnects, the facts it observes
    /// take precedence.
    fn current_reading(&self, route: &DirectRouteId) -> Option<&RouteAdmin> {
        let admin = self.remote(route)?;
        let (read_over, link) = admin.read_over.as_ref()?;
        let current = self.direct_routes.as_ref()?.connections.server(route)?;
        (read_over.same_handle(&current) && *link == current.link_generation()).then_some(admin)
    }

    /// The server's own version, as its last lane read over the current link reported it.
    pub(super) fn remote_version(&self, route: &DirectRouteId) -> Option<SharedString> {
        let status = self.current_reading(route)?.status.as_ref()?;
        Some(status.runtime.version.clone().into())
    }

    /// The newer release the server's last update check over the current link found, if any.
    pub(super) fn remote_available(&self, route: &DirectRouteId) -> Option<SharedString> {
        let check = self.current_reading(route)?.check.as_ref()?;
        let latest = check.latest.as_ref()?;
        (latest.sha256 != check.approved.sha256).then(|| latest.version.clone().into())
    }

    /// Whether the last change SpaceTerm made to `route` stopped it, or closed the lane while
    /// stopping. A Stop whose outcome is merely unknown doesn't count.
    pub(super) fn remote_stopped(&self, route: &DirectRouteId) -> bool {
        matches!(
            self.remote(route).and_then(|admin| admin.ended.as_ref()),
            Some(Ended::Changed(
                Act::Stop,
                Settlement::Done | Settlement::Shutdown
            ))
        )
    }

    /// The verb of a change underway on `route`, which the Server row names under its label.
    pub(super) fn remote_operation(&self, route: &DirectRouteId) -> Option<&'static str> {
        match self.remote_work(route)? {
            Work::Performing(Act::Stop) => Some("Stopping\u{2026}"),
            Work::Performing(Act::Restart) => Some("Restarting\u{2026}"),
            Work::Performing(Act::Update(_)) => Some("Updating\u{2026}"),
            Work::Performing(Act::Return(_)) => Some("Restoring\u{2026}"),
            Work::Preparing(Change::Update) => Some("Preparing\u{2026}"),
            Work::Reading | Work::Preparing(_) | Work::Confirming => None,
        }
    }

    /// Whether a read or a short preview of `route` is underway, which the state shows as
    /// Checking.
    pub(super) fn remote_reading(&self, route: &DirectRouteId) -> bool {
        matches!(
            self.remote_work(route),
            Some(Work::Reading | Work::Preparing(Change::Stop | Change::Restart))
        )
    }

    /// The change underway on `route`, as the Server row names it once it runs long.
    pub(super) fn remote_progress(&self, route: &DirectRouteId) -> Option<SharedString> {
        let named = |version: &Option<SharedString>, named: &str, unnamed: &str| {
            version.as_ref().map_or_else(
                || unnamed.to_owned(),
                |version| format!("{named} {version}\u{2026}"),
            )
        };
        let text = match self.remote_work(route)? {
            Work::Performing(Act::Stop) => "Stopping the server\u{2026}".to_owned(),
            Work::Performing(Act::Restart) => "Restarting the server\u{2026}".to_owned(),
            Work::Performing(Act::Update(to)) => {
                named(to, "Updating to", "Updating the server\u{2026}")
            }
            Work::Performing(Act::Return(to)) => {
                named(to, "Restoring", "Restoring the previous version\u{2026}")
            }
            Work::Preparing(Change::Update) => named(
                &self.remote_available(route),
                "Preparing the update to",
                "Preparing the update\u{2026}",
            ),
            Work::Reading | Work::Preparing(_) | Work::Confirming => return None,
        };
        Some(text.into())
    }

    /// What `route`'s card explains about its last lane step, if anything.
    pub(super) fn remote_message(
        &self,
        route: &DirectRouteId,
        name: &str,
    ) -> Option<(SharedString, CaptionTone)> {
        let admin = self.remote(route)?;
        if let Some(ended) = &admin.ended {
            let (text, tone) = ended_message(name, ended);
            return Some((text.into(), tone));
        }
        let (act, settlement) = admin.earlier.as_ref()?;
        let (text, tone) = earlier_message(name, act, settlement);
        Some((text.into(), tone))
    }

    /// Follows the registry's connection states. A server SpaceTerm stopped that connects again
    /// was started on its own computer, so the stop no longer needs explaining.
    pub(super) fn follow_remote_routes(&mut self) {
        let statuses: HashMap<DirectRouteId, DirectRouteStatus> = self
            .saved_direct_servers()
            .into_iter()
            .map(|(route, status, _)| (route, status))
            .collect();
        for (route, admin) in &mut self.remote_admin.routes {
            let stopped = matches!(
                admin.ended,
                Some(Ended::Changed(
                    Act::Stop,
                    Settlement::Done | Settlement::Shutdown
                ))
            );
            if !stopped {
                continue;
            }
            match statuses.get(route) {
                Some(DirectRouteStatus::Connected) if admin.left => {
                    admin.ended = None;
                    admin.left = false;
                }
                Some(DirectRouteStatus::Connected) => {}
                _ => admin.left = true,
            }
        }
    }

    /// Reads `route`'s Status and update check over its lane, as Check Now asks.
    pub(super) fn read_remote(&mut self, route: DirectRouteId, cx: &mut Context<Self>) {
        if !self.remote_ready(&route, cx) {
            return;
        }
        let Some((control, server)) = self.remote_control(&route, cx) else {
            return;
        };
        // Taken before the read, so a link that changes while it runs leaves it stale.
        let read_over = (server.clone(), server.link_generation());
        let task = cx.spawn(async move |settings, cx| {
            let read = cx.background_spawn(async move { control.read() }).await;
            let _ = settings.update(cx, |settings, cx| {
                let admin = settings.remote_mut(route);
                admin.work = None;
                match read {
                    Ok(reading) => {
                        admin.status = Some(reading.status);
                        admin.check = Some(reading.check);
                        admin.read_over = Some(read_over);
                        admin.earlier = earlier(&reading.settled);
                    }
                    Err(failure) => admin.ended = Some(Ended::Refused(failure)),
                }
                cx.notify();
            });
        });
        let admin = self.remote_mut(route);
        admin.work = Some(Work::Reading);
        admin.ended = None;
        admin.earlier = None;
        admin._task = Some(task);
        cx.notify();
    }

    /// Asks `route`'s server to preview `change`, then asks the person to confirm exactly what
    /// the server previewed, in the Settings window `window`.
    pub(super) fn request_remote_change(
        &mut self,
        route: DirectRouteId,
        change: Change,
        window: AnyWindowHandle,
        cx: &mut Context<Self>,
    ) {
        if !self.remote_ready(&route, cx) {
            return;
        }
        let Some((control, _)) = self.remote_control(&route, cx) else {
            return;
        };
        let task = cx.spawn(async move |settings, cx| {
            let prepared = cx
                .background_spawn(async move { control.prepare(change) })
                .await;
            let _ = settings.update(cx, |settings, cx| {
                let admin = settings.remote_mut(route);
                match prepared {
                    Ok(prepared) => {
                        admin.earlier = earlier(&prepared.settled);
                        admin.work = Some(Work::Confirming);
                        settings.confirm_remote(route, prepared.preview, window, cx);
                    }
                    Err(failure) => {
                        admin.work = None;
                        admin.ended = Some(Ended::Refused(failure));
                    }
                }
                cx.notify();
            });
        });
        let admin = self.remote_mut(route);
        admin.work = Some(Work::Preparing(change));
        admin.ended = None;
        admin.earlier = None;
        admin._task = Some(task);
        cx.notify();
    }

    /// Asks before the previewed change. Cancel is the default, so Return and Escape leave the
    /// server as it is; the unused preview expires on the server.
    fn confirm_remote(
        &mut self,
        route: DirectRouteId,
        preview: Preview,
        window: AnyWindowHandle,
        cx: &mut Context<Self>,
    ) {
        let name = self.server_name(&super::server_list::ServerChoice::Direct(route));
        let confirmation = confirmation(&name, &preview);
        let owner = cx.weak_entity();
        let entity = cx.entity();
        // The preview arrived outside any window update, so the alert is presented through the
        // Settings window it belongs to.
        cx.defer(move |cx| {
            let presented = window.update(cx, |_, window, cx| {
                entity.update(cx, |_, cx| {
                    let alert = Alert::new(
                        ModalId::new("settings-remote-server-change"),
                        confirmation.title,
                        confirmation.message.clone(),
                        confirmation.detail.clone(),
                        vec![
                            ModalAction::new(
                                true,
                                confirmation.confirm.clone(),
                                ModalActionRole::Affirmative,
                                CONFIRM_SELECTOR,
                            )
                            .with_intent(ModalActionIntent::Destructive)
                            .with_emphasis(ModalActionEmphasis::Prominent),
                            ModalAction::new(
                                false,
                                "Cancel",
                                ModalActionRole::Cancel,
                                CANCEL_SELECTOR,
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
                            settings.remote_mut(route).work = None;
                            if confirmed {
                                settings.perform_remote(route, preview, cx);
                            }
                            cx.notify();
                        });
                    });
                    if result.is_err() {
                        eprintln!("failed to present the paired server change confirmation");
                    }
                    result.is_ok()
                })
            });
            // An alert that could not be presented asks nothing, so the card is free again.
            if !presented.unwrap_or(false) {
                entity.update(cx, |settings, cx| {
                    settings.remote_mut(route).work = None;
                    cx.notify();
                });
            }
        });
    }

    /// Executes the change the person confirmed and follows it until it settles. The server
    /// refuses it if what its preview observed has changed.
    fn perform_remote(&mut self, route: DirectRouteId, preview: Preview, cx: &mut Context<Self>) {
        let Some((control, _)) = self.remote_control(&route, cx) else {
            let admin = self.remote_mut(route);
            admin.ended = Some(Ended::Refused(RemoteFailure::Unreachable));
            return;
        };
        let act = Act::of(&preview.action, preview.effects.to.as_ref());
        let performing = act.clone();
        let task = cx.spawn(async move |settings, cx| {
            let performed = cx
                .background_spawn(async move { control.perform(preview) })
                .await;
            let _ = settings.update(cx, |settings, cx| {
                let admin = settings.remote_mut(route);
                admin.work = None;
                match performed {
                    Ok(performed) => {
                        admin.earlier = earlier(&performed.settled);
                        admin.ended = Some(Ended::Changed(act, settlement(&performed.outcome)));
                        admin.left = false;
                        // The server changed, so what the last read said no longer holds.
                        admin.status = None;
                        admin.check = None;
                        admin.read_over = None;
                    }
                    Err(failure) => admin.ended = Some(Ended::Refused(failure)),
                }
                settings.follow_remote_routes();
                cx.notify();
            });
        });
        let admin = self.remote_mut(route);
        admin.work = Some(Work::Performing(performing));
        admin.ended = None;
        admin._task = Some(task);
        cx.notify();
    }
}

#[cfg(test)]
pub(super) mod testing {
    use std::sync::Mutex;

    use super::*;

    /// Lane steps whose answers a test sets and whose requests it records.
    pub(in crate::ui::settings_window) struct ScriptedRemote {
        pub(in crate::ui::settings_window) read: Mutex<Result<Reading, RemoteFailure>>,
        pub(in crate::ui::settings_window) prepare: Mutex<Result<Prepared, RemoteFailure>>,
        pub(in crate::ui::settings_window) perform: Mutex<Result<Performed, RemoteFailure>>,
        pub(in crate::ui::settings_window) prepared: Mutex<Vec<Change>>,
        pub(in crate::ui::settings_window) performed: Mutex<Vec<Preview>>,
    }

    impl ScriptedRemote {
        pub(in crate::ui::settings_window) fn new() -> Arc<Self> {
            Arc::new(Self {
                read: Mutex::new(Err(RemoteFailure::Unreachable)),
                prepare: Mutex::new(Err(RemoteFailure::Unreachable)),
                perform: Mutex::new(Err(RemoteFailure::Unreachable)),
                prepared: Mutex::default(),
                performed: Mutex::default(),
            })
        }

        /// Makes this the desktop's lane steps for every paired server.
        pub(in crate::ui::settings_window) fn install(self: &Arc<Self>, cx: &mut App) {
            let scripted = Arc::clone(self);
            install(
                Arc::new(move |_| Arc::clone(&scripted) as Arc<dyn RemoteServerControl>),
                cx,
            );
        }
    }

    impl RemoteServerControl for ScriptedRemote {
        fn read(&self) -> Result<Reading, RemoteFailure> {
            self.read.lock().unwrap().clone()
        }

        fn prepare(&self, change: Change) -> Result<Prepared, RemoteFailure> {
            self.prepared.lock().unwrap().push(change);
            self.prepare.lock().unwrap().clone()
        }

        fn perform(&self, preview: Preview) -> Result<Performed, RemoteFailure> {
            self.performed.lock().unwrap().push(preview);
            self.perform.lock().unwrap().clone()
        }
    }
}

#[cfg(test)]
#[path = "remote_admin_tests.rs"]
mod tests;
