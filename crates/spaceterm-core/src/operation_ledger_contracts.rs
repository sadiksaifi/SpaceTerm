//! Shared ledger data and checked value constructors.
//!
//! Projection and recovery records are comparison data, not execution authority.
//! Deserialization of a raw record does not install it as live ledger state.
//! The ledger and store validate the complete proposed or recovered bundle.

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::authority::{OrganizationRevision, StoreRevision};
use crate::identity::{ClientId, OperationStreamId, PairingId, ServerId};
use crate::operations::AssessmentTarget;
use crate::operations::{
    Command, OperationBinding, OperationId, Outcome, RepositoryScope, RequestDigest,
};
use crate::retirement::RetirementDescriptor;

/// Version of the complete ledger projection/recovery representation.
pub const LEDGER_FORMAT_VERSION: u32 = 1;
/// Version of the persisted slot and worst-case byte accounting policy.
pub const LEDGER_QUOTA_POLICY_VERSION: u32 = 1;
/// Binary request format. This is not operations::RequestDigest's JSON v1.
pub const CANONICAL_REQUEST_FORMAT_VERSION: u32 = 1;
/// Version of the ledger owner's compact Outcome encoding.
pub const COMPACT_OUTCOME_FORMAT_VERSION: u32 = 1;
/// Maximum complete canonical request encoding, including its binding header.
pub const MAX_CANONICAL_REQUEST_BYTES: usize = 64 * 1024;
/// Admission maximum, not the larger storage/reservation ceiling.
pub const MAX_COMPACT_OUTCOME_BYTES: usize = 60 * 1024;
/// Charged until cumulative acknowledgement, regardless of actual result size.
pub const OUTCOME_RESERVATION_BYTES: u32 = 64 * 1024;
/// Bound on a projection's write inventory before allocation or interpretation.
pub const MAX_LEDGER_PROJECTION_WRITES: usize = 8192;
/// A binary ledger request cannot be mistaken for the legacy JSON digest domain.
pub const CANONICAL_REQUEST_DOMAIN: &[u8] = b"spaceterm-operation-ledger/request\0";
/// Compact outcomes have a distinct domain and carry their format version.
pub const COMPACT_OUTCOME_DOMAIN: &[u8] = b"spaceterm-operation-ledger/outcome\0";

/// Typed failures contain no requests, digests, paths, outcome bytes, or secrets.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum LedgerFailureClass {
    ScopeMismatch,
    UnknownStream,
    StreamLimit,
    SequenceGap,
    IdentityReused,
    Retired,
    QuotaExceeded,
    PreparationBusy,
    AcknowledgementAhead,
    AcknowledgementGap,
    CounterExhausted,
    InvalidRequest,
    InvalidCompactOutcome,
    InvalidTransition,
    InvalidExternalProgress,
    OutcomeRevisionMismatch,
    InvalidDurableState,
    RecoveryRequired,
    AdmissionsClosed,
    PurposeMismatch,
    UnsupportedFormat,
}

/// Numeric quota scope; this never identifies a native path or credential.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum LedgerQuotaScope {
    Server,
    ClientBinding,
    Stream,
}

/// Closed resource classes used in diagnostics and admission failures.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum LedgerResourceKind {
    StreamHeaders,
    Slots,
    ReservedBytes,
    RequestBytes,
    OutcomeBytes,
    ProjectionWrites,
}

/// Fixed-size diagnostic metadata. None means the number is not applicable.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct LedgerFailureMetadata {
    pub expected: Option<u64>,
    pub actual: Option<u64>,
    pub limit: Option<u64>,
    pub scope: Option<LedgerQuotaScope>,
    pub resource: Option<LedgerResourceKind>,
}

/// A ledger failure with bounded, content-free metadata.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LedgerFailure {
    pub class: LedgerFailureClass,
    pub metadata: LedgerFailureMetadata,
}
impl LedgerFailure {
    pub fn new(class: LedgerFailureClass) -> Self {
        Self {
            class,
            metadata: LedgerFailureMetadata::default(),
        }
    }
    pub const fn with_metadata(class: LedgerFailureClass, metadata: LedgerFailureMetadata) -> Self {
        Self { class, metadata }
    }
}
impl fmt::Display for LedgerFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "operation ledger failure: {:?}", self.class)
    }
}
impl std::error::Error for LedgerFailure {}

/// Immutable stream comparison data. It is not an authenticated principal.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct StreamBinding(OperationStreamId);
impl StreamBinding {
    pub fn new(
        server: ServerId,
        pairing: PairingId,
        client: ClientId,
        stream: OperationStreamId,
    ) -> Result<Self, LedgerFailure> {
        if stream.server() != server || stream.pairing() != pairing || stream.client() != client {
            return Err(LedgerFailure::new(LedgerFailureClass::ScopeMismatch));
        }
        Ok(Self(stream))
    }
    pub const fn from_stream(stream: OperationStreamId) -> Self {
        Self(stream)
    }
    pub const fn server(self) -> ServerId {
        self.0.server()
    }
    pub const fn pairing(self) -> PairingId {
        self.0.pairing()
    }
    pub const fn client(self) -> ClientId {
        self.0.client()
    }
    pub const fn stream(self) -> OperationStreamId {
        self.0
    }
    pub fn check_operation(self, operation: OperationId) -> Result<(), LedgerFailure> {
        if operation.stream() != self.stream() {
            return Err(LedgerFailure::new(LedgerFailureClass::ScopeMismatch));
        }
        Ok(())
    }
}

/// Validated ceilings. Smaller policies support focused tests, not larger servers.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(try_from = "[u32; 6]", into = "[u32; 6]")]
pub struct LedgerLimits {
    max_streams: u32,
    max_streams_per_client_binding: u32,
    per_stream_slots: u32,
    global_slots: u32,
    per_stream_reserved_slots: u32,
    global_reserved_slots: u32,
}
impl LedgerLimits {
    pub const fn production_v1() -> Self {
        Self {
            max_streams: 4096,
            max_streams_per_client_binding: 16,
            per_stream_slots: 64,
            global_slots: 1024,
            per_stream_reserved_slots: 8,
            global_reserved_slots: 128,
        }
    }
    pub fn new(
        max_streams: u32,
        max_streams_per_client_binding: u32,
        per_stream_slots: u32,
        global_slots: u32,
        per_stream_reserved_slots: u32,
        global_reserved_slots: u32,
    ) -> Result<Self, LedgerFailure> {
        if max_streams == 0
            || max_streams > 4096
            || max_streams_per_client_binding == 0
            || max_streams_per_client_binding > 16
            || max_streams_per_client_binding > max_streams
            || per_stream_slots == 0
            || per_stream_slots > 64
            || global_slots == 0
            || global_slots > 1024
            || per_stream_slots > global_slots
            || per_stream_reserved_slots > 8
            || per_stream_reserved_slots >= per_stream_slots
            || global_reserved_slots > 128
            || global_reserved_slots >= global_slots
        {
            return Err(LedgerFailure::new(LedgerFailureClass::InvalidDurableState));
        }
        Ok(Self {
            max_streams,
            max_streams_per_client_binding,
            per_stream_slots,
            global_slots,
            per_stream_reserved_slots,
            global_reserved_slots,
        })
    }
    pub const fn max_streams(self) -> u32 {
        self.max_streams
    }
    pub const fn max_streams_per_client_binding(self) -> u32 {
        self.max_streams_per_client_binding
    }
    pub const fn per_stream_slots(self) -> u32 {
        self.per_stream_slots
    }
    pub const fn global_slots(self) -> u32 {
        self.global_slots
    }
    pub const fn per_stream_reserved_slots(self) -> u32 {
        self.per_stream_reserved_slots
    }
    pub const fn global_reserved_slots(self) -> u32 {
        self.global_reserved_slots
    }
    pub const fn ordinary_stream_slots(self) -> u32 {
        self.per_stream_slots - self.per_stream_reserved_slots
    }
    pub const fn ordinary_global_slots(self) -> u32 {
        self.global_slots - self.global_reserved_slots
    }
    pub const fn per_stream_reserved_byte_ceiling(self) -> u64 {
        self.per_stream_slots as u64 * OUTCOME_RESERVATION_BYTES as u64
    }
    pub const fn global_reserved_byte_ceiling(self) -> u64 {
        self.global_slots as u64 * OUTCOME_RESERVATION_BYTES as u64
    }
}
impl TryFrom<[u32; 6]> for LedgerLimits {
    type Error = LedgerFailure;
    fn try_from(v: [u32; 6]) -> Result<Self, Self::Error> {
        Self::new(v[0], v[1], v[2], v[3], v[4], v[5])
    }
}
impl From<LedgerLimits> for [u32; 6] {
    fn from(v: LedgerLimits) -> Self {
        [
            v.max_streams,
            v.max_streams_per_client_binding,
            v.per_stream_slots,
            v.global_slots,
            v.per_stream_reserved_slots,
            v.global_reserved_slots,
        ]
    }
}

/// Stream purpose is permanent comparison data, never protocol authorization.
/// Enrollment binds the prospective stream to the pairing owner's canonical,
/// nonsecret ticket/claim binding digest. Its prospective IDs are not active IDs.
#[derive(Clone, Copy, Deserialize, Eq, PartialEq, Serialize)]
pub enum StreamPurpose {
    Ordinary,
    SelfRevocation,
    Enrollment { claim_digest: RequestDigest },
}

/// Admission class is derived by the server's exhaustive Command classification.
/// Dedicated classes cannot be used by ordinary or unrelated safety operations.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum ReservationClass {
    Ordinary,
    Safety,
    DedicatedSelfRevocation,
    DedicatedEnrollment,
}

/// A checked, fixed worst-case outcome reservation, not a current result length.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(try_from = "(ReservationClass, u32)", into = "(ReservationClass, u32)")]
pub struct OutcomeReservation {
    class: ReservationClass,
    bytes: u32,
}
impl OutcomeReservation {
    pub fn new(class: ReservationClass, bytes: u32) -> Result<Self, LedgerFailure> {
        if bytes != OUTCOME_RESERVATION_BYTES {
            return Err(LedgerFailure::new(LedgerFailureClass::InvalidDurableState));
        }
        Ok(Self { class, bytes })
    }
    pub const fn full(class: ReservationClass) -> Self {
        Self {
            class,
            bytes: OUTCOME_RESERVATION_BYTES,
        }
    }
    pub const fn class(self) -> ReservationClass {
        self.class
    }
    pub const fn bytes(self) -> u32 {
        self.bytes
    }
}
impl TryFrom<(ReservationClass, u32)> for OutcomeReservation {
    type Error = LedgerFailure;
    fn try_from(v: (ReservationClass, u32)) -> Result<Self, Self::Error> {
        Self::new(v.0, v.1)
    }
}
impl From<OutcomeReservation> for (ReservationClass, u32) {
    fn from(v: OutcomeReservation) -> Self {
        (v.class, v.bytes)
    }
}

/// Permanent stream termination never acknowledges or evicts a completed result.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum StreamRetirementReason {
    ExplicitRetirement,
    Expired,
    PairingRevoked,
    SequenceExhausted,
}

/// A retired header stays in recovery even when its acknowledged prefix is empty.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum StreamStatus {
    Active,
    Retired(StreamRetirementReason),
}

/// Raw bounded-projection/recovery header, not live state.
/// Restricted streams are one-shot: sequence 1 consumes their dormant reservation.
/// The consumed reservation moves to the retained entry without being charged twice.
#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
pub struct StreamRecord {
    pub binding: StreamBinding,
    pub purpose: StreamPurpose,
    pub status: StreamStatus,
    pub admitted_through: u64,
    pub acknowledged_through: u64,
    pub dormant_reservation: Option<OutcomeReservation>,
    pub request_format_version: u32,
    pub outcome_format_version: u32,
    pub quota_policy_version: u32,
}
impl StreamRecord {
    pub fn new(
        binding: StreamBinding,
        purpose: StreamPurpose,
        status: StreamStatus,
        admitted_through: u64,
        acknowledged_through: u64,
        dormant_reservation: Option<OutcomeReservation>,
    ) -> Result<Self, LedgerFailure> {
        let v = Self {
            binding,
            purpose,
            status,
            admitted_through,
            acknowledged_through,
            dormant_reservation,
            request_format_version: CANONICAL_REQUEST_FORMAT_VERSION,
            outcome_format_version: COMPACT_OUTCOME_FORMAT_VERSION,
            quota_policy_version: LEDGER_QUOTA_POLICY_VERSION,
        };

        Ok(v)
    }
    pub fn validate_shape(&self) -> Result<(), LedgerFailure> {
        if self.acknowledged_through > self.admitted_through
            || self.request_format_version != CANONICAL_REQUEST_FORMAT_VERSION
            || self.outcome_format_version != COMPACT_OUTCOME_FORMAT_VERSION
            || self.quota_policy_version != LEDGER_QUOTA_POLICY_VERSION
        {
            return Err(LedgerFailure::new(LedgerFailureClass::InvalidDurableState));
        }
        let dedicated = match self.purpose {
            StreamPurpose::Ordinary => {
                if self.dormant_reservation.is_some() {
                    return Err(LedgerFailure::new(LedgerFailureClass::PurposeMismatch));
                }
                return Ok(());
            }
            StreamPurpose::SelfRevocation => ReservationClass::DedicatedSelfRevocation,
            StreamPurpose::Enrollment { .. } => ReservationClass::DedicatedEnrollment,
        };
        if self.admitted_through > 1
            || (self.admitted_through == 0
                && self.dormant_reservation != Some(OutcomeReservation::full(dedicated)))
            || (self.admitted_through == 1 && self.dormant_reservation.is_some())
        {
            return Err(LedgerFailure::new(LedgerFailureClass::PurposeMismatch));
        }
        Ok(())
    }
}

/// A separately versioned binary identity-content binding for new ledger users.
/// Existing operations::OperationRequest and its JSON digest remain unchanged.
#[derive(Clone, Eq, PartialEq)]
pub struct CanonicalOperationRequest {
    binding: OperationBinding,
    command: Command,
}
impl CanonicalOperationRequest {
    pub fn new(
        id: OperationId,
        command: Command,
        codec: &dyn LedgerCodec,
    ) -> Result<Self, LedgerFailure> {
        command
            .validate_scope(id.stream())
            .map_err(|_| LedgerFailure::new(LedgerFailureClass::ScopeMismatch))?;
        if matches!(&command, Command::BeginStartup(intent) if intent.id() != id) {
            return Err(LedgerFailure::new(LedgerFailureClass::ScopeMismatch));
        }
        let digest = codec.canonical_request_digest(
            &StreamBinding::from_stream(id.stream()),
            id,
            &command,
        )?;
        Ok(Self {
            binding: OperationBinding { id, digest },
            command,
        })
    }
    pub const fn binding(&self) -> OperationBinding {
        self.binding
    }
    pub fn command(&self) -> &Command {
        &self.command
    }
    pub fn encode(&self, codec: &dyn LedgerCodec) -> Result<Vec<u8>, LedgerFailure> {
        codec.encode_canonical_request(
            &StreamBinding::from_stream(self.binding.id.stream()),
            self.binding.id,
            &self.command,
        )
    }
    pub fn decode(bytes: &[u8], codec: &dyn LedgerCodec) -> Result<Self, LedgerFailure> {
        if bytes.len() > MAX_CANONICAL_REQUEST_BYTES {
            return Err(LedgerFailure::new(LedgerFailureClass::InvalidRequest));
        }
        codec.decode_canonical_request(bytes)
    }
    pub fn verify_binding(&self, binding: OperationBinding) -> Result<(), LedgerFailure> {
        if self.binding != binding {
            return Err(LedgerFailure::new(LedgerFailureClass::IdentityReused));
        }
        Ok(())
    }
}

/// Bounded compact bytes. Semantic validation requires the canonical codec port.
/// Encoding uses the ledger owner's codec; raw decoding checks shape only.
#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(try_from = "Vec<u8>", into = "Vec<u8>")]
pub struct CompactOutcome(Vec<u8>);
impl CompactOutcome {
    pub fn new(outcome: &Outcome, codec: &dyn LedgerCodec) -> Result<Self, LedgerFailure> {
        let bytes = codec.encode_compact_outcome(outcome)?;
        Self::from_encoded(bytes)
    }
    pub fn from_encoded(bytes: Vec<u8>) -> Result<Self, LedgerFailure> {
        if bytes.is_empty() || bytes.len() > MAX_COMPACT_OUTCOME_BYTES {
            return Err(LedgerFailure::new(
                LedgerFailureClass::InvalidCompactOutcome,
            ));
        }
        Ok(Self(bytes))
    }
    pub fn outcome(&self, codec: &dyn LedgerCodec) -> Result<Outcome, LedgerFailure> {
        codec.decode_compact_outcome(&self.0)
    }
    pub fn as_encoded(&self) -> &[u8] {
        &self.0
    }
    pub fn encoded_len(&self) -> usize {
        self.0.len()
    }
}
impl TryFrom<Vec<u8>> for CompactOutcome {
    type Error = LedgerFailure;
    fn try_from(bytes: Vec<u8>) -> Result<Self, Self::Error> {
        Self::from_encoded(bytes)
    }
}
impl From<CompactOutcome> for Vec<u8> {
    fn from(v: CompactOutcome) -> Self {
        v.0
    }
}

/// External classes have separate owning reconciliation paths.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum ExternalEffectKind {
    GitWrite,
    SessionLifecycle,
    ServerManagement,
}

/// Only recovered AtomicMutation can obtain a matching-request resumed permit.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum PendingKind {
    AtomicMutation,
    External { effect: ExternalEffectKind },
}

/// Comparison phases mirror the owning durable intent, not native observations.
/// Started remains distinct from ExternalFinished after a crash.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum ExternalIntentPhase {
    Prepared,
    Started,
    ExternalFinished,
}

/// Typed lookup scope for an owning intent. No native authority is retained here.
#[derive(Clone, Copy, Deserialize, Eq, PartialEq, Serialize)]
pub enum ExternalIntentScope {
    Git(RepositoryScope),
    Management,
}

/// Checked reference to a same-operation durable Git or management prerequisite.
/// Its full owner payload must exist in the same typed recovery bundle.
#[derive(Clone, Copy, Deserialize, Eq, PartialEq, Serialize)]
#[serde(
    try_from = "ExternalIntentReferenceParts",
    into = "ExternalIntentReferenceParts"
)]
pub struct ExternalIntentReference {
    binding: OperationBinding,
    scope: ExternalIntentScope,
    phase: ExternalIntentPhase,
    recorded_at: StoreRevision,
}
/// Untrusted persisted/reference-construction input.
#[derive(Clone, Copy, Deserialize, Eq, PartialEq, Serialize)]
pub struct ExternalIntentReferenceParts {
    pub binding: OperationBinding,
    pub scope: ExternalIntentScope,
    pub phase: ExternalIntentPhase,
    pub recorded_at: StoreRevision,
}
impl ExternalIntentReference {
    pub fn new(parts: ExternalIntentReferenceParts) -> Result<Self, LedgerFailure> {
        if parts.recorded_at.get() == 0 {
            return Err(LedgerFailure::new(
                LedgerFailureClass::InvalidExternalProgress,
            ));
        }
        if let ExternalIntentScope::Git(scope) = parts.scope {
            scope
                .validate()
                .map_err(|_| LedgerFailure::new(LedgerFailureClass::ScopeMismatch))?;
            if scope.server != parts.binding.id.stream().server() {
                return Err(LedgerFailure::new(LedgerFailureClass::ScopeMismatch));
            }
        }
        Ok(Self {
            binding: parts.binding,
            scope: parts.scope,
            phase: parts.phase,
            recorded_at: parts.recorded_at,
        })
    }
    pub const fn binding(self) -> OperationBinding {
        self.binding
    }
    pub const fn scope(self) -> ExternalIntentScope {
        self.scope
    }
    pub const fn phase(self) -> ExternalIntentPhase {
        self.phase
    }
    pub const fn recorded_at(self) -> StoreRevision {
        self.recorded_at
    }
    pub const fn effect(self) -> ExternalEffectKind {
        match self.scope {
            ExternalIntentScope::Git(_) => ExternalEffectKind::GitWrite,
            ExternalIntentScope::Management => ExternalEffectKind::ServerManagement,
        }
    }
}
impl TryFrom<ExternalIntentReferenceParts> for ExternalIntentReference {
    type Error = LedgerFailure;
    fn try_from(parts: ExternalIntentReferenceParts) -> Result<Self, Self::Error> {
        Self::new(parts)
    }
}
impl From<ExternalIntentReference> for ExternalIntentReferenceParts {
    fn from(v: ExternalIntentReference) -> Self {
        Self {
            binding: v.binding,
            scope: v.scope,
            phase: v.phase,
            recorded_at: v.recorded_at,
        }
    }
}

/// Data-only external progress. AwaitingIntent grants no external-effect authority.
/// Git progress can also retain its internal non-deleting retirement acceptance.
#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
pub enum ExternalProgressState {
    AwaitingIntent,
    AcceptedRetirement(Box<RetirementDescriptor>),
    RecordedIntent {
        reference: ExternalIntentReference,
        retirement: Option<Box<RetirementDescriptor>>,
    },
}

/// Invariant-checked progress preserving the original operation and digest.
#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(
    try_from = "PendingExternalProgressParts",
    into = "PendingExternalProgressParts"
)]
pub struct PendingExternalProgress {
    binding: OperationBinding,
    effect: ExternalEffectKind,
    state: ExternalProgressState,
}
/// Untrusted persisted/construction input, validated before live installation.
#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
pub struct PendingExternalProgressParts {
    pub binding: OperationBinding,
    pub effect: ExternalEffectKind,
    pub state: ExternalProgressState,
}
impl PendingExternalProgress {
    pub fn new(parts: PendingExternalProgressParts) -> Result<Self, LedgerFailure> {
        let check_retirement = |v: &RetirementDescriptor| {
            if v.binding() != parts.binding {
                return Err(LedgerFailure::new(
                    LedgerFailureClass::InvalidExternalProgress,
                ));
            }
            Ok(())
        };
        match &parts.state {
            ExternalProgressState::AwaitingIntent => {}
            ExternalProgressState::AcceptedRetirement(v) => {
                if parts.effect != ExternalEffectKind::SessionLifecycle
                    || matches!(v.target(), AssessmentTarget::RemoveWorktree { .. })
                {
                    return Err(LedgerFailure::new(
                        LedgerFailureClass::InvalidExternalProgress,
                    ));
                }
                check_retirement(v)?;
            }
            ExternalProgressState::RecordedIntent {
                reference,
                retirement,
            } => {
                if reference.binding() != parts.binding || reference.effect() != parts.effect {
                    return Err(LedgerFailure::new(
                        LedgerFailureClass::InvalidExternalProgress,
                    ));
                }
                if let Some(v) = retirement {
                    check_retirement(v)?;
                    if reference.phase() == ExternalIntentPhase::Prepared {
                        return Err(LedgerFailure::new(
                            LedgerFailureClass::InvalidExternalProgress,
                        ));
                    }
                    match (reference.scope(), v.target()) {
                        (
                            ExternalIntentScope::Git(repository),
                            AssessmentTarget::RemoveWorktree { scope, .. },
                        ) if repository == scope => {}
                        _ => {
                            return Err(LedgerFailure::new(
                                LedgerFailureClass::InvalidExternalProgress,
                            ));
                        }
                    }
                }
            }
        }
        Ok(Self {
            binding: parts.binding,
            effect: parts.effect,
            state: parts.state,
        })
    }
    pub const fn binding(&self) -> OperationBinding {
        self.binding
    }
    pub const fn effect(&self) -> ExternalEffectKind {
        self.effect
    }
    pub fn state(&self) -> &ExternalProgressState {
        &self.state
    }
}
impl TryFrom<PendingExternalProgressParts> for PendingExternalProgress {
    type Error = LedgerFailure;
    fn try_from(parts: PendingExternalProgressParts) -> Result<Self, Self::Error> {
        Self::new(parts)
    }
}
impl From<PendingExternalProgress> for PendingExternalProgressParts {
    fn from(v: PendingExternalProgress) -> Self {
        Self {
            binding: v.binding,
            effect: v.effect,
            state: v.state,
        }
    }
}

/// A raw pending projection/recovery record, not an ExecutionPermit.
/// No request body is retained by the server ledger.
#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
pub struct PendingOperationRecord {
    pub binding: OperationBinding,
    pub kind: PendingKind,
    pub reservation: OutcomeReservation,
    pub admitted_at: StoreRevision,
    pub admission_organization_revision: OrganizationRevision,
    pub progress_recorded_at: StoreRevision,
    pub external_progress: Option<PendingExternalProgress>,
}
impl PendingOperationRecord {
    pub fn validate_shape(&self) -> Result<(), LedgerFailure> {
        if self.admitted_at.get() == 0 || self.progress_recorded_at < self.admitted_at {
            return Err(LedgerFailure::new(LedgerFailureClass::InvalidDurableState));
        }
        match (self.kind, self.external_progress.as_ref()) {
            (PendingKind::AtomicMutation, None)
                if self.progress_recorded_at == self.admitted_at =>
            {
                Ok(())
            }
            (PendingKind::External { effect }, Some(progress))
                if progress.binding() == self.binding && progress.effect() == effect =>
            {
                Ok(())
            }
            _ => Err(LedgerFailure::new(
                LedgerFailureClass::InvalidExternalProgress,
            )),
        }
    }
}

/// Checked operation-owned reduction comparison basis.
/// It grants no reducer authority. The same transaction must contain the owning
/// reducer projection with this exact basis, including for a no-change conflict.
#[derive(Clone, Copy, Deserialize, Eq, PartialEq, Serialize)]
#[serde(
    try_from = "OperationReductionBasisParts",
    into = "OperationReductionBasisParts"
)]
pub struct OperationReductionBasis {
    binding: OperationBinding,
    base: OrganizationRevision,
    resulting: OrganizationRevision,
}
/// Untrusted comparison input, not a commit or reduction receipt.
#[derive(Clone, Copy, Deserialize, Eq, PartialEq, Serialize)]
pub struct OperationReductionBasisParts {
    pub binding: OperationBinding,
    pub base: OrganizationRevision,
    pub resulting: OrganizationRevision,
}
impl OperationReductionBasis {
    pub fn new(parts: OperationReductionBasisParts) -> Result<Self, LedgerFailure> {
        if parts.resulting != parts.base && parts.base.checked_successor() != Some(parts.resulting)
        {
            return Err(LedgerFailure::new(
                LedgerFailureClass::OutcomeRevisionMismatch,
            ));
        }
        Ok(Self {
            binding: parts.binding,
            base: parts.base,
            resulting: parts.resulting,
        })
    }
    pub const fn binding(self) -> OperationBinding {
        self.binding
    }
    pub const fn base(self) -> OrganizationRevision {
        self.base
    }
    pub const fn resulting(self) -> OrganizationRevision {
        self.resulting
    }
}
impl TryFrom<OperationReductionBasisParts> for OperationReductionBasis {
    type Error = LedgerFailure;
    fn try_from(parts: OperationReductionBasisParts) -> Result<Self, Self::Error> {
        Self::new(parts)
    }
}
impl From<OperationReductionBasis> for OperationReductionBasisParts {
    fn from(v: OperationReductionBasis) -> Self {
        Self {
            binding: v.binding,
            base: v.base,
            resulting: v.resulting,
        }
    }
}

/// The current commit revision and the operation's result revision are distinct.
/// Store-only contexts cannot claim an operation-owned organization reduction.
#[derive(Clone, Copy, Eq, PartialEq)]
pub struct TransactionContext {
    expected_store: StoreRevision,
    base_organization: OrganizationRevision,
    resulting_organization: OrganizationRevision,
    operation_basis: Option<OperationReductionBasis>,
}
impl TransactionContext {
    pub fn new(
        expected_store: StoreRevision,
        base_organization: OrganizationRevision,
        resulting_organization: OrganizationRevision,
        operation_basis: Option<OperationReductionBasis>,
    ) -> Result<Self, LedgerFailure> {
        if expected_store.checked_successor().is_none() {
            return Err(LedgerFailure::new(LedgerFailureClass::CounterExhausted));
        }
        if resulting_organization != base_organization
            && base_organization.checked_successor() != Some(resulting_organization)
        {
            return Err(LedgerFailure::new(
                LedgerFailureClass::OutcomeRevisionMismatch,
            ));
        }
        if operation_basis.is_some_and(|v| {
            v.base() != base_organization || v.resulting() != resulting_organization
        }) {
            return Err(LedgerFailure::new(
                LedgerFailureClass::OutcomeRevisionMismatch,
            ));
        }
        Ok(Self {
            expected_store,
            base_organization,
            resulting_organization,
            operation_basis,
        })
    }
    pub const fn expected_store(self) -> StoreRevision {
        self.expected_store
    }
    pub const fn base_organization(self) -> OrganizationRevision {
        self.base_organization
    }
    pub const fn resulting_organization(self) -> OrganizationRevision {
        self.resulting_organization
    }
    pub const fn operation_basis(self) -> Option<OperationReductionBasis> {
        self.operation_basis
    }
}

/// Completion either retains a previously recorded acceptance revision or uses
/// its own final reduction. Current unrelated organization revision is not a basis.
#[derive(Clone, Copy, Deserialize, Eq, PartialEq, Serialize)]
pub enum CompletionRevisionBasis {
    RecordedProgress,
    OwnedReduction(OperationReductionBasis),
}

/// Raw completed projection/recovery data. The result remains historical after
/// deletion of every identity it mentions. Its reservation survives retirement.
#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
pub struct CompletedOperationRecord {
    pub binding: OperationBinding,
    pub reservation: OutcomeReservation,
    pub admitted_at: StoreRevision,
    pub completed_at: StoreRevision,
    pub outcome: CompactOutcome,
    pub outcome_revision: OrganizationRevision,
}
impl CompletedOperationRecord {
    pub fn validate_shape(&self, codec: &dyn LedgerCodec) -> Result<(), LedgerFailure> {
        if self.admitted_at.get() == 0 || self.completed_at <= self.admitted_at {
            return Err(LedgerFailure::new(LedgerFailureClass::InvalidDurableState));
        }
        self.outcome
            .outcome(codec)?
            .validate_scope(self.binding.id.stream())
            .map_err(|_| LedgerFailure::new(LedgerFailureClass::ScopeMismatch))
    }
}

/// Raw typed writes for the one core durability boundary.
/// Validators compare original binding, reservation, phases, prefixes, revisions,
/// and owning reducer/intent projections against committed state.
#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
pub enum LedgerWrite {
    RegisterStream(StreamRecord),
    AdmitOperation(PendingOperationRecord),
    RecordExternalProgress {
        previous_progress_store_revision: StoreRevision,
        record: PendingOperationRecord,
    },
    CompleteOperation {
        record: CompletedOperationRecord,
        basis: CompletionRevisionBasis,
    },
    AcknowledgePrefix {
        binding: StreamBinding,
        previous: u64,
        through: u64,
    },
    RetireStream {
        binding: StreamBinding,
        reason: StreamRetirementReason,
    },
}

/// A bounded proposal, never an execution permit or durable receipt.
/// projection::append_to composes its writes into TransactionParts before the
/// checked DurableTransaction is constructed.
#[derive(Clone, Eq, PartialEq)]
pub struct LedgerProjection {
    context: TransactionContext,
    resulting_store: StoreRevision,
    writes: Vec<LedgerWrite>,
}
impl LedgerProjection {
    pub fn new(
        context: TransactionContext,
        writes: Vec<LedgerWrite>,
    ) -> Result<Self, LedgerFailure> {
        if writes.is_empty() || writes.len() > MAX_LEDGER_PROJECTION_WRITES {
            return Err(LedgerFailure::new(LedgerFailureClass::InvalidTransition));
        }
        let resulting_store = context
            .expected_store()
            .checked_successor()
            .ok_or_else(|| LedgerFailure::new(LedgerFailureClass::CounterExhausted))?;
        Ok(Self {
            context,
            resulting_store,
            writes,
        })
    }
    pub const fn context(&self) -> TransactionContext {
        self.context
    }
    pub const fn expected_store(&self) -> StoreRevision {
        self.context.expected_store()
    }
    pub const fn resulting_store(&self) -> StoreRevision {
        self.resulting_store
    }
    pub const fn base_organization(&self) -> OrganizationRevision {
        self.context.base_organization()
    }
    pub const fn resulting_organization(&self) -> OrganizationRevision {
        self.context.resulting_organization()
    }
    pub fn writes(&self) -> &[LedgerWrite] {
        &self.writes
    }
}

/// Untrusted retained operation row, validated before live ledger installation.
#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
pub enum RecoveredOperationRecord {
    Pending(Box<PendingOperationRecord>),
    Completed(CompletedOperationRecord),
}

/// Untrusted recovery inventory. SQL readers enforce row/byte ceilings before
/// allocating these vectors. The borrowed validator checks the complete core
/// recovery bundle, including pairing purposes, owning intents and retirement.
#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
pub struct LedgerRecovery {
    pub format_version: u32,
    pub quota_policy_version: u32,
    pub limits: LedgerLimits,
    pub streams: Vec<StreamRecord>,
    pub operations: Vec<RecoveredOperationRecord>,
}
impl LedgerRecovery {
    pub fn empty(limits: LedgerLimits) -> Self {
        Self {
            format_version: LEDGER_FORMAT_VERSION,
            quota_policy_version: LEDGER_QUOTA_POLICY_VERSION,
            limits,
            streams: Vec::new(),
            operations: Vec::new(),
        }
    }
    pub fn streams(&self) -> &[StreamRecord] {
        &self.streams
    }
    pub fn operations(&self) -> &[RecoveredOperationRecord] {
        &self.operations
    }
}

macro_rules! redacted_debug {
    ($($ty:ty),+ $(,)?) => { $(
        impl fmt::Debug for $ty {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(concat!(stringify!($ty), "([redacted])"))
            }
        }
    )+ };
}
redacted_debug!(
    StreamPurpose,
    StreamRecord,
    CanonicalOperationRequest,
    CompactOutcome,
    ExternalIntentScope,
    ExternalIntentReference,
    ExternalIntentReferenceParts,
    ExternalProgressState,
    PendingExternalProgress,
    PendingExternalProgressParts,
    PendingOperationRecord,
    OperationReductionBasis,
    OperationReductionBasisParts,
    TransactionContext,
    CompletionRevisionBasis,
    CompletedOperationRecord,
    LedgerWrite,
    LedgerProjection,
    RecoveredOperationRecord,
    LedgerRecovery,
);

/// The op-ledger owner supplies the only canonical request/outcome algorithms.
/// No implementation is installed by the declaration freeze.
pub trait LedgerCodec: Send + Sync {
    fn canonical_request_digest(
        &self,
        binding: &StreamBinding,
        operation: OperationId,
        command: &Command,
    ) -> Result<RequestDigest, LedgerFailure>;
    fn encode_canonical_request(
        &self,
        binding: &StreamBinding,
        operation: OperationId,
        command: &Command,
    ) -> Result<Vec<u8>, LedgerFailure>;
    fn decode_canonical_request(
        &self,
        bytes: &[u8],
    ) -> Result<CanonicalOperationRequest, LedgerFailure>;
    fn encode_compact_outcome(&self, outcome: &Outcome) -> Result<Vec<u8>, LedgerFailure>;
    fn decode_compact_outcome(&self, bytes: &[u8]) -> Result<Outcome, LedgerFailure>;
}

impl LedgerProjection {
    /// Shape-checked composition. Complete cross-projection validation is required
    /// by DurableTransaction::new_with_validation, never skipped here.
    pub fn append_to(
        &self,
        parts: &mut crate::storage::TransactionParts,
    ) -> Result<(), LedgerFailure> {
        if parts.expected_store_revision != self.expected_store()
            || parts.resulting_store_revision != self.resulting_store()
            || parts.expected_organization_revision != self.base_organization()
            || parts.resulting_organization_revision != self.resulting_organization()
        {
            return Err(LedgerFailure::new(LedgerFailureClass::InvalidTransition));
        }
        if parts
            .operation_writes
            .len()
            .checked_add(self.writes.len())
            .is_none_or(|n| n > MAX_LEDGER_PROJECTION_WRITES)
        {
            return Err(LedgerFailure::new(LedgerFailureClass::QuotaExceeded));
        }
        parts.operation_writes.extend(
            self.writes
                .iter()
                .cloned()
                .map(|v| crate::storage::OperationWrite::Ledger(Box::new(v))),
        );
        Ok(())
    }
}
