//! Injected connection boundaries. None can install, Start, enroll or replace a pin.

use crate::{
    api::{ConnectedInfo, ConnectionTarget, PairedServerBinding},
    failure::ClientFailure,
    limits::ClientLimits,
    replica::{PublicationStamp, ReplicaSnapshot, SnapshotPart},
};
use spaceterm_core::{operations::OperationId, organization::OrganizationDelta};
use spaceterm_protocol::{authentication::SigningKey, codec::Payload};
use std::{future::Future, pin::Pin, sync::Arc};

pub type ClientFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, ClientFailure>> + Send + 'a>>;

pub trait MessageSender: Send + Sync {
    fn send<'a>(&'a self, message: &'a [u8]) -> ClientFuture<'a, ()>;
    fn close(&self) -> ClientFuture<'_, ()>;
    /// Synchronous route invalidation, including cancellation of owned tunnel work.
    /// Dropping a route must have the same invalidation semantics.
    fn abort(&self);
}
pub trait MessageReceiver: Send {
    /// Cancellation preserves partially received framing.
    fn receive(&mut self) -> ClientFuture<'_, Option<Vec<u8>>>;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RouteKind {
    PrivateIpc,
    SecureWebSocket,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PeerFacts {
    pub route: RouteKind,
    pub local_account_verified: bool,
    pub pinned_server_key: Option<[u8; 32]>,
}

pub struct ConnectedRoute {
    pub sender: Arc<dyn MessageSender>,
    pub receiver: Box<dyn MessageReceiver>,
    pub peer: PeerFacts,
}
impl Drop for ConnectedRoute {
    fn drop(&mut self) {
        self.sender.abort();
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ConnectionPurpose {
    Ordinary,
    RestrictedRevocation { operation: OperationId },
}

pub trait RouteConnector: Send + Sync {
    /// Owns a bounded dial and its cancellation cleanup. A failed open resolves
    /// only after all resources it created have finished cleanup. Native adapters
    /// enforce DIAL_DEADLINE for readiness themselves; cleanup may outlast it.
    /// The runtime drains this future on deadline, Retry and shutdown instead of
    /// dropping it. A successful result transfers cleanup to the route's sender.
    fn open<'a>(
        &'a self,
        target: &'a ConnectionTarget,
        purpose: ConnectionPurpose,
    ) -> ClientFuture<'a, ConnectedRoute>;
}

#[derive(Clone)]
pub struct SigningAccess(pub Arc<dyn SigningKey>);
pub trait CredentialSource: Send + Sync {
    fn signing_access<'a>(
        &'a self,
        target: &'a ConnectionTarget,
        purpose: ConnectionPurpose,
    ) -> ClientFuture<'a, SigningAccess>;
}

/// Connection-facing view of the one ClientJournal. Implementations apply its
/// ordinary-use tombstones before returning bindings. No second database is opened.
/// Registration consumes an already verified, durably saved initial/additional
/// Client binding. This port has no pairing allocation or persistence operation.
pub trait ConnectionJournal: Send + Sync {
    fn recover_bindings(&self) -> ClientFuture<'_, Vec<PairedServerBinding>>;
    fn ordinary_use_allowed<'a>(
        &'a self,
        binding: &'a PairedServerBinding,
    ) -> ClientFuture<'a, bool>;
}

pub enum OrganizationPublication {
    Snapshot(Box<SnapshotPart>),
    Delta(OrganizationDelta),
    DeltaPart {
        stamp: PublicationStamp,
        delta: OrganizationDelta,
    },
}

/// Adapts server-core's generic both-major converter to the connection actor.
/// There is deliberately no production JSON or partial-success fallback.
pub trait OrganizationDecoder: Send + Sync {
    fn decode(&self, payload: Payload) -> Result<OrganizationPublication, ClientFailure>;
}

/// Already-authenticated I/O used during journal/cleanup reconciliation. It
/// neither manufactures connection fences nor creates operation identities.
pub trait ProtocolChannel: Send {
    /// Admits one message to the connection's bounded, ordered writer. Completion
    /// is queue admission, not delivery or a durable server outcome. Native
    /// backpressure stays outside receive callbacks; writer failure closes the
    /// attempt. The delegate rechecks authority and durability at final dequeue.
    fn send<'a>(&'a mut self, message: &'a Payload) -> ClientFuture<'a, ()>;
    fn receive(&mut self) -> ClientFuture<'_, Payload>;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OutboundClass {
    Ordinary,
    Lifecycle,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ConnectionInvalidation {
    Reconciliation,
    Publication,
    ConnectionLost,
    Stopped,
    Shutdown,
    BindingRemoved,
}

/// Operation/Pane integration uses the same per-server actor. The owner revokes
/// eligibility synchronously before any loss/stale-state notification. Admission
/// verifies journal durability or exact ephemeral fences, both before queueing
/// and immediately before transmission. Lifecycle reserve is not caller-selected.
pub trait ConnectionDelegate: Send + Sync {
    fn invalidate(&self, binding: &PairedServerBinding, reason: ConnectionInvalidation);
    fn reconcile<'a>(
        &'a self,
        info: ConnectedInfo,
        replica: &'a ReplicaSnapshot,
        channel: &'a mut dyn ProtocolChannel,
    ) -> ClientFuture<'a, ()>;
    fn publication(
        &self,
        info: ConnectedInfo,
        replica: &ReplicaSnapshot,
    ) -> Result<(), ClientFailure>;
    fn admit(&self, info: ConnectedInfo, message: &Payload)
    -> Result<OutboundClass, ClientFailure>;
    fn receive<'a>(
        &'a self,
        info: ConnectedInfo,
        message: Payload,
        channel: &'a mut dyn ProtocolChannel,
    ) -> ClientFuture<'a, ()>;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DiscoveryState {
    Unverified,
    Running,
    Stopped,
}
/// Only injected trusted native discovery may establish deliberate Stop without
/// authenticated management. Missing/refused routes do not implement this port.
pub trait TrustedDiscovery: Send + Sync {
    fn status<'a>(&'a self, target: &'a ConnectionTarget) -> ClientFuture<'a, DiscoveryState>;
}

pub struct ClientServices {
    pub routes: Arc<dyn RouteConnector>,
    pub credentials: Arc<dyn CredentialSource>,
    pub journal: Arc<dyn ConnectionJournal>,
    pub organization: Arc<dyn OrganizationDecoder>,
    pub delegate: Arc<dyn ConnectionDelegate>,
    pub discovery: Option<Arc<dyn TrustedDiscovery>>,
    pub limits: ClientLimits,
    /// Installed Git/directory adapters opt in; declarations alone advertise none.
    pub additional_capabilities: Vec<u32>,
}
