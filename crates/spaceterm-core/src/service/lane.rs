//! The service lane: bounded JSON administration with a protocol version of its own, which
//! changes far less often than the work protocol's, so Status, Stop, Restart and Update keep
//! working when a client and its Core share no work major.
//!
//! A client selects it with [`crate::protocol::Lane::Service`] in the admission preamble, which
//! negotiates the lane's `major.minor` ([`crate::protocol::version::SERVICE`]). Each frame is
//! length-prefixed JSON of at most [`MAX_FRAME`] bytes. A minor adds only a request or reply
//! variant ([`LaneRequest::since`]); adding, removing or renaming a field needs a new major.
//!
//! Installing never ends anything, so it runs in one step: `StageFetch` installs the release the
//! Core's own feed names, and `StageUpload` with `UploadChunk` installs a pushed archive. Both
//! answer `Installed`; [`crate::update::sources`] describes the sources.
//!
//! Stop, Restart, Uninstall and Network Access changes run in two steps. `Preview` observes the service, the live
//! Session set and the candidate, and returns a token bound to that observation. `Execute`
//! recomputes the observation under the hierarchy lock, sets the membership fence so no new
//! Session starts, admits the operation in the ledger and launches the helper that performs it.
//! The client later reads the outcome with `Resolve` and deletes it with `Acknowledge`.
//! [`network`] describes how a Network Access change differs.
use super::operations::{
    Action, Actor, Admission, Evidence, HELPER_LOCK, Ledger, OperationId, Record, State,
};
use super::tokens::Tokens;
use super::updates::Updates;
use super::{RetainedInstallation, Service, runtime};
use crate::CoreError;
use crate::diagnostics::TerminalCounterSnapshot;
use crate::hierarchy::Id;
use crate::permissions::{
    PermissionError, PermissionsReport, RequestOutcome, SetupReadiness, SystemPermission,
    SystemPermissions,
};
use crate::protocol::Preamble;
use crate::protocol::capability::{Capabilities, Capability, Unavailable};
use crate::storage::Directory;
use crate::update::facts::{UpdateFacts, Version};
use crate::update::source::FactsSource;
use crate::update::sources::{FeedCheck, Pushes, Received, SourceError};
use base64::Engine;
use network::{
    Followed, LISTEN_LIMIT, LaneNetworkEffects, ListenerProgress, ListenerView, NetworkAccess,
    NetworkCaller, NetworkPlan,
};
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

/// A Permission Setup name: 32 lowercase hexadecimal digits.
fn setup_name_valid(setup: &str) -> bool {
    setup.len() == 32
        && setup
            .bytes()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
}

/// The contract code for an action only the server's own machine takes.
pub const LOCAL_ONLY: &str = "local_only";
/// The largest lane frame, in bytes.
pub const MAX_FRAME: usize = 128 * 1024;
/// The largest decoded `UploadChunk::data`, in bytes.
pub const MAX_CHUNK: usize = 64 * 1024;
/// How long the lane waits for the next request before it closes the connection.
const REQUEST_LIMIT: Duration = Duration::from_secs(30);
/// How long `WaitFacts` waits for a change before it answers with the unchanged facts.
pub const WAIT_FACTS_LIMIT: Duration = Duration::from_secs(20);
/// How often an admitted operation's fence is rechecked against the ledger.
const FENCE_INTERVAL: Duration = Duration::from_millis(500);

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "request", rename_all = "snake_case", deny_unknown_fields)]
pub enum LaneRequest {
    Status,
    /// Reads the Core's update facts. A struct variant, so unknown fields are refused.
    Facts {},
    /// Answers with the facts once the Core's `boot` or facts `revision` differs from these, or
    /// after [`WAIT_FACTS_LIMIT`] with the unchanged facts. A client that shares no work major
    /// with the Core follows facts this way.
    WaitFacts {
        boot: String,
        revision: u64,
    },
    /// Checks the Core's own feed now, and installs the release it names when that is newer.
    /// Answers `Facts`. A Core that never fetches a feed answers `no_update_feed`.
    Check,
    /// Installs the release the Core's own feed names. Answers `Installed`.
    StageFetch,
    /// Begins a push of `length` archive bytes of `version`. The Core refuses a version it would
    /// not install before any byte arrives.
    StageUpload {
        version: String,
        length: u64,
        sha256: String,
        /// Base64.
        signature: String,
    },
    UploadChunk {
        upload: String,
        offset: u64,
        /// Base64, at most [`MAX_CHUNK`] bytes decoded.
        data: String,
    },
    Preview {
        action: LaneAction,
    },
    Execute {
        action: LaneAction,
        token: String,
        operation: String,
    },
    Resolve {
        operation: String,
    },
    Acknowledge {
        operation: String,
    },
    /// Reads the server's System Permissions. This and the requests below answer only a caller
    /// on the server's own machine, and refuse a paired one with `local_only`.
    Permissions,
    /// Asks the system for one System Permission.
    RequestPermission {
        permission: SystemPermission,
    },
    /// Prepares a Permission Setup of one System Permission. `setup` is the client's random
    /// name for it, which [`LaneRequest::CancelPermissionSetup`] names.
    PreparePermissionSetup {
        permission: SystemPermission,
        setup: String,
    },
    /// Cancels a Permission Setup, over another connection while its preparation runs. The
    /// preparation then takes no further effect.
    CancelPermissionSetup {
        setup: String,
    },
    /// Forgets the system's decision for one System Permission and this server alone.
    ResetPermission {
        permission: SystemPermission,
    },
}

impl LaneRequest {
    /// The first service lane version that has this request. A client never sends a request
    /// that its admission's selected version lacks, and the lane refuses one as `unsupported`.
    /// A minor adds a request by appending a variant above and naming its minor here.
    pub const fn since(&self) -> crate::protocol::version::ProtocolVersion {
        crate::protocol::version::ProtocolVersion { major: 1, minor: 0 }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum LaneAction {
    /// Only the Core's own machine stops it. A paired caller gets `local_only`, because a
    /// stopped Core can't be started remotely.
    Stop,
    Restart,
    Network { access: NetworkAccess },
    /// Remove Machine: stops SpaceTerm Core, revokes every pairing and removes the service,
    /// the launcher and every installed version. Workspace data stays. Gated by
    /// [`Capability::Uninstall`], as Restart is by [`Capability::Restart`]. Once the helper
    /// starts, the Core answers no further request, so the caller treats `Accepted` as the
    /// outcome.
    Uninstall,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "reply", rename_all = "snake_case", deny_unknown_fields)]
pub enum LaneReply {
    Status(LaneStatus),
    /// The update facts of the Core run `boot`.
    Facts {
        boot: String,
        facts: Box<crate::update::facts::UpdateFacts>,
    },
    /// `StageFetch` or a push's last `UploadChunk` finished. `installed` is false when the feed
    /// named no newer version.
    Installed {
        installed: bool,
        facts: Box<UpdateFacts>,
    },
    UploadProgress {
        upload: String,
        next_offset: u64,
    },
    Preview {
        token: String,
        expires_at: String,
        effects: LaneEffects,
    },
    /// The preview of a Network Access change.
    NetworkPreview {
        token: String,
        expires_at: String,
        effects: LaneNetworkEffects,
    },
    Accepted {
        operation: String,
        /// The run of the server that admitted it. A Restart or Update has completed its
        /// replacement once a receipt carries a different boot.
        boot: String,
    },
    Receipt(LaneReceipt),
    Acknowledged,
    Permissions(PermissionsReport),
    PermissionRequested(RequestOutcome),
    PermissionSetup(SetupReadiness),
    PermissionSetupCancelled,
    PermissionReset,
    /// `code` is a code from the CLI output contract.
    Error {
        code: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LaneReceipt {
    pub operation: String,
    pub action: LaneAction,
    pub state: LaneOperationState,
    pub code: Option<String>,
    /// The run of the server answering now.
    pub boot: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LaneOperationState {
    Admitted,
    Launched,
    Running,
    Done,
    Failed,
    NotPerformed,
    Unresolved,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LaneVersion {
    pub version: String,
    pub commit: String,
    pub sha256: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LaneActivation {
    Settled,
    Unresolved,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LaneNetwork {
    pub enabled: bool,
    pub bind: Option<String>,
    pub listening: bool,
    pub addresses: Vec<std::net::IpAddr>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LaneDevice {
    pub id: String,
    pub name: String,
    pub fingerprint: String,
    pub last_connected_at: Option<String>,
}

/// Mirrors the `server.status` data of the CLI output contract. Held outcomes are the CLI's own
/// journals, so the lane leaves them out.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LaneStatus {
    pub running: bool,
    pub stopped: bool,
    pub installed: bool,
    pub automatic_start: bool,
    pub linger: Option<bool>,
    pub runtime: LaneVersion,
    /// The Core's update facts. None while no runtime is installed.
    pub update: Option<Box<UpdateFacts>>,
    pub activation: LaneActivation,
    pub sessions: u32,
    pub network: LaneNetwork,
    pub devices: Vec<LaneDevice>,
    pub lane_major: u16,
    /// The running server's Terminal Diagnostics counters. A stopped server has none.
    pub terminal_counters: Option<TerminalCounterSnapshot>,
}

/// Mirrors the confirmation `effects` of `stop` and `restart`. Stop and a Restart into the
/// running version leave the version fields empty; a Restart into the Pending Version names it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LaneEffects {
    pub ends_sessions: u32,
    pub sessions: Vec<String>,
    pub start_requires_machine: bool,
    pub from: Option<LaneVersion>,
    pub to: Option<LaneVersion>,
    /// The work protocol major a Restart into the Pending Version activates, so a client can
    /// warn when its own work major differs.
    pub to_major: Option<u16>,
}

/// What the lane needs from the running server around it.
pub trait LaneHost: Send + Sync {
    /// Runs `admit` under the hierarchy lock with the live Terminal Session IDs, and sets the
    /// membership fence if it succeeds. While the fence is set, a claim that would start a
    /// Session is refused with `ServiceBusy`.
    fn fence(&self, admit: &mut dyn FnMut(&[Id]) -> Result<(), CoreError>)
    -> Result<(), CoreError>;
    /// Clears the membership fence.
    fn release_fence(&self);
    /// The live Terminal Session IDs.
    fn sessions(&self) -> Vec<Id>;
    /// The running server's Terminal Diagnostics counters.
    fn terminal_counters(&self) -> TerminalCounterSnapshot;
    fn devices(&self) -> Result<Vec<LaneDevice>, CoreError>;
    fn network(&self) -> Result<LaneNetwork, CoreError>;
    /// Whether the server is stopping. The lane then closes its connections, which is how a
    /// client following an operation observes the shutdown.
    fn stopping(&self) -> bool {
        false
    }
    /// Plans `access` for `caller` under the direct-network lock, and applies it in the same
    /// hold when `decide` returns true. An applied change returns the listener it left, read in
    /// that hold. A server without Network Access is `Unsupported`.
    fn change_network(
        &self,
        _access: NetworkAccess,
        _caller: Option<&NetworkCaller>,
        _decide: &mut dyn FnMut(&NetworkPlan) -> Result<bool, CoreError>,
    ) -> Result<Option<ListenerView>, CoreError> {
        Err(CoreError::Unsupported)
    }
    /// The listener now.
    fn listener(&self) -> Result<ListenerView, CoreError> {
        Err(CoreError::Unsupported)
    }
}

/// What an `Execute` admitted, which the lane starts after it wrote `Accepted`.
enum Dispatch {
    /// The helper performs it.
    Helper(OperationId),
    /// The lane applies a Network Access change itself.
    Network {
        id: OperationId,
        access: NetworkAccess,
        caller: Option<NetworkCaller>,
        observation: [u8; 32],
    },
}

type Launcher = dyn Fn(&OperationId) -> Result<(), CoreError> + Send + Sync;

/// One running server's lane. A server start creates it after readiness and before it accepts
/// connections, so the ledger is settled first.
pub struct LaneCore {
    service: Service,
    core: Id,
    boot: Id,
    host: Arc<dyn LaneHost>,
    tokens: Tokens,
    /// The update engine, or why it could not open, such as a Core with no installed runtime.
    updates: Result<Updates, CoreError>,
    /// A Release Core's own feed. None for Preflight and Development.
    feed: Option<Arc<FeedCheck>>,
    pushes: Pushes,
    /// Names each served connection, so a push ends with the connection that sent it.
    connections: AtomicU64,
    launch: Box<Launcher>,
    permissions: Arc<dyn SystemPermissions>,
    /// What this Core's service lane can do. Tests withhold capabilities.
    capabilities: Capabilities,
    /// The Permission Setups whose preparations run, and their cancellations.
    setups: Mutex<setups::Setups>,
    /// Held for each whole `Execute`, and while the fence watcher releases the fence.
    executing: Mutex<()>,
    /// A fence watcher runs. The watcher clears it only under `executing`.
    watching: AtomicBool,
}

/// The capability a previewed or executed action needs, when it is not part of every service
/// lane.
const fn required(action: &LaneAction) -> Option<Capability> {
    match action {
        LaneAction::Restart => Some(Capability::Restart),
        LaneAction::Uninstall => Some(Capability::Uninstall),
        LaneAction::Stop | LaneAction::Network { .. } => None,
    }
}

/// Whether `request` previews or executes an action newer than the connection's `selected`
/// service lane version.
fn newer_than(request: &LaneRequest, selected: crate::protocol::version::ProtocolVersion) -> bool {
    matches!(
        request,
        LaneRequest::Preview { action } | LaneRequest::Execute { action, .. }
            if required(action).is_some_and(|capability| capability.since().1 > selected)
    )
}

/// The part of an observation that does not depend on the Session set, and the effects it
/// implies.
struct Prepared {
    service: [u8; 32],
    running: bool,
    effects: LaneEffects,
    candidate: Option<[u8; 32]>,
}

impl LaneCore {
    /// Starts the lane of a server that is ready to accept connections. `updates` is the
    /// server's one update engine, or why it could not open.
    pub(crate) fn start(
        paths: &crate::host::Paths,
        host: Arc<dyn LaneHost>,
        updates: Result<Updates, CoreError>,
    ) -> Result<Arc<Self>, CoreError> {
        let service = Service::for_paths(paths.clone())?;
        let owned = paths.identity.clone();
        let feed = FeedCheck::for_identity(&paths.identity, paths.data.clone());
        Self::new(
            service,
            updates,
            host,
            feed,
            Box::new(move |op| super::helper::launch(&owned, op)),
            crate::permissions::native(&paths.identity),
        )
    }

    /// This Core's own feed check, which the server runs once it is ready. None for Preflight
    /// and Development.
    pub(crate) fn feed_check(&self) -> Option<Arc<FeedCheck>> {
        self.feed.clone()
    }

    fn new(
        service: Service,
        updates: Result<Updates, CoreError>,
        host: Arc<dyn LaneHost>,
        feed: Option<FeedCheck>,
        launch: Box<Launcher>,
        permissions: Arc<dyn SystemPermissions>,
    ) -> Result<Arc<Self>, CoreError> {
        let core = service.passive_machine_id()?.ok_or(CoreError::NotSetUp)?;
        let lane = Self {
            updates,
            pushes: Pushes::new(service.paths.data.clone()),
            service,
            core,
            boot: crate::protocol::random()?,
            host,
            tokens: Tokens::default(),
            feed: feed.map(Arc::new),
            connections: AtomicU64::new(0),
            launch,
            permissions,
            capabilities: Capabilities::new(
                Err(Unavailable::NotConnected),
                Ok(crate::protocol::version::SERVICE),
            ),
            setups: Mutex::default(),
            executing: Mutex::new(()),
            watching: AtomicBool::new(false),
        };
        lane.ledger()?
            .recover_at_start(&lane.boot, &lane.evidence()?)?;
        Ok(Arc::new(lane))
    }

    /// Answers lane requests on an admitted service-lane connection until the peer closes it,
    /// stays silent past the request limit, the server stops, or `authorized` fails.
    /// `authorized` runs before every read and again before every answer, so a revoked pairing
    /// ends the lane without another answer. A host that stops severs the transport, which ends
    /// a read already waiting. A newly admitted operation starts only after its `Accepted`
    /// reply was written. `route` is the server address a paired connection reached.
    /// Serves one admitted lane connection. A request that its selected version lacks is
    /// answered `unsupported`.
    pub(crate) fn serve(
        self: &Arc<Self>,
        stream: &mut dyn Preamble,
        admitted: crate::protocol::AdmittedLane,
        route: Option<std::net::SocketAddr>,
        authorized: &dyn Fn() -> Result<(), CoreError>,
    ) -> Result<(), CoreError> {
        let crate::protocol::AdmittedLane { actor, selected } = admitted;
        let connection = self.connections.fetch_add(1, Ordering::Relaxed) + 1;
        let served = self.serve_connection(stream, actor, selected, route, authorized, connection);
        self.pushes.closed(connection);
        served
    }

    fn serve_connection(
        self: &Arc<Self>,
        stream: &mut dyn Preamble,
        actor: Actor,
        selected: crate::protocol::version::ProtocolVersion,
        route: Option<std::net::SocketAddr>,
        authorized: &dyn Fn() -> Result<(), CoreError>,
        connection: u64,
    ) -> Result<(), CoreError> {
        loop {
            // A stopping server must not keep a polling client's connection open: its
            // shutdown waits for every connection, and the client waits for this one to close.
            if self.host.stopping() {
                return Ok(());
            }
            authorized()?;
            let Ok(bytes) = stream.receive(MAX_FRAME, Instant::now() + REQUEST_LIMIT) else {
                return Ok(());
            };
            if self.host.stopping() {
                return Ok(());
            }
            authorized()?;
            let caller = NetworkCaller::of(&actor, route);
            let (reply, launch) = match serde_json::from_slice::<LaneRequest>(&bytes) {
                Ok(request) if request.since() > selected || newer_than(&request, selected) => {
                    (error(&CoreError::Unsupported), None)
                }
                Ok(request) => self.respond(request, &actor, caller, connection),
                Err(_) => (error(&CoreError::Protocol), None),
            };
            let sent = serde_json::to_vec(&reply)
                .map_err(|_| CoreError::Protocol)
                .and_then(|bytes| stream.send(&bytes, Instant::now() + REQUEST_LIMIT));
            // The record is durable, so it starts even when its reply was lost. The client
            // resolves the same operation ID after a lost reply.
            if let Some(dispatch) = launch {
                self.dispatch(dispatch);
            }
            sent?;
        }
    }

    /// Answers one request from a caller with no paired connection, and starts what it
    /// admitted.
    pub fn answer(self: &Arc<Self>, request: LaneRequest, actor: &Actor) -> LaneReply {
        let (reply, launch) = self.respond(request, actor, NetworkCaller::of(actor, None), 0);
        if let Some(dispatch) = launch {
            self.dispatch(dispatch);
        }
        reply
    }

    /// Answers one request. An `Execute` that admitted a new record returns what the caller
    /// starts after it wrote the reply.
    fn respond(
        self: &Arc<Self>,
        request: LaneRequest,
        actor: &Actor,
        caller: Option<NetworkCaller>,
        connection: u64,
    ) -> (LaneReply, Option<Dispatch>) {
        // An action this Core's service lane does not have is refused before its preview or
        // admission, from the same table clients read.
        if let LaneRequest::Preview { action } | LaneRequest::Execute { action, .. } = &request
            && let Some(capability) = required(action)
            && self.capabilities.check(capability).is_err()
        {
            return (error(&CoreError::Unsupported), None);
        }
        // Stop runs only on the Machine itself: nothing could start SpaceTerm Core again from a
        // paired device.
        if actor.pairing.is_some()
            && matches!(
                &request,
                LaneRequest::Preview {
                    action: LaneAction::Stop
                } | LaneRequest::Execute {
                    action: LaneAction::Stop,
                    ..
                }
            )
        {
            return (
                LaneReply::Error {
                    code: LOCAL_ONLY.to_owned(),
                },
                None,
            );
        }
        let result = match request {
            LaneRequest::Execute {
                action: LaneAction::Network { access },
                token,
                operation,
            } => {
                return self
                    .execute_network(access, &token, &operation, actor, caller)
                    .unwrap_or_else(|failure| (error(&failure), None));
            }
            LaneRequest::Execute {
                action,
                token,
                operation,
            } => {
                return self
                    .execute(&action, &token, &operation, actor)
                    .map(|(reply, id)| (reply, id.map(Dispatch::Helper)))
                    .unwrap_or_else(|failure| (error(&failure), None));
            }
            LaneRequest::Preview {
                action: LaneAction::Network { access },
            } => self.preview_network(access, actor, caller),
            LaneRequest::Status => self
                .admin()
                .status(Some((self.host.as_ref(), self.facts().ok())))
                .map(LaneReply::Status),
            LaneRequest::Facts {} => self.facts().map(|facts| self.facts_reply(facts)),
            LaneRequest::WaitFacts { boot, revision } => self.updates().map(|updates| {
                self.facts_reply(if boot == hex(&self.boot) {
                    updates.wait(revision, WAIT_FACTS_LIMIT)
                } else {
                    updates.facts()
                })
            }),
            LaneRequest::Check => return (sourced(self.check()), None),
            LaneRequest::StageFetch => return (sourced(self.stage_fetch()), None),
            LaneRequest::StageUpload {
                version,
                length,
                sha256,
                signature,
            } => {
                let reply = self.stage_upload(connection, &version, length, &sha256, &signature);
                return (sourced(reply), None);
            }
            LaneRequest::UploadChunk {
                upload,
                offset,
                data,
            } => return (sourced(self.upload_chunk(&upload, offset, &data)), None),
            LaneRequest::Preview { action } => self.preview(&action, actor),
            LaneRequest::Resolve { operation } => self.resolve(&operation),
            LaneRequest::Acknowledge { operation } => self.acknowledge(&operation, actor),
            LaneRequest::Permissions
            | LaneRequest::RequestPermission { .. }
            | LaneRequest::PreparePermissionSetup { .. }
            | LaneRequest::CancelPermissionSetup { .. }
            | LaneRequest::ResetPermission { .. } => {
                return (self.permission(request, actor), None);
            }
        };
        (result.unwrap_or_else(|failure| error(&failure)), None)
    }

    fn setups(&self) -> std::sync::MutexGuard<'_, setups::Setups> {
        self.setups.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Answers a System Permission request. Only the server's own machine sets up or changes
    /// these grants, so a paired caller is refused before the system is asked anything.
    fn permission(&self, request: LaneRequest, actor: &Actor) -> LaneReply {
        if actor.pairing.is_some() {
            return LaneReply::Error {
                code: LOCAL_ONLY.to_owned(),
            };
        }
        let permissions = self.permissions.as_ref();
        let reply = match request {
            LaneRequest::Permissions => permissions.read().map(LaneReply::Permissions),
            LaneRequest::RequestPermission { permission } => permissions
                .request(permission)
                .map(LaneReply::PermissionRequested),
            LaneRequest::PreparePermissionSetup { permission, setup } => {
                if !setup_name_valid(&setup) {
                    return error(&CoreError::Protocol);
                }
                if let Err(failure) = self.setups().begin(&setup, Instant::now()) {
                    return error(&failure);
                }
                // The admission ends however the preparation returns.
                struct Finish<'a>(&'a LaneCore, &'a str);
                impl Drop for Finish<'_> {
                    fn drop(&mut self) {
                        self.0.setups().finish(self.1);
                    }
                }
                let _finish = Finish(self, &setup);
                let cancelled = || self.setups().cancelled(&setup);
                permissions
                    .prepare_setup(permission, &cancelled)
                    .map(LaneReply::PermissionSetup)
            }
            LaneRequest::CancelPermissionSetup { setup } => {
                if !setup_name_valid(&setup) {
                    return error(&CoreError::Protocol);
                }
                if let Err(failure) = self.setups().cancel(&setup, Instant::now()) {
                    return error(&failure);
                }
                Ok(LaneReply::PermissionSetupCancelled)
            }
            LaneRequest::ResetPermission { permission } => permissions
                .reset(permission)
                .map(|()| LaneReply::PermissionReset),
            _ => Err(PermissionError::Unsupported),
        };
        reply.unwrap_or_else(|failure| LaneReply::Error {
            code: failure.code().to_owned(),
        })
    }

    fn updates(&self) -> Result<&Updates, CoreError> {
        self.updates.as_ref().map_err(|error| *error)
    }

    /// The Core's update facts now.
    fn facts(&self) -> Result<UpdateFacts, CoreError> {
        self.updates().map(FactsSource::facts)
    }

    fn facts_reply(&self, facts: UpdateFacts) -> LaneReply {
        LaneReply::Facts {
            boot: hex(&self.boot),
            facts: Box::new(facts),
        }
    }

    /// Checks the feed now, and answers the facts after. A check that found nothing newer
    /// succeeds.
    fn check(&self) -> Result<LaneReply, SourceError> {
        let feed = self.feed.as_ref().ok_or(SourceError::NoFeed)?;
        feed.run(self.updates()?)?;
        Ok(self.facts_reply(self.facts()?))
    }

    fn stage_fetch(&self) -> Result<LaneReply, SourceError> {
        let feed = self.feed.as_ref().ok_or(SourceError::NoFeed)?;
        let outcome = feed.run(self.updates()?)?;
        Ok(LaneReply::Installed {
            installed: matches!(outcome, crate::update::sources::Outcome::Installed(_)),
            facts: Box::new(self.facts()?),
        })
    }

    fn stage_upload(
        &self,
        connection: u64,
        version: &str,
        length: u64,
        sha256: &str,
        signature: &str,
    ) -> Result<LaneReply, SourceError> {
        let (Ok(version), Ok(signature)) = (
            Version::parse(version),
            base64::engine::general_purpose::STANDARD.decode(signature),
        ) else {
            return Err(SourceError::Protocol);
        };
        let upload = self.pushes.begin(
            self.updates()?,
            connection,
            version,
            length,
            sha256,
            &signature,
        )?;
        Ok(LaneReply::UploadProgress {
            upload,
            next_offset: 0,
        })
    }

    fn upload_chunk(&self, id: &str, offset: u64, data: &str) -> Result<LaneReply, SourceError> {
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(data)
            .map_err(|_| SourceError::Protocol)?;
        if bytes.len() > MAX_CHUNK {
            return Err(SourceError::Protocol);
        }
        Ok(match self.pushes.chunk(id, offset, &bytes)? {
            Received::Next(next_offset) => LaneReply::UploadProgress {
                upload: id.to_owned(),
                next_offset,
            },
            Received::Installed(_) => LaneReply::Installed {
                installed: true,
                facts: Box::new(self.facts()?),
            },
        })
    }

    /// Plans a Network Access change without applying it. A paired caller's change that keeps
    /// no connected device's route is refused before any token is issued.
    fn preview_network(
        &self,
        access: NetworkAccess,
        actor: &Actor,
        caller: Option<NetworkCaller>,
    ) -> Result<LaneReply, CoreError> {
        let key = action_key(&LaneAction::Network { access })?;
        let mut planned = None;
        self.host
            .change_network(access, caller.as_ref(), &mut |plan| {
                planned = Some(plan.clone());
                Ok(false)
            })?;
        let plan = planned.ok_or(CoreError::Protocol)?;
        if plan.refused_for(caller.as_ref()) {
            return Ok(refusal());
        }
        let observation = plan.observation(&self.core, &self.boot, &key);
        let issued = self
            .tokens
            .issue(&key, observation, *actor, Instant::now())?;
        Ok(LaneReply::NetworkPreview {
            token: issued.token,
            expires_at: timestamp(issued.expires_at),
            effects: plan.effects(),
        })
    }

    /// Admits a previewed Network Access change whose plan still matches its token. The
    /// change itself starts after `Accepted` was written, because it may end this connection.
    fn execute_network(
        self: &Arc<Self>,
        access: NetworkAccess,
        token: &str,
        operation: &str,
        actor: &Actor,
        caller: Option<NetworkCaller>,
    ) -> Result<(LaneReply, Option<Dispatch>), CoreError> {
        let id = OperationId::parse(operation).ok_or(CoreError::Protocol)?;
        let _executing = self.executing.lock().unwrap_or_else(|e| e.into_inner());
        let accepted = LaneReply::Accepted {
            operation: id.to_string(),
            boot: hex(&self.boot),
        };
        if let Some(record) = self.ledger()?.resolve(&id)? {
            return if record.actor == *actor {
                Ok((accepted, None))
            } else {
                Err(CoreError::Changed)
            };
        }
        let key = action_key(&LaneAction::Network { access })?;
        let expected = self.tokens.redeem(token, &key, actor, Instant::now())?;
        let record = Record {
            id,
            actor: *actor,
            core: self.core,
            boot: self.boot,
            action: Action::Network(access),
            observation_digest: expected,
            candidate: None,
            state: State::Admitted,
            updated_at: 0,
            orphaned: false,
        };
        let mut refused = false;
        let mut admitted = None;
        self.host
            .change_network(access, caller.as_ref(), &mut |plan| {
                if plan.observation(&self.core, &self.boot, &key) != expected {
                    return Err(CoreError::Changed);
                }
                if plan.refused_for(caller.as_ref()) {
                    refused = true;
                    return Ok(false);
                }
                admitted = Some(self.ledger()?.admit(record.clone())?);
                Ok(false)
            })?;
        if refused {
            return Ok((refusal(), None));
        }
        let dispatch = matches!(admitted, Some(Admission::New(_))).then_some(Dispatch::Network {
            id,
            access,
            caller,
            observation: expected,
        });
        Ok((accepted, dispatch))
    }

    /// Applies an admitted Network Access change if its plan still matches, and settles its
    /// record. The record is `Running` while the change applies, so a server that ends then
    /// leaves it unresolved rather than not performed.
    fn change_network(
        self: &Arc<Self>,
        id: &OperationId,
        access: NetworkAccess,
        caller: Option<NetworkCaller>,
        observation: [u8; 32],
    ) {
        let key = match action_key(&LaneAction::Network { access }) {
            Ok(key) => key,
            Err(_) => return,
        };
        let (mut started, mut settled, mut refused, mut enables) = (false, false, false, false);
        let result = self
            .host
            .change_network(access, caller.as_ref(), &mut |plan| {
                if plan.observation(&self.core, &self.boot, &key) != observation {
                    return Err(CoreError::Changed);
                }
                // The devices that keep a route are not in the digest. One that left since
                // admission can leave none, so the refusal is checked again in this hold.
                if plan.refused_for(caller.as_ref()) {
                    refused = true;
                    return Ok(false);
                }
                // A record already settled without this change is left alone.
                if self.ledger()?.claim(id)?.is_none() {
                    settled = true;
                    return Ok(false);
                }
                started = true;
                enables = plan.to.enabled;
                Ok(true)
            });
        if settled {
            return;
        }
        // A change that turns access on is done only once the listener it applied accepts. One
        // that waits for the listener it replaces to retire settles on a watcher, because this
        // thread may hold one of that listener's connections.
        if let Ok(applied) = &result
            && !refused
            && enables
        {
            match applied.map(|v| (v, v.progress)) {
                Some((_, ListenerProgress::Listening)) => {}
                Some((applied, ListenerProgress::Starting)) => {
                    return self.watch_listener(*id, applied);
                }
                Some((_, ListenerProgress::Stopped)) | None => {
                    let _ = self
                        .ledger()
                        .and_then(|mut v| v.mark(id, State::Unresolved));
                    return;
                }
            }
        }
        let result = result.map(drop);
        // Once the change started, `AddressUnavailable` and `PortInUse` mean the host could not
        // ready the new listener and changed nothing. Any other failure is a commit it could not
        // confirm.
        let state = match result {
            Ok(()) if refused => State::Failed {
                code: super::lane_codes::DISCONNECTS_EVERY_DEVICE.to_owned(),
            },
            Ok(()) => State::Done,
            Err(
                failure @ CoreError::Direct(
                    crate::direct::DirectError::AddressUnavailable
                    | crate::direct::DirectError::PortInUse,
                ),
            ) => State::Failed {
                code: super::lane_codes::code(&failure).to_owned(),
            },
            Err(_) if started => State::Unresolved,
            Err(failure) => State::Failed {
                code: super::lane_codes::code(&failure).to_owned(),
            },
        };
        // An outcome that could not be recorded stays `Running` for start-up recovery.
        let _ = self.ledger().and_then(|mut v| v.mark(id, state));
    }

    /// Settles an applied Network Access change once the listener it applied accepts: done when
    /// that listener listens within [`LISTEN_LIMIT`]. It is unresolved, because the policy was
    /// saved, when that listener stops, when a later policy change replaces it first, or when
    /// the limit passes. The watcher holds no strong reference, so it never keeps a stopping
    /// server alive; start-up recovery then settles the record.
    fn watch_listener(self: &Arc<Self>, id: OperationId, applied: ListenerView) {
        let lane = Arc::downgrade(self);
        let until = Instant::now() + LISTEN_LIMIT;
        let watched = std::thread::Builder::new()
            .name("spaceterm-network-listener".into())
            .spawn(move || {
                loop {
                    std::thread::sleep(Duration::from_millis(100));
                    let Some(lane) = lane.upgrade() else {
                        return;
                    };
                    if lane.host.stopping() {
                        return;
                    }
                    let state = match lane.host.listener().map(|now| applied.follow(&now)) {
                        Ok(Followed::Accepting) => State::Done,
                        Ok(Followed::Waiting) if Instant::now() < until => continue,
                        _ => State::Unresolved,
                    };
                    let _ = lane.ledger().and_then(|mut v| v.mark(&id, state));
                    return;
                }
            });
        if watched.is_err() {
            let _ = self
                .ledger()
                .and_then(|mut v| v.mark(&id, State::Unresolved));
        }
    }

    fn preview(&self, action: &LaneAction, actor: &Actor) -> Result<LaneReply, CoreError> {
        let prepared = self.admin().prepare(action, &self.core, &self.boot)?;
        let sessions = self.host.sessions();
        let observation = observation(&prepared, &sessions);
        let issued =
            self.tokens
                .issue(&action_key(action)?, observation, *actor, Instant::now())?;
        Ok(LaneReply::Preview {
            token: issued.token,
            expires_at: timestamp(issued.expires_at),
            effects: effects(&prepared, &sessions),
        })
    }

    fn execute(
        self: &Arc<Self>,
        action: &LaneAction,
        token: &str,
        operation: &str,
        actor: &Actor,
    ) -> Result<(LaneReply, Option<OperationId>), CoreError> {
        let id = OperationId::parse(operation).ok_or(CoreError::Protocol)?;
        // One Execute at a time, so a repeated Execute on another connection waits for the
        // first and finds its record instead of a spent token.
        let _executing = self.executing.lock().unwrap_or_else(|e| e.into_inner());
        let accepted = LaneReply::Accepted {
            operation: id.to_string(),
            boot: hex(&self.boot),
        };
        // A repeated Execute after a lost reply returns the same admission and dispatches
        // nothing, even though its token is already spent.
        if let Some(record) = self.ledger()?.resolve(&id)? {
            return if record.actor == *actor {
                Ok((accepted, None))
            } else {
                Err(CoreError::Changed)
            };
        }
        let key = action_key(action)?;
        let expected = self.tokens.redeem(token, &key, actor, Instant::now())?;
        let prepared = self.admin().prepare(action, &self.core, &self.boot)?;
        let record = Record {
            id,
            actor: *actor,
            core: self.core,
            boot: self.boot,
            action: match action {
                LaneAction::Stop => Action::Stop,
                LaneAction::Restart if prepared.candidate.is_some() => Action::Update,
                LaneAction::Restart => Action::Restart,
                LaneAction::Uninstall => Action::Uninstall,
                LaneAction::Network { .. } => return Err(CoreError::Protocol),
            },
            observation_digest: expected,
            candidate: prepared.candidate,
            state: State::Admitted,
            updated_at: 0,
            orphaned: false,
        };
        let admit = || {
            let mut admitted = None;
            self.host.fence(&mut |sessions| {
                if observation(&prepared, sessions) != expected {
                    return Err(CoreError::Changed);
                }
                admitted = Some(self.ledger()?.admit(record.clone())?);
                Ok(())
            })?;
            Ok(admitted)
        };
        // The engine owns a confirmed Restart or Stop, so every client's facts show it.
        let change = match action {
            LaneAction::Stop => runtime::updates::Confirmed::Stop,
            _ => runtime::updates::Confirmed::Restart {
                candidate: prepared.candidate,
            },
        };
        let admitted = match self.updates() {
            Ok(updates) => updates.confirm(change, admit)?,
            Err(_) => admit()?,
        };
        let launch = matches!(admitted, Some(Admission::New(_))).then_some(id);
        Ok((accepted, launch))
    }

    /// Starts what `Execute` admitted.
    fn dispatch(self: &Arc<Self>, dispatch: Dispatch) {
        match dispatch {
            Dispatch::Helper(id) => self.launch_helper(&id),
            Dispatch::Network {
                id,
                access,
                caller,
                observation,
            } => self.change_network(&id, access, caller, observation),
        }
    }

    /// Launches the helper for a newly admitted record and watches the record until it
    /// settles, then clears the membership fence.
    fn launch_helper(self: &Arc<Self>, id: &OperationId) {
        let launched = (self.launch)(id);
        let mark = match launched {
            Ok(()) => State::Launched,
            Err(_) => State::NotPerformed(super::operations::NotPerformed::LaunchFailed),
        };
        // A failed write leaves the record unsettled. Settlement after the launch grace or at the
        // next server start resolves it, and the watcher below keeps the fence until then.
        let _ = self.ledger().and_then(|mut v| v.mark(id, mark));
        if self.watching.swap(true, Ordering::SeqCst) {
            return;
        }
        // The watcher must not keep the server alive: the lane holds the pane service, whose
        // shutdown the server waits for, while the helper that settles the record waits for
        // the server to exit. It holds a weak reference and ends with the server.
        let lane = Arc::downgrade(self);
        let watched = std::thread::Builder::new()
            .name("spaceterm-service-fence".into())
            .spawn(move || {
                loop {
                    std::thread::sleep(FENCE_INTERVAL);
                    let Some(lane) = lane.upgrade() else {
                        return;
                    };
                    if lane.host.stopping() || lane.release_if_settled() {
                        return;
                    }
                }
            });
        if watched.is_err() {
            self.watching.store(false, Ordering::SeqCst);
        }
    }

    /// Clears the membership fence and ends the watch once every record is terminal. It holds
    /// `executing`, so no Execute admits an operation and sets its fence between the check and
    /// the release. An operation admitted after the release finds no watcher and starts one.
    fn release_if_settled(&self) -> bool {
        let _executing = self.executing.lock().unwrap_or_else(|e| e.into_inner());
        if !self.settled().unwrap_or(false) {
            return false;
        }
        self.host.release_fence();
        self.watching.store(false, Ordering::SeqCst);
        if let Ok(updates) = self.updates() {
            updates.operations_settled();
        }
        true
    }

    /// Settles what can be settled and reports whether every record is terminal.
    fn settled(&self) -> Result<bool, CoreError> {
        let mut ledger = self.ledger()?;
        ledger.settle(&self.boot, &self.evidence()?, None)?;
        Ok(ledger.records()?.iter().all(|v| v.state.terminal()))
    }

    fn resolve(&self, operation: &str) -> Result<LaneReply, CoreError> {
        let id = OperationId::parse(operation).ok_or(CoreError::Protocol)?;
        let mut ledger = self.ledger()?;
        ledger.settle(&self.boot, &self.evidence()?, None)?;
        let Some(record) = ledger.resolve(&id)? else {
            return Ok(LaneReply::Error {
                code: "not_found".to_owned(),
            });
        };
        let (state, code) = match record.state {
            State::Admitted => (LaneOperationState::Admitted, None),
            State::Launched => (LaneOperationState::Launched, None),
            State::Running => (LaneOperationState::Running, None),
            State::Done => (LaneOperationState::Done, None),
            State::Failed { code } => (LaneOperationState::Failed, Some(code)),
            State::NotPerformed(_) => (
                LaneOperationState::NotPerformed,
                Some("operation_failed".to_owned()),
            ),
            State::Unresolved => (LaneOperationState::Unresolved, None),
        };
        Ok(LaneReply::Receipt(LaneReceipt {
            operation: id.to_string(),
            action: match record.action {
                Action::Stop => LaneAction::Stop,
                // A restart into the Pending Version is the Restart its client confirmed.
                Action::Restart | Action::Update => LaneAction::Restart,
                Action::Uninstall => LaneAction::Uninstall,
                Action::Network(access) => LaneAction::Network { access },
            },
            state,
            code,
            boot: hex(&self.boot),
        }))
    }

    fn acknowledge(&self, operation: &str, actor: &Actor) -> Result<LaneReply, CoreError> {
        let id = OperationId::parse(operation).ok_or(CoreError::Protocol)?;
        self.ledger()?.acknowledge(&id, actor)?;
        Ok(LaneReply::Acknowledged)
    }

    fn admin(&self) -> Admin<'_> {
        Admin {
            service: &self.service,
        }
    }

    fn ledger(&self) -> Result<Ledger, CoreError> {
        Ok(Ledger::in_directory(Directory::open(
            &self.service.paths.data,
            false,
        )?))
    }

    fn evidence(&self) -> Result<Journal, CoreError> {
        Ok(Journal {
            service: self.service.clone(),
            data: Directory::open(&self.service.paths.data, false)?,
        })
    }
}

/// Service administration that needs no running server: status and the observation an action
/// previews. The lane uses it while the server runs, and the CLI uses it on the server's own
/// machine while the server is stopped.
pub struct Admin<'a> {
    service: &'a Service,
}

impl Admin<'_> {
    /// The service's status, with what only a running server knows supplied by `live`: its host
    /// and its update facts. A stopped server reports its persisted update facts.
    pub fn status(
        &self,
        live: Option<(&dyn LaneHost, Option<UpdateFacts>)>,
    ) -> Result<LaneStatus, CoreError> {
        let (live, facts) = match live {
            Some((host, facts)) => (Some(host), facts),
            None => (None, Updates::stored(self.service).ok()),
        };
        let Some((_, state)) = self.service.read()? else {
            // Set up but never installed: nothing runs, and no runtime is approved.
            return Ok(LaneStatus {
                running: false,
                stopped: false,
                installed: false,
                automatic_start: false,
                linger: self.service.linger()?,
                runtime: LaneVersion {
                    version: String::new(),
                    commit: String::new(),
                    sha256: String::new(),
                },
                update: None,
                activation: LaneActivation::Settled,
                sessions: 0,
                network: offline_network(self.service)?,
                devices: offline_devices(self.service)?,
                lane_major: crate::protocol::version::SERVICE.major,
                terminal_counters: live.map(LaneHost::terminal_counters),
            });
        };
        let unresolved = state
            .activation
            .as_ref()
            .is_some_and(|operation| !operation.settled());
        let snapshot = self.service.snapshot(&state)?;
        Ok(LaneStatus {
            running: snapshot.running,
            stopped: snapshot.stopped,
            installed: snapshot.installed,
            automatic_start: snapshot.enabled,
            linger: self.service.linger()?,
            runtime: self.approved(&state)?,
            update: facts.map(Box::new),
            activation: if unresolved {
                LaneActivation::Unresolved
            } else {
                LaneActivation::Settled
            },
            sessions: live.map_or(0, |host| {
                u32::try_from(host.sessions().len()).unwrap_or(u32::MAX)
            }),
            network: match live {
                Some(host) => host.network()?,
                None => offline_network(self.service)?,
            },
            devices: match live {
                Some(host) => host.devices()?,
                None => offline_devices(self.service)?,
            },
            lane_major: crate::protocol::version::SERVICE.major,
            terminal_counters: live.map(LaneHost::terminal_counters),
        })
    }

    /// Observes the service for `action` without changing it.
    fn prepare(&self, action: &LaneAction, core: &Id, boot: &Id) -> Result<Prepared, CoreError> {
        let mut digest = ring::digest::Context::new(&ring::digest::SHA256);
        digest.update(b"spaceterm-service-lane-observation-1\0");
        digest.update(core);
        digest.update(boot);
        digest.update(&action_key(action)?);
        let mut effects = LaneEffects {
            ends_sessions: 0,
            sessions: Vec::new(),
            start_requires_machine: false,
            from: None,
            to: None,
            to_major: None,
        };
        // Restart moves into the Pending Version when one waits.
        let pending = match action {
            LaneAction::Restart => {
                let (data, _) = self.service.read()?.ok_or(CoreError::NotInstalled)?;
                runtime::updates::pending_digest(&data)?
            }
            _ => None,
        };
        let (running, candidate) = match (action, pending) {
            (LaneAction::Restart, Some(installation)) => {
                let candidate = self.installation(&installation)?;
                let approval = self
                    .service
                    .prepare_activation(&candidate)?
                    .ok_or(CoreError::CandidateMissing)?;
                digest.update(approval.confirmation_token()?.as_bytes());
                effects.from = Some(version_of(approval.previous()));
                effects.to = Some(version_of(approval.candidate()));
                effects.to_major = protocol_major(approval.candidate());
                (approval.running(), Some(digest_bytes(&installation)?))
            }
            // Uninstall begins with a Stop, so it observes what a Stop observes.
            (LaneAction::Stop | LaneAction::Uninstall, _) | (LaneAction::Restart, None) => {
                let control = if *action == LaneAction::Restart {
                    super::control::Action::Restart
                } else {
                    super::control::Action::Stop
                };
                let confirmation =
                    super::control::Controller::new(self.service.clone()).prepare(control)?;
                let (_, state) = self.service.read()?.ok_or(CoreError::NotInstalled)?;
                digest.update(&serde_json::to_vec(&state).map_err(|_| CoreError::Protocol)?);
                effects.start_requires_machine = *action == LaneAction::Stop;
                (confirmation.running(), None)
            }
            (LaneAction::Network { .. }, _) => return Err(CoreError::Unsupported),
        };
        Ok(Prepared {
            service: digest
                .finish()
                .as_ref()
                .try_into()
                .map_err(|_| CoreError::Protocol)?,
            running,
            effects,
            candidate,
        })
    }

    fn approved(&self, state: &super::State) -> Result<LaneVersion, CoreError> {
        Ok(version_of(&self.service.describe_approved(state)?))
    }

    /// A verified retained installation by digest.
    pub fn installation(&self, digest: &str) -> Result<RetainedInstallation, CoreError> {
        if !runtime::digest_valid(digest) {
            return Err(CoreError::CandidateMissing);
        }
        let directory = self.service.paths.data.join("installations").join(digest);
        if !directory.is_dir() {
            return Err(CoreError::CandidateMissing);
        }
        let (version, commit) = runtime::describe(&directory, &self.service.identity)?;
        Ok(RetainedInstallation {
            directory,
            version,
            commit,
        })
    }
}

/// [`Admin`] with its own service, for a stopped server on its own machine. It also installs
/// new versions while the server is stopped.
pub struct Offline {
    service: Service,
}

impl Offline {
    pub fn new(identity: &str) -> Result<Self, CoreError> {
        Ok(Self {
            service: Service::for_identity(identity)?,
        })
    }

    pub fn admin(&self) -> Admin<'_> {
        Admin {
            service: &self.service,
        }
    }

    /// Installs the release this Core's own feed names. Returns whether a version was installed
    /// and the facts after.
    pub fn install_from_feed(&self) -> Result<(bool, UpdateFacts), SourceError> {
        let feed = FeedCheck::for_identity(&self.service.identity, self.service.paths.data.clone())
            .ok_or(SourceError::NoFeed)?;
        let updates = Updates::open(self.service.clone())?;
        let outcome = feed.run(&updates)?;
        Ok((
            matches!(outcome, crate::update::sources::Outcome::Installed(_)),
            updates.facts(),
        ))
    }

    /// The HTTP proxy this Core checks its feed through, when one is set.
    pub fn update_proxy(&self) -> Result<Option<crate::update::proxy::Proxy>, CoreError> {
        crate::update::proxy::read(&self.service.paths.data)
    }

    /// Sets the HTTP proxy this Core checks its feed through, or removes it with `None`.
    /// Returns whether the setting changed. The next check uses it.
    pub fn set_update_proxy(
        &self,
        proxy: Option<&crate::update::proxy::Proxy>,
    ) -> Result<bool, CoreError> {
        if self.update_proxy()?.as_ref() == proxy {
            return Ok(false);
        }
        crate::update::proxy::write(&self.service.paths.data, proxy)?;
        Ok(true)
    }

    /// Installs a signed local archive and its raw signature. Returns the facts after.
    pub fn install_archive(
        &self,
        archive: &std::path::Path,
        signature: &std::path::Path,
        sha256: &str,
    ) -> Result<UpdateFacts, SourceError> {
        let updates = Updates::open(self.service.clone())?;
        updates.install(super::updates::Archive {
            archive,
            signature,
            sha256,
            source: super::updates::Source::Local,
        })?;
        Ok(updates.facts())
    }
}

/// Network Access as a stopped server's own direct state records it.
fn offline_network(service: &Service) -> Result<LaneNetwork, CoreError> {
    let (policy, enabled, _) = crate::direct::observe(&service.paths)?;
    Ok(LaneNetwork {
        enabled,
        bind: policy.map(|policy| policy.bind.to_string()),
        listening: false,
        addresses: Vec::new(),
    })
}

/// Paired devices as a stopped server's own direct state records them.
fn offline_devices(service: &Service) -> Result<Vec<LaneDevice>, CoreError> {
    let (_, _, devices) = crate::direct::observe(&service.paths)?;
    Ok(devices.iter().map(device).collect())
}

/// A paired device as the lane reports it.
pub(crate) fn device(info: &crate::direct::DeviceInfo) -> LaneDevice {
    LaneDevice {
        id: hex(&info.pairing),
        name: info.name.as_str().to_owned(),
        fingerprint: info.fingerprint(),
        last_connected_at: info.last_connected_at_unix_seconds.map(unix_timestamp),
    }
}

impl runtime::updates::Restarter for LaneCore {
    /// Admits a restart the Core decided on, as Execute admits one a client confirmed, and
    /// launches its helper from this runtime.
    fn restart(
        self: Arc<Self>,
        candidate: Option<[u8; 32]>,
        silent: bool,
    ) -> Result<(), runtime::updates::RestartRefusal> {
        use runtime::updates::RestartRefusal;
        let id = OperationId::random().map_err(RestartRefusal::Core)?;
        let record = Record {
            id,
            actor: Actor::CORE,
            core: self.core,
            boot: self.boot,
            action: if candidate.is_some() {
                Action::Update
            } else {
                Action::Restart
            },
            observation_digest: [0; 32],
            candidate,
            state: State::Admitted,
            updated_at: 0,
            orphaned: false,
        };
        let admitted = {
            let _executing = self.executing.lock().unwrap_or_else(|e| e.into_inner());
            let mut open = false;
            let mut admitted = None;
            let fenced = self.host.fence(&mut |sessions| {
                if silent && !sessions.is_empty() {
                    open = true;
                    return Err(CoreError::Changed);
                }
                admitted = Some(self.ledger()?.admit(record.clone())?);
                Ok(())
            });
            match fenced {
                Ok(()) => admitted,
                Err(_) if open => return Err(RestartRefusal::SessionsOpen),
                Err(CoreError::ServiceBusy) => return Err(RestartRefusal::Busy),
                Err(error) => return Err(RestartRefusal::Core(error)),
            }
        };
        if matches!(admitted, Some(Admission::New(_))) {
            self.launch_helper(&id);
        }
        Ok(())
    }
}

/// Settlement evidence from this account's own files.
struct Journal {
    service: Service,
    data: Directory,
}

impl Evidence for Journal {
    fn helper_active(&self) -> Result<bool, CoreError> {
        Ok(self.data.try_lock(HELPER_LOCK)?.is_none())
    }

    fn activation(&self, record: &Record) -> Result<Option<State>, CoreError> {
        // An operation that holds the service lock may be the activation itself. Wait for it
        // to finish instead of reading its journal mid-step.
        let Some(_operation) = self.data.try_lock("service.lock")? else {
            return Err(CoreError::ServiceBusy);
        };
        let Some(status) = self.service.activation_status_locked(&self.data)? else {
            return Ok(None);
        };
        let digest = |installation: &RetainedInstallation| {
            installation
                .directory
                .file_name()
                .and_then(|v| v.to_str())
                .and_then(|v| digest_bytes(v).ok())
        };
        Ok(match record.action {
            Action::Update if digest(&status.candidate) == record.candidate => {
                Some(super::helper::activation_outcome(status.phase))
            }
            _ => None,
        })
    }
}

fn observation(prepared: &Prepared, sessions: &[Id]) -> [u8; 32] {
    let mut sorted = sessions.to_vec();
    sorted.sort_unstable();
    let mut digest = ring::digest::Context::new(&ring::digest::SHA256);
    digest.update(&prepared.service);
    if prepared.running {
        for session in &sorted {
            digest.update(session);
        }
    }
    let mut out = [0; 32];
    out.copy_from_slice(digest.finish().as_ref());
    out
}

fn effects(prepared: &Prepared, sessions: &[Id]) -> LaneEffects {
    let mut effects = prepared.effects.clone();
    if prepared.running {
        let mut sorted: Vec<String> = sessions.iter().map(|v| hex(v)).collect();
        sorted.sort_unstable();
        effects.ends_sessions = u32::try_from(sorted.len()).unwrap_or(u32::MAX);
        effects.sessions = sorted;
    }
    effects
}

fn action_key(action: &LaneAction) -> Result<Vec<u8>, CoreError> {
    serde_json::to_vec(action).map_err(|_| CoreError::Protocol)
}

fn version_of(installation: &RetainedInstallation) -> LaneVersion {
    LaneVersion {
        version: installation.version.clone(),
        commit: installation.commit.clone(),
        sha256: installation
            .directory
            .file_name()
            .and_then(|v| v.to_str())
            .unwrap_or_default()
            .to_owned(),
    }
}

/// The protocol major a verified installation's signed build information declares.
fn protocol_major(installation: &RetainedInstallation) -> Option<u16> {
    #[derive(Deserialize)]
    struct Build {
        protocols: crate::update::facts::Protocols,
    }
    let bytes = std::fs::read(installation.directory.join("build.json")).ok()?;
    serde_json::from_slice::<Build>(&bytes)
        .ok()
        .map(|v| v.protocols.work.major)
}

fn digest_bytes(text: &str) -> Result<[u8; 32], CoreError> {
    if !runtime::digest_valid(text) {
        return Err(CoreError::CandidateMissing);
    }
    let mut out = [0; 32];
    for (index, byte) in out.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&text[index * 2..index * 2 + 2], 16)
            .map_err(|_| CoreError::CandidateMissing)?;
    }
    Ok(out)
}

fn timestamp(at: SystemTime) -> String {
    unix_timestamp(
        at.duration_since(SystemTime::UNIX_EPOCH)
            .map_or(0, |v| v.as_secs()),
    )
}

/// RFC 3339 in UTC, to the second.
pub(crate) fn unix_timestamp(seconds: u64) -> String {
    jiff::Timestamp::from_second(i64::try_from(seconds).unwrap_or(i64::MAX))
        .map(|v| v.to_string())
        .unwrap_or_default()
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|v| format!("{v:02x}")).collect()
}

/// A paired caller's Network Access change that keeps no connected device's route.
fn refusal() -> LaneReply {
    LaneReply::Error {
        code: super::lane_codes::DISCONNECTS_EVERY_DEVICE.to_owned(),
    }
}

fn sourced(reply: Result<LaneReply, SourceError>) -> LaneReply {
    reply.unwrap_or_else(|failure| source_error(&failure))
}

fn source_error(failure: &SourceError) -> LaneReply {
    LaneReply::Error {
        code: failure.code().to_owned(),
    }
}

fn error(failure: &CoreError) -> LaneReply {
    LaneReply::Error {
        code: super::lane_codes::code(failure).to_owned(),
    }
}

#[cfg(feature = "client")]
pub mod admin;
#[cfg(feature = "client")]
pub mod client;
pub mod network;
mod setups;
#[cfg(test)]
pub(crate) mod tests;
