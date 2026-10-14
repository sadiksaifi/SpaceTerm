//! Server-core composition ports frozen before actor implementation. These
//! declarations grant no native authority and install no production provider.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use spaceterm_core::authority::EffectFence;
use spaceterm_core::authority::{ConnectionBinding, ServerEpoch, SessionKey};
use spaceterm_core::content::ContentBase;
use spaceterm_core::content_queries::{
    ContentPageRequest, ContentPageResult, CopyRequest, CopyStreamItem, FindRequest,
    FindStreamItem, GraphicsChunk, GraphicsRequest, TopologyPageRequest, TopologyPageResult,
};
use spaceterm_core::identity::{ClientId, DeviceId, PairingId, ServerId};
use spaceterm_core::native_requests::{NativeCompletion, NativeRequest};
use spaceterm_core::observations::{MonotonicTick, RuntimeObservationSource};
use spaceterm_core::operations::{OperationBinding, Outcome};
use spaceterm_core::retirement::{RetirementRequest, SessionCleanupObservation};
use spaceterm_core::runtime_authority::{
    AuthorityApplied, EffectDisposition, RuntimeAuthorityTransition, RuntimeLifecycle,
};
use spaceterm_core::storage::{OrganizationStore, RecoveredState, TransactionParts};
use spaceterm_protocol::authentication::{PairingLookup, SigningKey};
use spaceterm_terminal::runtime_contracts::{
    FencedRuntimeCommand, RuntimeMetadataSnapshot, RuntimeStartRequest,
};

pub type ServerFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ServerFailureClass {
    Authentication,
    ScopeMismatch,
    StaleAuthority,
    Capacity,
    Exhausted,
    StorageUnavailable,
    RecoveryRequired,
    RuntimeUnavailable,
    NativeUnavailable,
    CleanupIncomplete,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ServerFailure {
    pub class: ServerFailureClass,
    pub expected: Option<u64>,
    pub actual: Option<u64>,
    pub limit: Option<u64>,
}

/// One epoch-scoped monotonic source shared with runtime owners and assessments.
pub trait ServerClock: Send + Sync {
    fn now(&self) -> Result<MonotonicTick, ServerFailure>;
}
pub trait ServerCredentialPort: SigningKey {}
pub trait PairingAuthorizationPort: PairingLookup + Send + Sync {}

/// Bounded, ordered, cancellation-aware stream. Drop cancels only this read or
/// subscription. It never retires an owner. Native payloads are transient.
pub trait RuntimeStream<T>: Send {
    fn next(&mut self) -> ServerFuture<'_, Option<Result<T, ServerFailure>>>;
}
pub enum RuntimePresentationEvent {
    ContentInvalidated(ContentBase),
    NativeRequest(NativeRequest),
    Lifecycle(RuntimeLifecycle),
    Metadata(RuntimeMetadataSnapshot),
    DisplayDirectory(spaceterm_core::observations::DisplayDirectoryObservation),
    ConfirmedDirectory(spaceterm_core::observations::ConfirmedDirectoryObservation),
    CloseWork(spaceterm_core::observations::CloseWorkObservation),
}
/// Clone the Arc, not lifetime authority. Immediate Result is ingress admission;
/// the future is the bounded worker reply. Dropping it cancels only that request.
pub trait RuntimeHandlePort: Send + Sync {
    fn try_submit(
        &self,
        command: FencedRuntimeCommand,
    ) -> Result<ServerFuture<'_, Result<EffectDisposition, ServerFailure>>, ServerFailure>;
    fn complete_native(
        &self,
        completion: NativeCompletion,
    ) -> Result<ServerFuture<'_, Result<EffectDisposition, ServerFailure>>, ServerFailure>;
    fn read_page(
        &self,
        request: ContentPageRequest,
    ) -> Result<ServerFuture<'_, Result<ContentPageResult, ServerFailure>>, ServerFailure>;
    fn read_topology(
        &self,
        request: TopologyPageRequest,
    ) -> Result<ServerFuture<'_, Result<TopologyPageResult, ServerFailure>>, ServerFailure>;
    fn prepare_copy(
        &self,
        request: CopyRequest,
    ) -> Result<Box<dyn RuntimeStream<CopyStreamItem>>, ServerFailure>;
    fn find(
        &self,
        request: FindRequest,
    ) -> Result<Box<dyn RuntimeStream<FindStreamItem>>, ServerFailure>;
    fn read_graphics(
        &self,
        request: GraphicsRequest,
    ) -> Result<Box<dyn RuntimeStream<GraphicsChunk>>, ServerFailure>;
}
/// A non-cloneable native runtime owner. Disconnect and subscriber Drop must not
/// retire this owner. Cleanup refers to the owner's first exact Session reason.
pub trait RuntimeOwnerPort: RuntimeObservationSource {
    fn handle(&self) -> Arc<dyn RuntimeHandlePort>;
    fn subscribe(
        &self,
        fence: EffectFence,
    ) -> Result<Box<dyn RuntimeStream<RuntimePresentationEvent>>, ServerFailure>;
    fn lifecycle_snapshot(&self) -> RuntimeLifecycle;
    /// Retained source facts; polling never replaces the original source sequence or tick.
    fn metadata_snapshot(&self) -> Option<RuntimeMetadataSnapshot>;
    /// Completion acknowledges both the final-effect barrier and worker cleanup.
    /// The actor awaits this before enabling the new connection's effects.
    fn transition_authority(
        &mut self,
        transition: RuntimeAuthorityTransition,
    ) -> Result<ServerFuture<'_, Result<AuthorityApplied, ServerFailure>>, ServerFailure>;
    fn retire<'a>(
        &'a mut self,
        request: RetirementRequest<'a>,
    ) -> ServerFuture<'a, Result<SessionCleanupObservation, ServerFailure>>;
}
/// Composition calls this only after admission and StartupReserved commit.
/// A measured reservation by itself never establishes executable authority.
pub trait RuntimeFactoryPort: Send + Sync {
    fn start<'a>(
        &'a self,
        request: &'a RuntimeStartRequest,
    ) -> ServerFuture<'a, Result<Box<dyn RuntimeOwnerPort>, ServerFailure>>;
}
/// Pure proposals, not separately committed auxiliary edits. The actor installs
/// them only through its one OrganizationStore and domain validator.
pub struct RecoveryEdits {
    pub transactions: Vec<TransactionParts>,
}
pub trait ExternalWorkPort: Send + Sync {
    /// One bounded recovery proposal against freshly recovered committed facts.
    /// The state owner commits it and supplies the next basis. Trusted native
    /// cleanup is comparison evidence, never synthesized by the extension.
    fn prepare_recovery_step<'a>(
        &'a self,
        _recovered: &'a RecoveredState,
        _cleanup: &'a [SessionCleanupObservation],
    ) -> ServerFuture<'a, Result<Option<TransactionParts>, ServerFailure>> {
        Box::pin(async { Ok(None) })
    }
    fn cancel_reads(&self);
    fn finish_started_writes(&self) -> ServerFuture<'_, Result<(), ServerFailure>>;
    fn reconcile<'a>(
        &'a self,
        recovered: &'a RecoveredState,
    ) -> ServerFuture<'a, Result<RecoveryEdits, ServerFailure>>;
}
/// Trusted native management provider. Actor code must validate the provider's
/// durable Stopped authorization before shutting down. This port is not a
/// decoded wire request, same-UID shortcut, or fabricated pairing identity.
pub trait HostManagementPort: Send + Sync {
    /// Fences the exact next recovery epoch after previous-instance cleanup and
    /// before any external reconciliation work or epoch commit can start.
    fn prepare_next_instance<'a>(
        &'a self,
        _server: ServerId,
        _next_epoch: ServerEpoch,
    ) -> ServerFuture<'a, Result<(), ServerFailure>> {
        Box::pin(async { Ok(()) })
    }
    /// The first retirement reason comes from the recovered store, not a new
    /// request. Existing providers must return observations matching it exactly.
    fn reconcile_previous_instance<'a>(
        &'a self,
        server: ServerId,
        epoch: ServerEpoch,
        expected: &'a [spaceterm_core::retirement::RetiredSession],
    ) -> ServerFuture<'a, Result<Vec<SessionCleanupObservation>, ServerFailure>> {
        Box::pin(async move {
            let sessions: Vec<_> = expected.iter().map(|value| value.session()).collect();
            self.previous_instance_cleanup(server, epoch, &sessions)
                .await
        })
    }
    fn previous_instance_cleanup<'a>(
        &'a self,
        server: ServerId,
        epoch: ServerEpoch,
        expected_sessions: &'a [SessionKey],
    ) -> ServerFuture<'a, Result<Vec<SessionCleanupObservation>, ServerFailure>>;
}
pub struct ServerDependencies {
    pub store: Box<dyn OrganizationStore>,
    pub credentials: Arc<dyn ServerCredentialPort>,
    pub pairing: Arc<dyn PairingAuthorizationPort>,
    pub runtime: Arc<dyn RuntimeFactoryPort>,
    pub external_work: Arc<dyn ExternalWorkPort>,
    pub host_management: Arc<dyn HostManagementPort>,
    pub clock: Arc<dyn ServerClock>,
}

/// Frozen actor access comparison. Authentication constructs its principal in
/// the actor-owned API; this raw record cannot grant ordinary/effect access.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ConnectionAccessSnapshot {
    pub server: ServerId,
    pub device: DeviceId,
    pub pairing: PairingId,
    pub client: ClientId,
    pub binding: ConnectionBinding,
    pub epoch: ServerEpoch,
    pub reconciled_revision: spaceterm_core::authority::OrganizationRevision,
    pub effects_enabled: bool,
}

/// Unsigned typed egress. Queue policy cannot relabel arbitrary bytes as
/// lifecycle traffic. Signing and envelope sequencing happen at final dequeue.
#[derive(Clone, Debug)]
pub enum OutboundMessage {
    SnapshotPart(crate::publication::SnapshotPage),
    Outcome {
        binding: OperationBinding,
        revision: spaceterm_core::authority::OrganizationRevision,
        value: Outcome,
    },
    ResyncRequired {
        epoch: ServerEpoch,
        revision: spaceterm_core::authority::OrganizationRevision,
    },
    PendingRetirement {
        binding: OperationBinding,
        completed: u32,
        total: u32,
    },
    Status(spaceterm_core::management::StatusPublication),
    ContentInvalidated(SessionKey),
}
