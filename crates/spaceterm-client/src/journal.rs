//! The one durable client journal. Native adapters persist this checked projection;
//! pairing, operations and cleanup share its physical transaction and health latch.

mod controller;
pub(crate) mod model;
mod pairing_projection;
pub use controller::{JournalController, JournalHealth};
pub use pairing_projection::{
    JournalCredentialSlot, JournalEnrollment, JournalEnrollmentAcknowledgement,
    JournalEnrollmentReservation, PairingJournalProjection,
};

use crate::{
    api::{ConnectionTarget, PairedServerBinding, RouteDescription},
    failure::{ClientFailure, ClientFailureClass},
    ports::ClientFuture,
};
use serde::{Deserialize, Serialize};
use spaceterm_core::{
    authority::{GrantedControl, OrganizationRevision},
    identity::{ClientId, DeviceId, OperationStreamId, PairingId, ServerId, WorkspaceId},
    operation_ledger_contracts::{CompactOutcome, LedgerCodec, StreamPurpose},
    operations::{OperationBinding, OperationId},
    pairing::{
        CredentialReference, LocalPairingGeneration, LocalPairingKey, PairingClientStreams,
        PairingReceiptBody, PendingRevocationTask, SignedPairingReceipt,
    },
};
use spaceterm_protocol::authentication::ServerPin;

pub const CLIENT_JOURNAL_SCHEMA: u32 = 1;

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct ClientJournalRevision(#[serde(with = "fixed_u64")] u64);
impl ClientJournalRevision {
    pub const fn new(value: u64) -> Self {
        Self(value)
    }
    pub const fn get(self) -> u64 {
        self.0
    }
    pub fn checked_successor(self) -> Option<Self> {
        self.0.checked_add(1).map(Self)
    }
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct JournalReceipt {
    revision: ClientJournalRevision,
}
impl JournalReceipt {
    pub const fn new(revision: ClientJournalRevision) -> Self {
        Self { revision }
    }
    pub const fn revision(self) -> ClientJournalRevision {
        self.revision
    }
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum JournalFailure {
    Unavailable,
    UnsafeStorage,
    UnknownSchema,
    InvalidState,
    Conflict,
    RecoveryRequired,
}
impl From<JournalFailure> for ClientFailure {
    fn from(value: JournalFailure) -> Self {
        Self::new(match value {
            JournalFailure::Unavailable => ClientFailureClass::StorageUnavailable,
            JournalFailure::UnsafeStorage => ClientFailureClass::UnsafeStorage,
            JournalFailure::UnknownSchema => ClientFailureClass::UnknownSchema,
            JournalFailure::InvalidState | JournalFailure::Conflict => {
                ClientFailureClass::InvalidStorage
            }
            JournalFailure::RecoveryRequired => ClientFailureClass::RecoveryRequired,
        })
    }
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[must_use]
pub enum JournalCommitDisposition {
    Durable(JournalReceipt),
    NotApplied(JournalFailure),
    Indeterminate,
}

pub trait ClientJournal: Send + Sync {
    fn recover(&self) -> ClientFuture<'_, RecoveredClientState>;
    fn commit<'a>(
        &'a self,
        transaction: &'a ClientJournalTransaction,
    ) -> ClientFuture<'a, JournalCommitDisposition>;
}

/// Pairing owns exact-byte verification against the ORIGINAL pin. No default
/// verifier exists, and native recovery must call the same port as enrollment.
pub trait ReceiptVerifier: Send + Sync {
    fn verify(&self, pin: ServerPin, receipt: &SignedPairingReceipt) -> Result<(), ClientFailure>;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct JournalLimits {
    pub saved_servers: usize,
    pub operations_per_stream: usize,
    pub request_bytes_per_stream: usize,
    pub outcome_bytes_per_server: usize,
    pub cleanup_grants: usize,
    pub pending_revocations: usize,
    pub retained_bytes: usize,
}
impl Default for JournalLimits {
    fn default() -> Self {
        Self {
            saved_servers: 64,
            operations_per_stream: 128,
            request_bytes_per_stream: 8 * 1024 * 1024,
            outcome_bytes_per_server: 8 * 1024 * 1024,
            cleanup_grants: 4096,
            pending_revocations: 256,
            retained_bytes: 32 * 1024 * 1024,
        }
    }
}
impl JournalLimits {
    pub fn validate(self) -> Result<Self, ClientFailure> {
        let max = Self::default();
        for (value, ceiling) in [
            (self.saved_servers, max.saved_servers),
            (self.operations_per_stream, max.operations_per_stream),
            (self.request_bytes_per_stream, max.request_bytes_per_stream),
            (self.outcome_bytes_per_server, max.outcome_bytes_per_server),
            (self.cleanup_grants, max.cleanup_grants),
            (self.pending_revocations, max.pending_revocations),
            (self.retained_bytes, max.retained_bytes),
        ] {
            if value == 0 || value > ceiling {
                return Err(crate::failure::resource_limit());
            }
        }
        Ok(self)
    }
}

/// Serialized comparison data only. It contains no signing key or local authority.
#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
pub struct SavedTarget {
    server: ServerId,
    public_key: [u8; 32],
    device: DeviceId,
    pairing: PairingId,
    client: ClientId,
    route: RouteDescription,
    credential: CredentialReference,
    generation: LocalPairingGeneration,
}
impl SavedTarget {
    pub fn from_target(target: &ConnectionTarget) -> Self {
        Self {
            server: target.server(),
            public_key: target.pin().public_key,
            device: target.device(),
            pairing: target.pairing(),
            client: target.client(),
            route: target.route().clone(),
            credential: target.credential(),
            generation: target.incarnation(),
        }
    }
    pub fn target(&self) -> Result<ConnectionTarget, ClientFailure> {
        if self.credential.opaque() == [0; 16] {
            return Err(invalid());
        }
        ConnectionTarget::new(
            ServerPin {
                identity: self.server.bytes(),
                public_key: self.public_key,
            },
            self.device,
            self.pairing,
            self.client,
            self.route.clone(),
            self.credential,
            self.generation,
        )
        .map_err(|_| invalid())
    }
}
impl std::fmt::Debug for SavedTarget {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SavedTarget(..)")
    }
}

#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
pub struct SavedBinding {
    target: SavedTarget,
    streams: PairingClientStreams,
    proof: SignedPairingReceipt,
}
impl SavedBinding {
    pub fn verified(
        binding: PairedServerBinding,
        streams: PairingClientStreams,
        proof: SignedPairingReceipt,
        verifier: &dyn ReceiptVerifier,
    ) -> Result<Self, ClientFailure> {
        let value = Self {
            target: SavedTarget::from_target(binding.target()),
            streams,
            proof,
        };
        value.validate(verifier)?;
        Ok(value)
    }
    pub fn binding(&self) -> Result<PairedServerBinding, ClientFailure> {
        Ok(PairedServerBinding::new(self.target.target()?))
    }
    pub fn streams(&self) -> &PairingClientStreams {
        &self.streams
    }
    pub fn proof(&self) -> &SignedPairingReceipt {
        &self.proof
    }
    pub fn validate(&self, verifier: &dyn ReceiptVerifier) -> Result<(), ClientFailure> {
        let target = self.target.target()?;
        self.streams.validate().map_err(|_| invalid())?;
        self.proof.validate().map_err(|_| invalid())?;
        let (claim, client) = match &self.proof.parts().claims {
            PairingReceiptBody::Enrollment { target, client, .. }
            | PairingReceiptBody::ClientBinding { target, client, .. } => (target.parts(), client),
            _ => return Err(invalid()),
        };
        if claim.server != target.server()
            || claim.pairing != target.pairing()
            || claim.device != target.device()
            || client != &self.streams
            || client.parts().client.id != target.client()
        {
            return Err(invalid());
        }
        verifier
            .verify(target.pin(), &self.proof)
            .map_err(|_| invalid())
    }
}
impl std::fmt::Debug for SavedBinding {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SavedBinding(..)")
    }
}

#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
pub struct JournalStream {
    pub stream: OperationStreamId,
    pub local_pairing: LocalPairingKey,
    pub purpose: StreamPurpose,
    #[serde(with = "fixed_u64")]
    pub high_water: u64,
    #[serde(with = "fixed_u64")]
    pub durable_prefix: u64,
    #[serde(with = "fixed_u64")]
    pub confirmed_prefix: u64,
    pub retired: bool,
}
#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
pub enum RetainedOperationState {
    Pending,
    Completed {
        outcome: CompactOutcome,
        publication_revision: OrganizationRevision,
        consumed: bool,
    },
    Retired,
}
#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
pub struct JournalOperation {
    pub binding: OperationBinding,
    #[serde(deserialize_with = "bounded::request")]
    pub request: Vec<u8>,
    pub workspace: Option<WorkspaceId>,
    pub reserves_grant: bool,
    pub cleanup: bool,
    pub state: RetainedOperationState,
}
impl std::fmt::Debug for JournalOperation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("JournalOperation(..)")
    }
}

/// A cleanup reservation survives outcome acknowledgement and UI consumption.
/// This historical comparison record is never live control authority.
#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
pub struct JournalGrant {
    pub local_pairing: LocalPairingKey,
    pub workspace: WorkspaceId,
    pub control: GrantedControl,
    pub publication_revision: OrganizationRevision,
}
#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
pub struct DetachTask {
    pub local_pairing: LocalPairingKey,
    pub workspace: WorkspaceId,
    #[serde(deserialize_with = "bounded::headers")]
    pub grants: Vec<GrantedControl>,
    #[serde(deserialize_with = "bounded::headers")]
    pub dependencies: Vec<OperationBinding>,
    #[serde(deserialize_with = "bounded::headers")]
    pub releases: Vec<OperationBinding>,
}
#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
pub struct WorkspaceSuppression {
    pub local_pairing: LocalPairingKey,
    pub workspace: WorkspaceId,
}
#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
pub enum RevocationPhase {
    AwaitingConfirmation,
    CleanupPending(Box<SignedPairingReceipt>),
}
#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
pub struct JournalRevocation {
    pub task: PendingRevocationTask,
    pub target: SavedTarget,
    pub phase: RevocationPhase,
    pub notice_dismissed: bool,
    pub attempts: u32,
}
impl std::fmt::Debug for JournalRevocation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("JournalRevocation(..)")
    }
}
#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
struct GenerationHighWater {
    server: ServerId,
    generation: LocalPairingGeneration,
}

#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RecoveredClientState {
    schema: u32,
    revision: ClientJournalRevision,
    #[serde(deserialize_with = "bounded::bindings")]
    bindings: Vec<SavedBinding>,
    #[serde(deserialize_with = "bounded::headers")]
    tombstones: Vec<LocalPairingKey>,
    #[serde(deserialize_with = "bounded::headers")]
    generations: Vec<GenerationHighWater>,
    #[serde(deserialize_with = "bounded::headers")]
    streams: Vec<JournalStream>,
    #[serde(deserialize_with = "bounded::operations")]
    operations: Vec<JournalOperation>,
    #[serde(deserialize_with = "bounded::headers")]
    grants: Vec<JournalGrant>,
    #[serde(deserialize_with = "bounded::headers")]
    suppressed_workspaces: Vec<WorkspaceSuppression>,
    #[serde(deserialize_with = "bounded::headers")]
    detaches: Vec<DetachTask>,
    #[serde(deserialize_with = "bounded::revocations")]
    revocations: Vec<JournalRevocation>,
    #[serde(default)]
    pairing: PairingJournalProjection,
}
impl std::fmt::Debug for RecoveredClientState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("RecoveredClientState(..)")
    }
}
impl RecoveredClientState {
    pub fn empty() -> Self {
        Self {
            schema: CLIENT_JOURNAL_SCHEMA,
            revision: ClientJournalRevision::default(),
            bindings: vec![],
            tombstones: vec![],
            generations: vec![],
            streams: vec![],
            operations: vec![],
            grants: vec![],
            suppressed_workspaces: vec![],
            detaches: vec![],
            revocations: vec![],
            pairing: PairingJournalProjection::default(),
        }
    }
    pub fn pairing(&self) -> &PairingJournalProjection {
        &self.pairing
    }
    pub fn revision(&self) -> ClientJournalRevision {
        self.revision
    }
    pub fn bindings(&self) -> &[SavedBinding] {
        &self.bindings
    }
    pub fn operations(&self) -> &[JournalOperation] {
        &self.operations
    }
    pub fn grants(&self) -> &[JournalGrant] {
        &self.grants
    }
    pub fn streams(&self) -> &[JournalStream] {
        &self.streams
    }
    pub fn detaches(&self) -> &[DetachTask] {
        &self.detaches
    }
    pub fn revocations(&self) -> &[JournalRevocation] {
        &self.revocations
    }
    pub fn tombstones(&self) -> &[LocalPairingKey] {
        &self.tombstones
    }
    pub fn suppressed_workspaces(&self) -> &[WorkspaceSuppression] {
        &self.suppressed_workspaces
    }
    pub fn ordinary_use_allowed(&self, binding: &PairedServerBinding) -> bool {
        binding.target().local_pairing().is_ok_and(|key| {
            !self.tombstones.iter().any(|marker| {
                marker.parts().server == key.parts().server
                    && marker.parts().pairing == key.parts().pairing
            })
        }) && self
            .bindings
            .iter()
            .any(|saved| saved.binding().is_ok_and(|saved| &saved == binding))
    }
    pub fn workspace_barrier(&self, key: &LocalPairingKey, workspace: WorkspaceId) -> bool {
        self.detaches
            .iter()
            .any(|task| &task.local_pairing == key && task.workspace == workspace)
    }
    pub fn operation(&self, id: OperationId) -> Option<&JournalOperation> {
        self.operations
            .iter()
            .find(|record| record.binding.id == id)
    }
    pub fn stream(&self, id: OperationStreamId) -> Option<&JournalStream> {
        self.streams.iter().find(|record| record.stream == id)
    }
    pub fn validate(
        &self,
        codec: &dyn LedgerCodec,
        verifier: &dyn ReceiptVerifier,
        limits: JournalLimits,
    ) -> Result<(), ClientFailure> {
        model::validate(self, codec, verifier, limits)
    }
    pub fn apply(
        &self,
        tx: &ClientJournalTransaction,
        codec: &dyn LedgerCodec,
        verifier: &dyn ReceiptVerifier,
        limits: JournalLimits,
    ) -> Result<Self, ClientFailure> {
        model::apply(self, tx, codec, verifier, limits)
    }
}

#[derive(Clone)]
pub struct ClientJournalTransaction {
    pub expected_revision: ClientJournalRevision,
    pub changes: Vec<ClientJournalChange>,
}
#[derive(Clone)]
pub enum ClientJournalChange {
    ReserveCredential(JournalCredentialSlot),
    RecordCredentialPublicKey {
        reference: CredentialReference,
        public_key: [u8; 32],
    },
    PrepareEnrollment(Box<JournalEnrollment>),
    SaveEnrollment {
        binding: Box<SavedBinding>,
        acknowledgement: Box<JournalEnrollmentAcknowledgement>,
    },
    FinishEnrollmentAcknowledgement(OperationId),
    SaveBinding(Box<SavedBinding>),
    Prepare(JournalOperation),
    Complete {
        binding: OperationBinding,
        outcome: CompactOutcome,
        publication_revision: OrganizationRevision,
    },
    Retire(OperationBinding),
    ConfirmAcknowledgement {
        stream: OperationStreamId,
        through: u64,
    },
    ConsumeResult(OperationId),
    ReconcileGrants {
        local_pairing: LocalPairingKey,
        epoch: spaceterm_core::authority::ServerEpoch,
        revision: OrganizationRevision,
        grants: Vec<(WorkspaceId, GrantedControl)>,
    },
    BeginDetach(DetachTask),
    AddDetachRelease {
        local_pairing: LocalPairingKey,
        workspace: WorkspaceId,
        operation: OperationBinding,
    },
    FinishDetach {
        local_pairing: LocalPairingKey,
        workspace: WorkspaceId,
    },
    BeginRevocation(Box<JournalRevocation>),
    DismissRevocation(OperationId),
    RevocationAttempt(OperationId),
    ConfirmRevocation {
        operation: OperationId,
        receipt: Box<SignedPairingReceipt>,
    },
    ConfirmRevocationAcknowledgement(OperationId),
    FinishCredentialCleanup(OperationId),
}

pub(crate) fn invalid() -> ClientFailure {
    ClientFailure::new(ClientFailureClass::InvalidStorage)
}
pub(crate) mod fixed_u64 {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};
    pub fn serialize<S: Serializer>(value: &u64, serializer: S) -> Result<S::Ok, S::Error> {
        value.to_be_bytes().serialize(serializer)
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<u64, D::Error> {
        <[u8; 8]>::deserialize(deserializer).map(u64::from_be_bytes)
    }
}

mod bounded {
    use serde::{
        Deserialize, Deserializer,
        de::{Error, IgnoredAny, SeqAccess, Visitor},
    };
    use std::{fmt, marker::PhantomData};
    fn sequence<'de, D: Deserializer<'de>, T: Deserialize<'de>, const MAX: usize>(
        deserializer: D,
    ) -> Result<Vec<T>, D::Error> {
        struct Bounded<T, const MAX: usize>(PhantomData<T>);
        impl<'de, T: Deserialize<'de>, const MAX: usize> Visitor<'de> for Bounded<T, MAX> {
            type Value = Vec<T>;
            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("a bounded journal sequence")
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut sequence: A) -> Result<Vec<T>, A::Error> {
                let mut values = Vec::new();
                while values.len() < MAX {
                    let Some(value) = sequence.next_element()? else {
                        return Ok(values);
                    };
                    values
                        .try_reserve(1)
                        .map_err(|_| A::Error::custom("journal allocation limit"))?;
                    values.push(value);
                }
                if sequence.next_element::<IgnoredAny>()?.is_some() {
                    return Err(A::Error::custom("journal sequence limit"));
                }
                Ok(values)
            }
        }
        deserializer.deserialize_seq(Bounded::<T, MAX>(PhantomData))
    }
    pub fn bindings<'de, D: Deserializer<'de>, T: Deserialize<'de>>(
        d: D,
    ) -> Result<Vec<T>, D::Error> {
        sequence::<D, T, 64>(d)
    }
    pub fn headers<'de, D: Deserializer<'de>, T: Deserialize<'de>>(
        d: D,
    ) -> Result<Vec<T>, D::Error> {
        sequence::<D, T, 4096>(d)
    }
    pub fn operations<'de, D: Deserializer<'de>, T: Deserialize<'de>>(
        d: D,
    ) -> Result<Vec<T>, D::Error> {
        sequence::<D, T, 16384>(d)
    }
    pub fn revocations<'de, D: Deserializer<'de>, T: Deserialize<'de>>(
        d: D,
    ) -> Result<Vec<T>, D::Error> {
        sequence::<D, T, 1024>(d)
    }
    pub fn request<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<u8>, D::Error> {
        sequence::<D, u8, 65536>(d)
    }
}
