//! Bounded actor API. Handles own neither terminal workers nor store authority.

use crate::adapters::{
    core::CorePorts,
    terminal::{LaunchPreparationPort, RuntimeCleanupPort},
};
use crate::contracts::ServerFailureClass;
use crate::contracts::{ConnectionAccessSnapshot, ServerFailure, ServerFuture};
use crate::failure::failure;
use crate::service::{ActorRequest, LifecycleRequest};
use crate::shutdown::{ShutdownReport, TrustedShutdownRequest};
use spaceterm_core::{
    authority::*,
    geometry::{GridLimits, SUPPORTED_GRID_LIMITS},
    identity::*,
    operation_ledger_contracts::CanonicalOperationRequest,
    operations::*,
    organization::OrganizationSnapshot,
    storage::{CommitReceipt, TransactionParts},
    worktrees::intent::*,
};
use spaceterm_protocol::connection::AuthenticatedConnection;
use std::sync::Arc;
use tokio::sync::{mpsc, oneshot, watch};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ServicePhase {
    Recovering,
    Ready,
    Serving,
    Quiescing,
    Draining,
    Stopped,
    RecoveryRequired,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ServerLimits {
    pub actor_requests: usize,
    pub runtime_owners: usize,
    pub startup_intents: usize,
    pub startup_intents_per_connection: usize,
    pub launch_preparations: usize,
    pub connections: usize,
    pub grid: GridLimits,
}
impl Default for ServerLimits {
    fn default() -> Self {
        Self {
            actor_requests: 256,
            runtime_owners: 64,
            startup_intents: 128,
            startup_intents_per_connection: 32,
            launch_preparations: 4,
            connections: 32,
            grid: SUPPORTED_GRID_LIMITS,
        }
    }
}
impl ServerLimits {
    pub fn validate(self) -> Result<Self, ServerFailure> {
        for (actual, limit) in [
            (self.actor_requests, 256),
            (self.runtime_owners, 64),
            (self.startup_intents, 128),
            (self.startup_intents_per_connection, 32),
            (self.launch_preparations, 4),
            (self.connections, 32),
        ] {
            if actual == 0 || actual > limit {
                return Err(ServerFailure {
                    class: ServerFailureClass::Capacity,
                    expected: None,
                    actual: Some(actual as u64),
                    limit: Some(limit as u64),
                });
            }
        }
        Ok(self)
    }
}

/// Only native composition implements this port. Validation covers account/build,
/// approved installation and Running intent before recovery or credential use.
pub trait StartupAuthorityPort: Send + Sync {
    fn validate_startup(&self) -> Result<ServerId, ServerFailure>;
    fn validate_credentials(
        &self,
        credentials: &dyn crate::contracts::ServerCredentialPort,
        server: ServerId,
    ) -> Result<(), ServerFailure>;
}
pub struct ServerHostContext {
    pub authority: Arc<dyn StartupAuthorityPort>,
}
pub struct ServerConfig {
    pub limits: ServerLimits,
    pub host: ServerHostContext,
    pub core: CorePorts,
    pub launch: Arc<dyn LaunchPreparationPort>,
    pub cleanup: Arc<dyn RuntimeCleanupPort>,
    pub extensions: Arc<dyn crate::extensions::ExtensionObserverPort>,
    pub initial_appearance: spaceterm_terminal::runtime_contracts::RuntimeAppearance,
    pub outcomes: Arc<dyn ReliableOutcomePort>,
}

/// Constructed from the real protocol's ordinary authenticated type state only.
/// Comparison data or a restricted connection cannot construct this principal.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct VerifiedPrincipal {
    server: ServerId,
    device: DeviceId,
    pairing: PairingId,
    client: ClientId,
    binding: ConnectionBinding,
    epoch: ServerEpoch,
}
impl VerifiedPrincipal {
    pub fn from_authenticated(
        codec: &AuthenticatedConnection,
        generation: ConnectionGeneration,
    ) -> Result<Self, ServerFailure> {
        let p = codec.principal();
        let server = ServerId::new(p.server);
        let pairing = PairingId::new(server, local_identity(p.pairing)?)
            .map_err(|_| failure(ServerFailureClass::Authentication))?;
        let client = ClientId::new(server, local_identity(p.client)?)
            .map_err(|_| failure(ServerFailureClass::Authentication))?;
        if generation.get() == 0 || codec.epoch() == 0 {
            return Err(failure(ServerFailureClass::Authentication));
        }
        Ok(Self {
            server,
            device: DeviceId::new(p.device),
            pairing,
            client,
            binding: ConnectionBinding::new(client, generation),
            epoch: ServerEpoch::new(codec.epoch()),
        })
    }
    pub const fn server(self) -> ServerId {
        self.server
    }
    pub const fn device(self) -> DeviceId {
        self.device
    }
    pub const fn pairing(self) -> PairingId {
        self.pairing
    }
    pub const fn client(self) -> ClientId {
        self.client
    }
    pub const fn binding(self) -> ConnectionBinding {
        self.binding
    }
    pub const fn epoch(self) -> ServerEpoch {
        self.epoch
    }
}
fn local_identity(bytes: [u8; 16]) -> Result<u64, ServerFailure> {
    let mut low = [0; 8];
    low.copy_from_slice(&bytes[8..]);
    let n = u64::from_be_bytes(low);
    if bytes[..8] != [0; 8] || n == 0 {
        return Err(failure(ServerFailureClass::Authentication));
    }
    Ok(n)
}

pub struct AuthenticatedSession {
    pub principal: VerifiedPrincipal,
    pub codec: AuthenticatedConnection,
}
impl AuthenticatedSession {
    pub fn new(
        codec: AuthenticatedConnection,
        generation: ConnectionGeneration,
    ) -> Result<Self, ServerFailure> {
        let principal = VerifiedPrincipal::from_authenticated(&codec, generation)?;
        Ok(Self { principal, codec })
    }
}

/// Reliable per-connection egress capacity is reserved before ledger admission.
/// The permit retains its charge through enqueue or a classified connection close.
/// Only the connection's egress owner implements publication of these typed facts.
pub trait ReliableOutcomePermit: Send {
    fn publish(&mut self, response: &OperationResponse) -> Result<(), ServerFailure>;
}
pub trait ReliableOutcomePort: Send + Sync {
    fn reserve(
        &self,
        principal: VerifiedPrincipal,
        binding: OperationBinding,
    ) -> Result<Box<dyn ReliableOutcomePermit>, ServerFailure>;
}

#[derive(Clone, Eq, PartialEq)]
pub enum OperationResponse {
    Pending(OperationBinding),
    Completed {
        binding: OperationBinding,
        revision: OrganizationRevision,
        outcome: Outcome,
    },
    Retired,
}

impl std::fmt::Debug for OperationResponse {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("OperationResponse([redacted])")
    }
}

pub trait ListenerLifecyclePort: Send + Sync {
    fn open(
        &self,
        handle: ServerHandle,
        snapshot: Arc<OrganizationSnapshot>,
    ) -> ServerFuture<'_, Result<(), ServerFailure>>;
    /// Apply only the committed version and exact IPv4/IPv6 scope. The provider
    /// retains that generation and must not reopen it after orderly close.
    fn apply_remote_policy(
        &self,
        policy: spaceterm_core::organization::Versioned<
            spaceterm_core::remote_access::RemoteAccessPolicy,
        >,
        receipt: CommitReceipt,
    ) -> ServerFuture<'_, Result<(), ServerFailure>>;
    /// Close listeners and their transports, proving identity-checked IPC cleanup.
    fn close(&self) -> ServerFuture<'_, Result<(), ServerFailure>>;
}
/// Bounded allocation needs. Only the reducer chooses identities and ordinals.
pub struct GitAllocationNeeds {
    pub repository: bool,
    pub worktree: bool,
    pub fallbacks: Vec<WorkspaceFallbackPlan>,
}
pub struct ReservedGitAllocation {
    pub reservation: GitAllocationReservation,
    pub receipt: CommitReceipt,
}

/// Per-connection presentation facts. These never become organization state or
/// local-file authority. Native completions still require the native provider.
#[derive(Clone, Copy)]
pub struct ClientFocusUpdate {
    pub context: ContextGeneration,
    pub focused_pane: Option<PaneId>,
    pub terminal_input_focus: bool,
    pub native_effects_eligible: bool,
}
pub struct RuntimeAccess {
    pub authority: spaceterm_core::runtime_authority::RuntimeAuthority,
    pub handle: Arc<dyn crate::contracts::RuntimeHandlePort>,
}

pub struct ListenerFactories {
    pub lifecycle: Arc<dyn ListenerLifecyclePort>,
}

#[derive(Clone)]
pub struct ServerHandle {
    pub(crate) requests: mpsc::Sender<ActorRequest>,
    pub(crate) lifecycle: mpsc::Sender<LifecycleRequest>,
    pub(crate) phase: watch::Receiver<ServicePhase>,
    pub(crate) snapshot: watch::Receiver<Arc<OrganizationSnapshot>>,
}
impl ServerHandle {
    pub fn phase(&self) -> ServicePhase {
        *self.phase.borrow()
    }
    pub fn snapshot(&self) -> Arc<OrganizationSnapshot> {
        self.snapshot.borrow().clone()
    }
    pub fn subscribe_snapshot(&self) -> watch::Receiver<Arc<OrganizationSnapshot>> {
        self.snapshot.clone()
    }
    pub async fn register(
        &self,
        principal: VerifiedPrincipal,
    ) -> Result<ConnectionAccessSnapshot, ServerFailure> {
        let (reply, result) = oneshot::channel();
        self.send(ActorRequest::Register { principal, reply })?;
        receive(result).await
    }
    pub async fn reconcile(
        &self,
        principal: VerifiedPrincipal,
        revision: OrganizationRevision,
    ) -> Result<ConnectionAccessSnapshot, ServerFailure> {
        let (reply, result) = oneshot::channel();
        self.send(ActorRequest::Reconcile {
            principal,
            revision,
            reply,
        })?;
        receive(result).await
    }
    pub async fn submit(
        &self,
        principal: VerifiedPrincipal,
        request: CanonicalOperationRequest,
    ) -> Result<OperationResponse, ServerFailure> {
        let (reply, result) = oneshot::channel();
        self.send(ActorRequest::Submit {
            principal,
            request,
            reply,
        })?;
        receive(result).await
    }
    pub async fn lookup(
        &self,
        principal: VerifiedPrincipal,
        binding: OperationBinding,
    ) -> Result<OperationResponse, ServerFailure> {
        let (reply, result) = oneshot::channel();
        self.send(ActorRequest::Lookup {
            principal,
            binding,
            reply,
        })?;
        receive(result).await
    }
    pub async fn acknowledge(
        &self,
        principal: VerifiedPrincipal,
        stream: OperationStreamId,
        through: OperationSequence,
    ) -> Result<CommitReceipt, ServerFailure> {
        let (reply, result) = oneshot::channel();
        self.send(ActorRequest::Acknowledge {
            principal,
            stream,
            through,
            reply,
        })?;
        receive(result).await
    }
    /// Reserved lifecycle capacity stays independent of decoded request pressure.
    pub async fn disconnect(&self, principal: VerifiedPrincipal) -> Result<(), ServerFailure> {
        let (reply, result) = oneshot::channel();
        self.lifecycle
            .send(LifecycleRequest::Disconnect { principal, reply })
            .await
            .map_err(|_| failure(ServerFailureClass::RecoveryRequired))?;
        receive(result).await
    }
    pub async fn shutdown(
        &self,
        request: TrustedShutdownRequest,
    ) -> Result<ShutdownReport, ServerFailure> {
        let (reply, result) = oneshot::channel();
        self.lifecycle
            .send(LifecycleRequest::Shutdown { request, reply })
            .await
            .map_err(|_| failure(ServerFailureClass::RecoveryRequired))?;
        receive(result).await
    }
    /// Allocate only after durable ledger admission. The same actor transaction
    /// persists the reducer's reservation and advances its identity high-water.
    pub async fn reserve_git_allocation(
        &self,
        binding: OperationBinding,
        needs: GitAllocationNeeds,
    ) -> Result<ReservedGitAllocation, ServerFailure> {
        let (reply, result) = oneshot::channel();
        self.send(ActorRequest::ReserveGitAllocation {
            binding,
            needs,
            reply,
        })?;
        receive(result).await
    }
    /// Invoked by trusted extensions only. It advances an already-admitted
    /// operation, never performs a second admission or trusts wire intent bytes.
    pub async fn record_external_progress(
        &self,
        binding: OperationBinding,
        progress: spaceterm_core::operation_ledger_contracts::PendingExternalProgress,
        writes: Vec<spaceterm_core::storage::OperationWrite>,
    ) -> Result<CommitReceipt, ServerFailure> {
        let (reply, result) = oneshot::channel();
        self.send(ActorRequest::ExternalProgress {
            binding,
            progress,
            writes,
            reply,
        })?;
        receive(result).await
    }
    /// Wait for the already committed non-deleting Git retirement. This observes
    /// one retained cleanup result and never requests a second process effect.
    pub async fn await_git_cleanup(
        &self,
        binding: OperationBinding,
    ) -> Result<Vec<spaceterm_core::retirement::SessionCleanupObservation>, ServerFailure> {
        let (reply, result) = oneshot::channel();
        self.send(ActorRequest::GitCleanup { binding, reply })?;
        receive(result).await
    }
    /// Not an ordinary principal. Exact durable Started intent grants only its
    /// bounded settlement, even while its original stream is retired.
    pub async fn settle_started_git(
        &self,
        intent: GitWriteIntent,
        result: RetainedExternalResult,
    ) -> Result<OperationResponse, ServerFailure> {
        let (reply, receiver) = oneshot::channel();
        self.send(ActorRequest::SettleGit {
            intent: Box::new(intent),
            result,
            reply,
        })?;
        receive(receiver).await
    }
    /// Trusted pairing policy completes an already-admitted canonical request.
    /// Allocation, binding, purpose-bound capacity and outcome share one commit.
    /// This supplies no ordinary principal or restricted enrollment authority.
    pub async fn complete_pairing(
        &self,
        request: CanonicalOperationRequest,
        completion: crate::extensions::PairingCompletionRequest,
    ) -> Result<OperationResponse, ServerFailure> {
        completion.validate_bounds()?;
        let (reply, receiver) = oneshot::channel();
        self.send(ActorRequest::CompletePairing {
            request,
            completion,
            reply,
        })?;
        receive(receiver).await
    }
    pub async fn refresh_runtime_observations(&self) -> Result<(), ServerFailure> {
        let (reply, result) = oneshot::channel();
        self.send(ActorRequest::Observe { reply })?;
        receive(result).await
    }
    pub async fn update_client_focus(
        &self,
        principal: VerifiedPrincipal,
        update: ClientFocusUpdate,
    ) -> Result<ContextGeneration, ServerFailure> {
        let (reply, result) = oneshot::channel();
        self.send(ActorRequest::Focus {
            principal,
            update,
            reply,
        })?;
        receive(result).await
    }
    /// Disposable access, never lifecycle ownership. Every handle request still
    /// carries its original fence for the worker's final authority comparison.
    pub async fn runtime_access(
        &self,
        principal: VerifiedPrincipal,
        session: SessionKey,
    ) -> Result<RuntimeAccess, ServerFailure> {
        let (reply, result) = oneshot::channel();
        self.send(ActorRequest::RuntimeAccess {
            principal,
            session,
            reply,
        })?;
        receive(result).await
    }
    pub async fn subscribe_runtime(
        &self,
        principal: VerifiedPrincipal,
        fence: EffectFence,
    ) -> Result<
        Box<dyn crate::contracts::RuntimeStream<crate::contracts::RuntimePresentationEvent>>,
        ServerFailure,
    > {
        let (reply, result) = oneshot::channel();
        self.send(ActorRequest::SubscribeRuntime {
            principal,
            fence,
            reply,
        })?;
        receive(result).await
    }
    pub async fn runtime_command(
        &self,
        principal: VerifiedPrincipal,
        command: spaceterm_terminal::runtime_contracts::FencedRuntimeCommand,
    ) -> Result<spaceterm_core::runtime_authority::EffectDisposition, ServerFailure> {
        let (reply, result) = oneshot::channel();
        self.send(ActorRequest::RuntimeCommand {
            principal,
            command: Box::new(command),
            reply,
        })?;
        receive(result).await
    }
    /// Trusted notification of an already durable pairing revocation, not an
    /// ordinary wire command. The pairing owner supplies every retained Client.
    pub async fn revoke_clients(
        &self,
        clients: Vec<ClientId>,
    ) -> Result<CommitReceipt, ServerFailure> {
        let (reply, result) = oneshot::channel();
        self.lifecycle
            .send(LifecycleRequest::RevokeClients { clients, reply })
            .await
            .map_err(|_| failure(ServerFailureClass::RecoveryRequired))?;
        receive(result).await
    }
    pub(crate) fn send(&self, request: ActorRequest) -> Result<(), ServerFailure> {
        self.requests
            .try_send(request)
            .map_err(|_| failure(ServerFailureClass::Capacity))
    }
}
pub(crate) async fn receive<T>(
    result: oneshot::Receiver<Result<T, ServerFailure>>,
) -> Result<T, ServerFailure> {
    result
        .await
        .map_err(|_| failure(ServerFailureClass::RecoveryRequired))?
}

/// Pure extension proposals are checked and committed by the actor, not by their
/// producer. This alias deliberately retains the one core transaction inventory.
pub type ExtensionRecoveryTransaction = TransactionParts;
