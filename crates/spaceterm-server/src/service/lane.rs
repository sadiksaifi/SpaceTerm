//! The service lane: bounded JSON administration that every protocol major reads the
//! same way, so Status, Stop, Restart and Update work across majors.
//!
//! A client selects it with [`crate::protocol::Lane::Service`] in the admission preamble. Each
//! frame is length-prefixed JSON of at most [`MAX_FRAME`] bytes. Fields may be added to these
//! messages. They are never removed or renamed.
//!
//! Stop, Restart, Update, Return and Network Access changes run in two steps. `Preview` observes the service, the live
//! Session set and the candidate, and returns a token bound to that observation. `Execute`
//! recomputes the observation under the hierarchy lock, sets the membership fence so no new
//! Session starts, admits the operation in the ledger and launches the helper that performs it.
//! The client later reads the outcome with `Resolve` and deletes it with `Acknowledge`.
//! [`network`] describes how a Network Access change differs.
use super::feed::{self, Source};
use super::operations::{
    Action, Actor, Admission, Evidence, HELPER_LOCK, Ledger, OperationId, Record, State,
};
use super::tokens::Tokens;
use super::{RetainedInstallation, Service, runtime};
use crate::ServerError;
use crate::hierarchy::Id;
use crate::permissions::{
    PermissionError, PermissionsReport, RequestOutcome, SetupReadiness, SystemPermission,
    SystemPermissions,
};
use crate::protocol::Preamble;
use crate::storage::Directory;
use base64::Engine;
use network::{
    Followed, LISTEN_LIMIT, LaneNetworkEffects, ListenerProgress, ListenerView, NetworkAccess,
    NetworkCaller, NetworkPlan,
};
use serde::{Deserialize, Serialize};
use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
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
/// How often an admitted operation's fence is rechecked against the ledger.
const FENCE_INTERVAL: Duration = Duration::from_millis(500);

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "request", rename_all = "snake_case", deny_unknown_fields)]
pub enum LaneRequest {
    Status,
    Check,
    StageFetch,
    StageUpload {
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

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum LaneAction {
    Stop,
    Restart,
    Update { installation: String },
    Return,
    Network { access: NetworkAccess },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "reply", rename_all = "snake_case")]
pub enum LaneReply {
    Status(LaneStatus),
    Check(LaneCheck),
    Staged {
        installation: String,
        version: String,
        commit: String,
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

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LaneStaged {
    pub version: String,
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
    pub staged: Vec<LaneStaged>,
    pub activation: LaneActivation,
    pub sessions: u32,
    pub network: LaneNetwork,
    pub devices: Vec<LaneDevice>,
    pub lane_major: u16,
}

/// Mirrors the `server.update --check` data of the CLI output contract.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LaneCheck {
    pub approved: LaneVersion,
    pub latest: Option<LaneVersion>,
    pub staged: Vec<LaneVersion>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LaneUpdateKind {
    Update,
    Return,
}

/// Mirrors the confirmation `effects` of `server.stop`, `server.restart` and `server.update`.
/// Stop and Restart leave the version fields empty.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LaneEffects {
    pub ends_sessions: u32,
    pub sessions: Vec<String>,
    pub start_requires_machine: bool,
    pub from: Option<LaneVersion>,
    pub to: Option<LaneVersion>,
    pub kind: Option<LaneUpdateKind>,
    /// The protocol major the update or return activates, so a client can warn when its own
    /// work major differs.
    pub to_major: Option<u16>,
}

/// The lane major this server speaks.
pub const LANE_MAJOR: u16 = 1;

/// What the lane needs from the running server around it.
pub trait LaneHost: Send + Sync {
    /// Runs `admit` under the hierarchy lock with the live Terminal Session IDs, and sets the
    /// membership fence if it succeeds. While the fence is set, a claim that would start a
    /// Session is refused with `ServiceBusy`.
    fn fence(
        &self,
        admit: &mut dyn FnMut(&[Id]) -> Result<(), ServerError>,
    ) -> Result<(), ServerError>;
    /// Clears the membership fence.
    fn release_fence(&self);
    /// The live Terminal Session IDs.
    fn sessions(&self) -> Vec<Id>;
    fn devices(&self) -> Result<Vec<LaneDevice>, ServerError>;
    fn network(&self) -> Result<LaneNetwork, ServerError>;
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
        _decide: &mut dyn FnMut(&NetworkPlan) -> Result<bool, ServerError>,
    ) -> Result<Option<ListenerView>, ServerError> {
        Err(ServerError::Unsupported)
    }
    /// The listener now.
    fn listener(&self) -> Result<ListenerView, ServerError> {
        Err(ServerError::Unsupported)
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

type Launcher = dyn Fn(&OperationId) -> Result<(), ServerError> + Send + Sync;

/// One running server's lane. A server start creates it after readiness and before it accepts
/// connections, so the ledger is settled first.
pub struct LaneServer {
    service: Service,
    server: Id,
    boot: Id,
    host: Arc<dyn LaneHost>,
    tokens: Tokens,
    feed: Arc<dyn Source>,
    launch: Box<Launcher>,
    permissions: Arc<dyn SystemPermissions>,
    /// The Permission Setups whose preparations run, and their cancellations.
    setups: Mutex<setups::Setups>,
    upload: Mutex<Option<Upload>>,
    /// Held for each whole `Execute`, and while the fence watcher releases the fence.
    executing: Mutex<()>,
    /// A fence watcher runs. The watcher clears it only under `executing`.
    watching: AtomicBool,
}

struct Upload {
    id: String,
    length: u64,
    sha256: String,
    next: u64,
    file: std::fs::File,
    archive: PathBuf,
    signature: PathBuf,
}

impl Drop for Upload {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.archive);
        let _ = std::fs::remove_file(&self.signature);
    }
}

/// The part of an observation that does not depend on the Session set, and the effects it
/// implies.
struct Prepared {
    service: [u8; 32],
    running: bool,
    effects: LaneEffects,
    candidate: Option<[u8; 32]>,
}

impl LaneServer {
    /// Starts the lane of a server that is ready to accept connections.
    pub(crate) fn start(
        paths: &crate::host::Paths,
        host: Arc<dyn LaneHost>,
    ) -> Result<Arc<Self>, ServerError> {
        let service = Service::for_paths(paths.clone())?;
        let owned = paths.identity.clone();
        Self::new(
            service,
            host,
            Arc::new(feed::Releases::new(&paths.identity)),
            Box::new(move |op| super::helper::launch(&owned, op)),
            crate::permissions::native(&paths.identity),
        )
    }

    fn new(
        service: Service,
        host: Arc<dyn LaneHost>,
        feed: Arc<dyn Source>,
        launch: Box<Launcher>,
        permissions: Arc<dyn SystemPermissions>,
    ) -> Result<Arc<Self>, ServerError> {
        let server = service.passive_server_id()?.ok_or(ServerError::NotSetUp)?;
        let lane = Self {
            service,
            server,
            boot: crate::protocol::random()?,
            host,
            tokens: Tokens::default(),
            feed,
            launch,
            permissions,
            setups: Mutex::default(),
            upload: Mutex::new(None),
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
    pub(crate) fn serve(
        self: &Arc<Self>,
        stream: &mut dyn Preamble,
        actor: Actor,
        route: Option<std::net::SocketAddr>,
        authorized: &dyn Fn() -> Result<(), ServerError>,
    ) -> Result<(), ServerError> {
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
                Ok(request) => self.respond(request, &actor, caller),
                Err(_) => (error(&ServerError::Protocol), None),
            };
            let sent = serde_json::to_vec(&reply)
                .map_err(|_| ServerError::Protocol)
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
        let (reply, launch) = self.respond(request, actor, NetworkCaller::of(actor, None));
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
    ) -> (LaneReply, Option<Dispatch>) {
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
                .status(Some(self.host.as_ref()))
                .map(LaneReply::Status),
            LaneRequest::Check => self.admin().check().map(LaneReply::Check),
            LaneRequest::StageFetch => self
                .admin()
                .stage_fetch()
                .map(|installation| staged_reply(&installation)),
            LaneRequest::StageUpload {
                length,
                sha256,
                signature,
            } => self.stage_upload(length, sha256, &signature),
            LaneRequest::UploadChunk {
                upload,
                offset,
                data,
            } => self.upload_chunk(&upload, offset, &data),
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
                    return error(&ServerError::Protocol);
                }
                if let Err(failure) = self.setups().begin(&setup, Instant::now()) {
                    return error(&failure);
                }
                // The admission ends however the preparation returns.
                struct Finish<'a>(&'a LaneServer, &'a str);
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
                    return error(&ServerError::Protocol);
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

    fn stage_upload(
        &self,
        length: u64,
        sha256: String,
        signature: &str,
    ) -> Result<LaneReply, ServerError> {
        let signature = base64::engine::general_purpose::STANDARD
            .decode(signature)
            .map_err(|_| ServerError::Protocol)?;
        if length == 0
            || length > feed::MAX_ARCHIVE
            || !runtime::digest_valid(&sha256)
            || signature.len() != 64
        {
            return Err(ServerError::Protocol);
        }
        let id = hex(&crate::protocol::random()?[..16]);
        let (archive, signature_path) = self.admin().staging_files(&id)?;
        let mut slot = self.upload.lock().map_err(|_| ServerError::Protocol)?;
        // One upload at a time: a new one replaces any unfinished upload.
        *slot = None;
        let file = create_private(&archive)?;
        create_private(&signature_path)?
            .write_all(&signature)
            .map_err(|_| ServerError::StorageUnavailable)?;
        *slot = Some(Upload {
            id: id.clone(),
            length,
            sha256,
            next: 0,
            file,
            archive,
            signature: signature_path,
        });
        Ok(LaneReply::UploadProgress {
            upload: id,
            next_offset: 0,
        })
    }

    fn upload_chunk(&self, id: &str, offset: u64, data: &str) -> Result<LaneReply, ServerError> {
        let mut slot = self.upload.lock().map_err(|_| ServerError::Protocol)?;
        let upload = slot
            .as_mut()
            .filter(|v| v.id == id)
            .ok_or(ServerError::Changed)?;
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(data)
            .map_err(|_| ServerError::Protocol)?;
        if bytes.is_empty()
            || bytes.len() > MAX_CHUNK
            || offset != upload.next
            || upload.next + bytes.len() as u64 > upload.length
        {
            return Err(ServerError::Protocol);
        }
        upload
            .file
            .write_all(&bytes)
            .map_err(|_| ServerError::StorageUnavailable)?;
        upload.next += bytes.len() as u64;
        if upload.next < upload.length {
            return Ok(LaneReply::UploadProgress {
                upload: upload.id.clone(),
                next_offset: upload.next,
            });
        }
        let upload = slot.take().ok_or(ServerError::Changed)?;
        upload
            .file
            .sync_all()
            .map_err(|_| ServerError::StorageUnavailable)?;
        self.service
            .stage_candidate(&upload.archive, &upload.signature, &upload.sha256)
            .map(|installation| staged_reply(&installation))
    }

    /// Plans a Network Access change without applying it. A paired caller's change that keeps
    /// no connected device's route is refused before any token is issued.
    fn preview_network(
        &self,
        access: NetworkAccess,
        actor: &Actor,
        caller: Option<NetworkCaller>,
    ) -> Result<LaneReply, ServerError> {
        let key = action_key(&LaneAction::Network { access })?;
        let mut planned = None;
        self.host
            .change_network(access, caller.as_ref(), &mut |plan| {
                planned = Some(plan.clone());
                Ok(false)
            })?;
        let plan = planned.ok_or(ServerError::Protocol)?;
        if plan.refused_for(caller.as_ref()) {
            return Ok(refusal());
        }
        let observation = plan.observation(&self.server, &self.boot, &key);
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
    ) -> Result<(LaneReply, Option<Dispatch>), ServerError> {
        let id = OperationId::parse(operation).ok_or(ServerError::Protocol)?;
        let _executing = self.executing.lock().unwrap_or_else(|e| e.into_inner());
        let accepted = LaneReply::Accepted {
            operation: id.to_string(),
            boot: hex(&self.boot),
        };
        if let Some(record) = self.ledger()?.resolve(&id)? {
            return if record.actor == *actor {
                Ok((accepted, None))
            } else {
                Err(ServerError::Changed)
            };
        }
        let key = action_key(&LaneAction::Network { access })?;
        let expected = self.tokens.redeem(token, &key, actor, Instant::now())?;
        let record = Record {
            id,
            actor: *actor,
            server: self.server,
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
                if plan.observation(&self.server, &self.boot, &key) != expected {
                    return Err(ServerError::Changed);
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
                if plan.observation(&self.server, &self.boot, &key) != observation {
                    return Err(ServerError::Changed);
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
        // Once the change started, `AddressUnavailable` means the host could not ready the new
        // listener and changed nothing. Any other failure is a commit it could not confirm.
        let state = match result {
            Ok(()) if refused => State::Failed {
                code: super::lane_codes::DISCONNECTS_EVERY_DEVICE.to_owned(),
            },
            Ok(()) => State::Done,
            Err(failure @ ServerError::Direct(crate::direct::DirectError::AddressUnavailable)) => {
                State::Failed {
                    code: super::lane_codes::code(&failure).to_owned(),
                }
            }
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

    fn preview(&self, action: &LaneAction, actor: &Actor) -> Result<LaneReply, ServerError> {
        let prepared = self.admin().prepare(action, &self.server, &self.boot)?;
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
    ) -> Result<(LaneReply, Option<OperationId>), ServerError> {
        let id = OperationId::parse(operation).ok_or(ServerError::Protocol)?;
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
                Err(ServerError::Changed)
            };
        }
        let key = action_key(action)?;
        let expected = self.tokens.redeem(token, &key, actor, Instant::now())?;
        let prepared = self.admin().prepare(action, &self.server, &self.boot)?;
        let record = Record {
            id,
            actor: *actor,
            server: self.server,
            boot: self.boot,
            action: match action {
                LaneAction::Stop => Action::Stop,
                LaneAction::Restart => Action::Restart,
                LaneAction::Update { .. } => Action::Update,
                LaneAction::Return => Action::Return,
                LaneAction::Network { .. } => return Err(ServerError::Protocol),
            },
            observation_digest: expected,
            candidate: prepared.candidate,
            state: State::Admitted,
            updated_at: 0,
            orphaned: false,
        };
        let mut admitted = None;
        self.host.fence(&mut |sessions| {
            if observation(&prepared, sessions) != expected {
                return Err(ServerError::Changed);
            }
            admitted = Some(self.ledger()?.admit(record.clone())?);
            Ok(())
        })?;
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
        true
    }

    /// Settles what can be settled and reports whether every record is terminal.
    fn settled(&self) -> Result<bool, ServerError> {
        let mut ledger = self.ledger()?;
        ledger.settle(&self.boot, &self.evidence()?, None)?;
        Ok(ledger.records()?.iter().all(|v| v.state.terminal()))
    }

    fn resolve(&self, operation: &str) -> Result<LaneReply, ServerError> {
        let id = OperationId::parse(operation).ok_or(ServerError::Protocol)?;
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
                Action::Restart => LaneAction::Restart,
                Action::Update => LaneAction::Update {
                    installation: record
                        .candidate
                        .as_ref()
                        .map(|v| hex(v))
                        .unwrap_or_default(),
                },
                Action::Return => LaneAction::Return,
                Action::Network(access) => LaneAction::Network { access },
            },
            state,
            code,
            boot: hex(&self.boot),
        }))
    }

    fn acknowledge(&self, operation: &str, actor: &Actor) -> Result<LaneReply, ServerError> {
        let id = OperationId::parse(operation).ok_or(ServerError::Protocol)?;
        self.ledger()?.acknowledge(&id, actor)?;
        Ok(LaneReply::Acknowledged)
    }

    fn admin(&self) -> Admin<'_> {
        Admin {
            service: &self.service,
            feed: self.feed.as_ref(),
        }
    }

    fn ledger(&self) -> Result<Ledger, ServerError> {
        Ok(Ledger::in_directory(Directory::open(
            &self.service.paths.data,
            false,
        )?))
    }

    fn evidence(&self) -> Result<Journal, ServerError> {
        Ok(Journal {
            service: self.service.clone(),
            data: Directory::open(&self.service.paths.data, false)?,
        })
    }
}

/// Service administration that needs no running server: status, the feed, staging and the
/// observation an action previews. The lane uses it while the server runs, and the CLI uses it
/// on the server's own machine while the server is stopped.
pub struct Admin<'a> {
    service: &'a Service,
    feed: &'a dyn Source,
}

impl Admin<'_> {
    /// The service's status, with what only a running server knows supplied by `live`.
    pub fn status(&self, live: Option<&dyn LaneHost>) -> Result<LaneStatus, ServerError> {
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
                staged: Vec::new(),
                activation: LaneActivation::Settled,
                sessions: 0,
                network: offline_network(self.service)?,
                devices: offline_devices(self.service)?,
                lane_major: LANE_MAJOR,
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
            staged: self
                .staged(&state)?
                .into_iter()
                .map(|v| LaneStaged {
                    version: v.version,
                    sha256: v.sha256,
                })
                .collect(),
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
            lane_major: LANE_MAJOR,
        })
    }

    pub fn check(&self) -> Result<LaneCheck, ServerError> {
        let (_, state) = self.service.read()?.ok_or(ServerError::NotInstalled)?;
        let latest = self.latest()?.map(|feed| {
            let staged = self.installation(&feed.archive.sha256).ok();
            LaneVersion {
                version: feed.version,
                commit: staged.map(|v| v.commit).unwrap_or_default(),
                sha256: feed.archive.sha256,
            }
        });
        Ok(LaneCheck {
            approved: self.approved(&state)?,
            latest,
            staged: self.staged(&state)?,
        })
    }

    fn latest(&self) -> Result<Option<feed::Feed>, ServerError> {
        let Some((document, signature)) = self.feed.latest()? else {
            return Ok(None);
        };
        feed::verify(&self.service.identity, &document, &signature).map(Some)
    }

    /// Downloads and stages the latest feed's archive, or finds it already staged.
    pub fn stage_fetch(&self) -> Result<RetainedInstallation, ServerError> {
        let feed = self.latest()?.ok_or(ServerError::CandidateMissing)?;
        if let Ok(installation) = self.installation(&feed.archive.sha256) {
            return Ok(installation);
        }
        let (archive, signature) = self.staging_files(&hex(&crate::protocol::random()?[..8]))?;
        let result = (|| {
            let mut file = create_private(&archive)?;
            self.feed.download(&feed, &mut file)?;
            file.sync_all()
                .map_err(|_| ServerError::StorageUnavailable)?;
            create_private(&signature)?
                .write_all(&feed.archive.signature)
                .map_err(|_| ServerError::StorageUnavailable)?;
            self.service
                .stage_candidate(&archive, &signature, &feed.archive.sha256)
        })();
        let _ = std::fs::remove_file(&archive);
        let _ = std::fs::remove_file(&signature);
        result
    }

    /// Stages a signed archive and its detached signature from local files.
    pub fn stage_files(
        &self,
        archive: &std::path::Path,
        signature: &std::path::Path,
    ) -> Result<RetainedInstallation, ServerError> {
        let bytes = std::fs::read(archive).map_err(|_| ServerError::CandidateMissing)?;
        if bytes.len() as u64 > feed::MAX_ARCHIVE {
            return Err(ServerError::RuntimeUnavailable);
        }
        let sha256 = hex(ring::digest::digest(&ring::digest::SHA256, &bytes).as_ref());
        self.service.stage_candidate(archive, signature, &sha256)
    }

    /// Observes the service for `action` without changing it.
    fn prepare(
        &self,
        action: &LaneAction,
        server: &Id,
        boot: &Id,
    ) -> Result<Prepared, ServerError> {
        let mut digest = ring::digest::Context::new(&ring::digest::SHA256);
        digest.update(b"spaceterm-service-lane-observation-1\0");
        digest.update(server);
        digest.update(boot);
        digest.update(&action_key(action)?);
        let mut effects = LaneEffects {
            ends_sessions: 0,
            sessions: Vec::new(),
            start_requires_machine: false,
            from: None,
            to: None,
            kind: None,
            to_major: None,
        };
        let (running, candidate) = match action {
            LaneAction::Stop | LaneAction::Restart => {
                let control = if *action == LaneAction::Stop {
                    super::control::Action::Stop
                } else {
                    super::control::Action::Restart
                };
                let confirmation =
                    super::control::Controller::new(self.service.clone()).prepare(control)?;
                let (_, state) = self.service.read()?.ok_or(ServerError::NotInstalled)?;
                digest.update(&serde_json::to_vec(&state).map_err(|_| ServerError::Protocol)?);
                effects.start_requires_machine = *action == LaneAction::Stop;
                (confirmation.running(), None)
            }
            LaneAction::Update { installation } => {
                let candidate = self.installation(installation)?;
                let approval = self
                    .service
                    .prepare_activation(&candidate)?
                    .ok_or(ServerError::CandidateMissing)?;
                digest.update(approval.confirmation_token()?.as_bytes());
                effects.from = Some(version_of(approval.previous()));
                effects.to = Some(version_of(approval.candidate()));
                effects.kind = Some(LaneUpdateKind::Update);
                effects.to_major = protocol_major(approval.candidate());
                (approval.running(), Some(digest_bytes(installation)?))
            }
            LaneAction::Return => {
                let approval = self.service.prepare_return()?;
                digest.update(approval.confirmation_token()?.as_bytes());
                // Return activates the previous runtime, and the candidate stays retained.
                effects.from = Some(version_of(approval.candidate()));
                effects.to = Some(version_of(approval.previous()));
                effects.kind = Some(LaneUpdateKind::Return);
                effects.to_major = protocol_major(approval.previous());
                (false, None)
            }
            LaneAction::Network { .. } => return Err(ServerError::Unsupported),
        };
        Ok(Prepared {
            service: digest
                .finish()
                .as_ref()
                .try_into()
                .map_err(|_| ServerError::Protocol)?,
            running,
            effects,
            candidate,
        })
    }

    fn approved(&self, state: &super::State) -> Result<LaneVersion, ServerError> {
        Ok(version_of(&self.service.describe_approved(state)?))
    }

    /// Verified installations other than the approved one.
    fn staged(&self, state: &super::State) -> Result<Vec<LaneVersion>, ServerError> {
        let parent = self.service.paths.data.join("installations");
        let Ok(entries) = std::fs::read_dir(&parent) else {
            return Ok(Vec::new());
        };
        let mut staged = Vec::new();
        for entry in entries.flatten() {
            let name = entry.file_name();
            let Some(name) = name.to_str() else { continue };
            if !runtime::digest_valid(name) || name == state.installation {
                continue;
            }
            if let Ok(installation) = self.installation(name) {
                staged.push(version_of(&installation));
            }
        }
        staged.sort_by(|a, b| (&a.version, &a.sha256).cmp(&(&b.version, &b.sha256)));
        Ok(staged)
    }

    /// A verified retained installation by digest.
    pub fn installation(&self, digest: &str) -> Result<RetainedInstallation, ServerError> {
        if !runtime::digest_valid(digest) {
            return Err(ServerError::CandidateMissing);
        }
        let directory = self.service.paths.data.join("installations").join(digest);
        if !directory.is_dir() {
            return Err(ServerError::CandidateMissing);
        }
        let (version, commit) = runtime::describe(&directory, &self.service.identity)?;
        Ok(RetainedInstallation {
            directory,
            version,
            commit,
        })
    }

    fn staging_files(&self, id: &str) -> Result<(PathBuf, PathBuf), ServerError> {
        let directory = self.service.paths.data.join("staging");
        std::fs::DirBuilder::new()
            .mode(0o700)
            .recursive(true)
            .create(&directory)
            .map_err(|_| ServerError::StorageUnavailable)?;
        Directory::open(&directory, false)?;
        Ok((
            directory.join(format!("{id}.tar.gz")),
            directory.join(format!("{id}.sig")),
        ))
    }
}

/// [`Admin`] with its own service and the public release feed, for a stopped server.
pub struct Offline {
    service: Service,
    feed: Arc<dyn Source>,
}

impl Offline {
    pub fn new(identity: &str) -> Result<Self, ServerError> {
        Ok(Self {
            service: Service::for_identity(identity)?,
            feed: Arc::new(feed::Releases::new(identity)),
        })
    }

    pub fn admin(&self) -> Admin<'_> {
        Admin {
            service: &self.service,
            feed: self.feed.as_ref(),
        }
    }
}

/// Network Access as a stopped server's own direct state records it.
fn offline_network(service: &Service) -> Result<LaneNetwork, ServerError> {
    let (policy, enabled, _) = crate::direct::observe(&service.paths)?;
    Ok(LaneNetwork {
        enabled,
        bind: policy.map(|policy| policy.bind.to_string()),
        listening: false,
        addresses: Vec::new(),
    })
}

/// Paired devices as a stopped server's own direct state records them.
fn offline_devices(service: &Service) -> Result<Vec<LaneDevice>, ServerError> {
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

/// Settlement evidence from this account's own files.
struct Journal {
    service: Service,
    data: Directory,
}

impl Evidence for Journal {
    fn helper_active(&self) -> Result<bool, ServerError> {
        Ok(self.data.try_lock(HELPER_LOCK)?.is_none())
    }

    fn activation(&self, record: &Record) -> Result<Option<State>, ServerError> {
        // An operation that holds the service lock may be the activation itself. Wait for it
        // to finish instead of reading its journal mid-step.
        let Some(_operation) = self.data.try_lock("service.lock")? else {
            return Err(ServerError::ServiceBusy);
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
            Action::Return if status.returned => Some(State::Done),
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

fn action_key(action: &LaneAction) -> Result<Vec<u8>, ServerError> {
    serde_json::to_vec(action).map_err(|_| ServerError::Protocol)
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

fn staged_reply(installation: &RetainedInstallation) -> LaneReply {
    let version = version_of(installation);
    LaneReply::Staged {
        installation: version.sha256,
        version: version.version,
        commit: version.commit,
    }
}

/// The protocol major a verified installation's signed build information declares.
fn protocol_major(installation: &RetainedInstallation) -> Option<u16> {
    #[derive(Deserialize)]
    struct Major {
        protocol_major: u16,
    }
    let bytes = std::fs::read(installation.directory.join("build.json")).ok()?;
    serde_json::from_slice::<Major>(&bytes)
        .ok()
        .map(|v| v.protocol_major)
}

fn digest_bytes(text: &str) -> Result<[u8; 32], ServerError> {
    if !runtime::digest_valid(text) {
        return Err(ServerError::CandidateMissing);
    }
    let mut out = [0; 32];
    for (index, byte) in out.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&text[index * 2..index * 2 + 2], 16)
            .map_err(|_| ServerError::CandidateMissing)?;
    }
    Ok(out)
}

fn create_private(path: &std::path::Path) -> Result<std::fs::File, ServerError> {
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)
        .map_err(|_| ServerError::StorageUnavailable)
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

fn error(failure: &ServerError) -> LaneReply {
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
