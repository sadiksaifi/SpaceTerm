//! Ledger binary format v1. Integers are fixed-width big-endian. Options and
//! booleans use one byte (0/1); UTF-8 and sequences have a u32 length. Fixed arrays,
//! tuples and checked record parts use declaration-order fields without padding.
//! Enum tags below are explicit u16 values, independent of Rust or Protobuf layout.
//! Maps, floats, indefinite sequences and noncanonical trailing bytes are rejected.
//! The request header is domain, u32 version, exact OperationId, then Command.
//! The outcome header is its distinct domain, u32 version, then Outcome.

use std::fmt;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, SocketAddrV6};

use serde::{Deserialize, Serialize, de, ser};
use sha2::{Digest, Sha256};

use crate::operation_ledger_contracts::*;
use crate::operations::*;
use crate::remote_access::RemoteAccessPolicy;

#[derive(Clone, Copy, Debug, Default)]
pub struct CanonicalCodec;

pub fn canonical_request_digest(
    binding: &StreamBinding,
    operation: OperationId,
    command: &Command,
) -> Result<RequestDigest, LedgerFailure> {
    let mut encoder = Encoder::new(Sha256::new(), MAX_CANONICAL_REQUEST_BYTES);
    encode_request(&mut encoder, binding, operation, command)?;
    Ok(RequestDigest::from_bytes(encoder.sink.finalize().into()))
}

impl LedgerCodec for CanonicalCodec {
    fn canonical_request_digest(
        &self,
        binding: &StreamBinding,
        operation: OperationId,
        command: &Command,
    ) -> Result<RequestDigest, LedgerFailure> {
        canonical_request_digest(binding, operation, command)
    }
    fn encode_canonical_request(
        &self,
        binding: &StreamBinding,
        operation: OperationId,
        command: &Command,
    ) -> Result<Vec<u8>, LedgerFailure> {
        let mut encoder = Encoder::new(Vec::new(), MAX_CANONICAL_REQUEST_BYTES);
        encode_request(&mut encoder, binding, operation, command)?;
        Ok(encoder.sink)
    }
    fn decode_canonical_request(
        &self,
        bytes: &[u8],
    ) -> Result<CanonicalOperationRequest, LedgerFailure> {
        if bytes.len() > MAX_CANONICAL_REQUEST_BYTES {
            return Err(invalid_request());
        }
        let decode = || -> Result<_, CodecError> {
            let mut decoder = Decoder::new(bytes);
            decoder.header(CANONICAL_REQUEST_DOMAIN, CANONICAL_REQUEST_FORMAT_VERSION)?;
            let operation = OperationId::deserialize(&mut decoder)?;
            let command = decode_command(&mut decoder)?;
            decoder.end()?;
            Ok((operation, command))
        };
        let (operation, command) = decode().map_err(|_| invalid_request())?;
        let request = CanonicalOperationRequest::new(operation, command, self)?;
        // This also rejects alternate encodings of unordered endpoint sets.
        if request.encode(self)?.as_slice() != bytes {
            return Err(invalid_request());
        }
        Ok(request)
    }
    fn encode_compact_outcome(&self, outcome: &Outcome) -> Result<Vec<u8>, LedgerFailure> {
        let mut encoder = Encoder::new(Vec::new(), MAX_COMPACT_OUTCOME_BYTES);
        let mut encode = || -> Result<_, CodecError> {
            encoder.raw(COMPACT_OUTCOME_DOMAIN)?;
            encoder.value(&COMPACT_OUTCOME_FORMAT_VERSION)?;
            encode_outcome(&mut encoder, outcome)?;
            Ok(())
        };
        encode().map_err(|_| invalid_outcome())?;
        Ok(encoder.sink)
    }
    fn decode_compact_outcome(&self, bytes: &[u8]) -> Result<Outcome, LedgerFailure> {
        if bytes.len() > MAX_COMPACT_OUTCOME_BYTES {
            return Err(invalid_outcome());
        }
        let decode = || -> Result<_, CodecError> {
            let mut decoder = Decoder::new(bytes);
            decoder.header(COMPACT_OUTCOME_DOMAIN, COMPACT_OUTCOME_FORMAT_VERSION)?;
            let outcome = decode_outcome(&mut decoder)?;
            decoder.end()?;
            Ok(outcome)
        };
        let outcome = decode().map_err(|_| invalid_outcome())?;
        if self.encode_compact_outcome(&outcome)?.as_slice() != bytes {
            return Err(invalid_outcome());
        }
        Ok(outcome)
    }
}
fn invalid_request() -> LedgerFailure {
    LedgerFailure::new(LedgerFailureClass::InvalidRequest)
}
fn invalid_outcome() -> LedgerFailure {
    LedgerFailure::new(LedgerFailureClass::InvalidCompactOutcome)
}

fn encode_request<S: Sink>(
    encoder: &mut Encoder<S>,
    binding: &StreamBinding,
    operation: OperationId,
    command: &Command,
) -> Result<(), LedgerFailure> {
    binding.check_operation(operation)?;
    command
        .validate_scope(binding.stream())
        .map_err(|_| LedgerFailure::new(LedgerFailureClass::ScopeMismatch))?;
    if matches!(command, Command::BeginStartup(intent) if intent.id() != operation) {
        return Err(LedgerFailure::new(LedgerFailureClass::ScopeMismatch));
    }
    let mut encode = || -> Result<(), CodecError> {
        encoder.raw(CANONICAL_REQUEST_DOMAIN)?;
        encoder.value(&CANONICAL_REQUEST_FORMAT_VERSION)?;
        encoder.value(&operation)?;
        encode_command(encoder, command)
    };
    encode().map_err(|_| invalid_request())
}

// Command tags 1..29 and Outcome tags 1..17 are frozen v1 discriminants.
// Each exhaustive arm encodes every field explicitly in the order written here.
fn encode_command<S: Sink>(e: &mut Encoder<S>, command: &Command) -> Result<(), CodecError> {
    macro_rules! arm { ($tag:expr, $($field:expr),* $(,)?) => {{ e.value(&($tag as u16))?; $(e.value($field)?;)* Ok(()) }}; }
    match command {
        Command::CreateWorkspace {
            server,
            source,
            name,
        } => arm!(1, server, source, name),
        Command::CreateTab { workspace, source } => arm!(2, workspace, source),
        Command::CreatePane {
            tab,
            source,
            axis,
            edge,
        } => arm!(3, tab, source, axis, edge),
        Command::RenameWorkspace {
            workspace,
            expected_version,
            draft,
        } => arm!(4, workspace, expected_version, draft),
        Command::SetPinnedDirectory {
            workspace,
            expected_version,
            value,
        } => arm!(5, workspace, expected_version, value),
        Command::RepairPaneDirectory {
            pane,
            expected_version,
            value,
        } => arm!(6, pane, expected_version, value),
        Command::RetryPaneDirectory {
            pane,
            expected_version,
        } => arm!(7, pane, expected_version),
        Command::SetWorktreeLocation {
            server,
            expected_version,
            template,
        } => arm!(8, server, expected_version, template),
        Command::AssessClose { target } => arm!(9, target),
        Command::CloseConfirmed { target, assessment } => arm!(10, target, assessment),
        Command::KillWorkspaceConfirmed {
            workspace,
            assessment,
        } => arm!(11, workspace, assessment),
        Command::AssessEndAllSessions { server } => arm!(12, server),
        Command::EndAllSessions { server, assessment } => arm!(13, server, assessment),
        Command::CompareAcquire(v) => arm!(14, v),
        Command::TakeOver(v) => arm!(15, v),
        Command::ReleaseControl(v) => arm!(16, v),
        Command::BeginStartup(v) => arm!(17, v),
        Command::CompleteStartup(v) => arm!(18, v),
        Command::CancelStartup { intent } => arm!(19, intent),
        Command::RevokePairing { pairing } => arm!(20, pairing),
        Command::RefreshWorktrees { scope } => arm!(21, scope),
        Command::CreateWorktree { scope, branch } => arm!(22, scope, branch),
        Command::AssessRemoveWorktree { scope, worktree } => arm!(23, scope, worktree),
        Command::RemoveWorktreeConfirmed {
            scope,
            worktree,
            assessment,
            force,
        } => arm!(24, scope, worktree, assessment, force),
        Command::AssessKillWorkspace { workspace } => arm!(25, workspace),
        Command::SetRemoteAccess {
            server,
            expected_version,
            value,
        } => {
            arm!(26, server, expected_version)?;
            encode_policy(e, value)
        }
        Command::Pairing(v) => arm!(27, v),
        Command::Worktree(v) => arm!(28, v),
        Command::CompleteMeasuredStartup(v) => arm!(29, v),
    }
}
fn decode_command(d: &mut Decoder<'_>) -> Result<Command, CodecError> {
    macro_rules! read {
        () => {
            Deserialize::deserialize(&mut *d)?
        };
    }
    Ok(match u16::deserialize(&mut *d)? {
        1 => Command::CreateWorkspace {
            server: read!(),
            source: read!(),
            name: read!(),
        },
        2 => Command::CreateTab {
            workspace: read!(),
            source: read!(),
        },
        3 => Command::CreatePane {
            tab: read!(),
            source: read!(),
            axis: read!(),
            edge: read!(),
        },
        4 => Command::RenameWorkspace {
            workspace: read!(),
            expected_version: read!(),
            draft: read!(),
        },
        5 => Command::SetPinnedDirectory {
            workspace: read!(),
            expected_version: read!(),
            value: read!(),
        },
        6 => Command::RepairPaneDirectory {
            pane: read!(),
            expected_version: read!(),
            value: read!(),
        },
        7 => Command::RetryPaneDirectory {
            pane: read!(),
            expected_version: read!(),
        },
        8 => Command::SetWorktreeLocation {
            server: read!(),
            expected_version: read!(),
            template: read!(),
        },
        9 => Command::AssessClose { target: read!() },
        10 => Command::CloseConfirmed {
            target: read!(),
            assessment: read!(),
        },
        11 => Command::KillWorkspaceConfirmed {
            workspace: read!(),
            assessment: read!(),
        },
        12 => Command::AssessEndAllSessions { server: read!() },
        13 => Command::EndAllSessions {
            server: read!(),
            assessment: read!(),
        },
        14 => Command::CompareAcquire(read!()),
        15 => Command::TakeOver(read!()),
        16 => Command::ReleaseControl(read!()),
        17 => Command::BeginStartup(read!()),
        18 => Command::CompleteStartup(read!()),
        19 => Command::CancelStartup { intent: read!() },
        20 => Command::RevokePairing { pairing: read!() },
        21 => Command::RefreshWorktrees { scope: read!() },
        22 => Command::CreateWorktree {
            scope: read!(),
            branch: read!(),
        },
        23 => Command::AssessRemoveWorktree {
            scope: read!(),
            worktree: read!(),
        },
        24 => Command::RemoveWorktreeConfirmed {
            scope: read!(),
            worktree: read!(),
            assessment: read!(),
            force: read!(),
        },
        25 => Command::AssessKillWorkspace { workspace: read!() },
        26 => Command::SetRemoteAccess {
            server: read!(),
            expected_version: read!(),
            value: decode_policy(d)?,
        },
        27 => Command::Pairing(read!()),
        28 => Command::Worktree(read!()),
        29 => Command::CompleteMeasuredStartup(read!()),
        _ => return Err(CodecError),
    })
}
fn encode_outcome<S: Sink>(e: &mut Encoder<S>, outcome: &Outcome) -> Result<(), CodecError> {
    macro_rules! arm { ($tag:expr, $($field:expr),* $(,)?) => {{ e.value(&($tag as u16))?; $(e.value($field)?;)* Ok(()) }}; }
    match outcome {
        Outcome::Applied => arm!(1,),
        Outcome::CreatedWorkspace {
            workspace,
            tab,
            pane,
        } => arm!(2, workspace, tab, pane),
        Outcome::CreatedTab { tab, pane } => arm!(3, tab, pane),
        Outcome::CreatedPane { pane } => arm!(4, pane),
        Outcome::CreatedWorktree { worktree } => arm!(5, worktree),
        Outcome::ControlAcquired(v) => arm!(6, v),
        Outcome::ControlUnavailable(v) => arm!(7, v),
        Outcome::StartupAwaitingMeasurement { intent } => arm!(8, intent),
        Outcome::SessionStarted(v) => arm!(9, v),
        Outcome::Assessed {
            assessment,
            affected_panes,
            running_sessions,
        } => arm!(10, assessment, affected_panes, running_sessions),
        Outcome::Conflict(v) => {
            arm!(11,)?;
            encode_conflict(e, v)
        }
        Outcome::Failed(v) => arm!(12, v),
        Outcome::CreatedWithControl(v) => arm!(13, v),
        Outcome::ControlGranted(v) => arm!(14, v),
        Outcome::Assessment(v) => arm!(15, v),
        Outcome::Pairing(v) => arm!(16, v),
        Outcome::Git(v) => arm!(17, v),
    }
}
fn decode_outcome(d: &mut Decoder<'_>) -> Result<Outcome, CodecError> {
    macro_rules! read {
        () => {
            Deserialize::deserialize(&mut *d)?
        };
    }
    Ok(match u16::deserialize(&mut *d)? {
        1 => Outcome::Applied,
        2 => Outcome::CreatedWorkspace {
            workspace: read!(),
            tab: read!(),
            pane: read!(),
        },
        3 => Outcome::CreatedTab {
            tab: read!(),
            pane: read!(),
        },
        4 => Outcome::CreatedPane { pane: read!() },
        5 => Outcome::CreatedWorktree { worktree: read!() },
        6 => Outcome::ControlAcquired(read!()),
        7 => Outcome::ControlUnavailable(read!()),
        8 => Outcome::StartupAwaitingMeasurement { intent: read!() },
        9 => Outcome::SessionStarted(read!()),
        10 => Outcome::Assessed {
            assessment: read!(),
            affected_panes: read!(),
            running_sessions: read!(),
        },
        11 => Outcome::Conflict(decode_conflict(d)?),
        12 => Outcome::Failed(read!()),
        13 => Outcome::CreatedWithControl(read!()),
        14 => Outcome::ControlGranted(read!()),
        15 => Outcome::Assessment(read!()),
        16 => Outcome::Pairing(read!()),
        17 => Outcome::Git(read!()),
        _ => return Err(CodecError),
    })
}
fn encode_conflict<S: Sink>(e: &mut Encoder<S>, v: &FieldConflict) -> Result<(), CodecError> {
    macro_rules! arm { ($tag:expr, $($field:expr),*) => {{ e.value(&($tag as u16))?; $(e.value($field)?;)* Ok(()) }}; }
    match v {
        FieldConflict::WorkspaceName {
            workspace,
            current_value,
            current_version,
        } => arm!(1, workspace, current_value, current_version),
        FieldConflict::PinnedDirectory {
            workspace,
            current_value,
            current_version,
        } => arm!(2, workspace, current_value, current_version),
        FieldConflict::PaneDirectory {
            pane,
            current_value,
            current_version,
        } => arm!(3, pane, current_value, current_version),
        FieldConflict::WorktreeLocation {
            server,
            current_value,
            current_version,
        } => arm!(4, server, current_value, current_version),
        FieldConflict::RemoteAccess {
            server,
            current_value,
            current_version,
        } => {
            arm!(5, server)?;
            encode_policy(e, current_value)?;
            e.value(current_version)
        }
    }
}
fn decode_conflict(d: &mut Decoder<'_>) -> Result<FieldConflict, CodecError> {
    macro_rules! read {
        () => {
            Deserialize::deserialize(&mut *d)?
        };
    }
    Ok(match u16::deserialize(&mut *d)? {
        1 => FieldConflict::WorkspaceName {
            workspace: read!(),
            current_value: read!(),
            current_version: read!(),
        },
        2 => FieldConflict::PinnedDirectory {
            workspace: read!(),
            current_value: read!(),
            current_version: read!(),
        },
        3 => FieldConflict::PaneDirectory {
            pane: read!(),
            current_value: read!(),
            current_version: read!(),
        },
        4 => FieldConflict::WorktreeLocation {
            server: read!(),
            current_value: read!(),
            current_version: read!(),
        },
        5 => FieldConflict::RemoteAccess {
            server: read!(),
            current_value: decode_policy(d)?,
            current_version: read!(),
        },
        _ => return Err(CodecError),
    })
}

// Endpoint order is not policy. Encode a sorted bounded set, preserving IPv6
// scope/flow facts that standard non-human-readable SocketAddr serde omits.
fn encode_policy<S: Sink>(
    e: &mut Encoder<S>,
    policy: &RemoteAccessPolicy,
) -> Result<(), CodecError> {
    e.value(&policy.server())?;
    let mut endpoints = policy.endpoints().to_vec();
    endpoints.sort();
    e.value(&(endpoints.len() as u32))?;
    for endpoint in endpoints {
        match endpoint {
            SocketAddr::V4(v) => {
                e.value(&4u8)?;
                e.value(&v.ip().octets())?;
                e.value(&v.port())?;
            }
            SocketAddr::V6(v) => {
                e.value(&6u8)?;
                e.value(&v.ip().octets())?;
                e.value(&v.port())?;
                e.value(&v.flowinfo())?;
                e.value(&v.scope_id())?;
            }
        }
    }
    Ok(())
}
fn decode_policy(d: &mut Decoder<'_>) -> Result<RemoteAccessPolicy, CodecError> {
    let server = Deserialize::deserialize(&mut *d)?;
    let len = u32::deserialize(&mut *d)? as usize;
    if len > crate::remote_access::MAX_REMOTE_BIND_ENDPOINTS {
        return Err(CodecError);
    }
    let mut endpoints = Vec::with_capacity(len);
    for _ in 0..len {
        let endpoint = match u8::deserialize(&mut *d)? {
            4 => {
                let ip = Ipv4Addr::from(<[u8; 4]>::deserialize(&mut *d)?);
                let port = u16::deserialize(&mut *d)?;
                SocketAddr::new(IpAddr::V4(ip), port)
            }
            6 => {
                let ip = Ipv6Addr::from(<[u8; 16]>::deserialize(&mut *d)?);
                let port = u16::deserialize(&mut *d)?;
                let flow = u32::deserialize(&mut *d)?;
                let scope = u32::deserialize(&mut *d)?;
                SocketAddr::V6(SocketAddrV6::new(ip, port, flow, scope))
            }
            _ => return Err(CodecError),
        };
        if endpoints
            .last()
            .is_some_and(|previous| *previous >= endpoint)
        {
            return Err(CodecError);
        }
        endpoints.push(endpoint);
    }
    if len == 0 {
        Ok(RemoteAccessPolicy::disabled(server))
    } else {
        RemoteAccessPolicy::enabled(server, endpoints).map_err(|_| CodecError)
    }
}

// Nested discriminants are explicitly frozen by semantic name, never serde's
// variant_index. Checked record constructors still run on binary decoding.
fn variants(name: &str) -> Result<&'static [&'static str], CodecError> {
    Ok(match name {
        "WorkspaceCreationSource" => &["ServerHome", "Directory", "Pane"],
        "TabCreationSource" => &["Pane", "Directory", "Worktree"],
        "NameDraft" => &["Automatic", "Custom", "Blank"],
        "NameMode" => &["Automatic", "Custom"],
        "SplitAxis" => &["Horizontal", "Vertical"],
        "SplitEdge" => &["Before", "After"],
        "CloseTarget" => &["Pane", "Tab", "Workspace", "WorktreeTabs"],
        "AssessmentTarget" => &["Close", "EndAllSessions", "RemoveWorktree", "KillWorkspace"],
        "StartupSource" => &["ExplicitSelection", "TabOpening", "StartSession"],
        "ControlRecord" => &["Unclaimed", "Held"],
        "CreatedEntities" => &["Workspace", "Tab", "Pane"],
        "OperationFailure" => &[
            "TargetDeleted",
            "StaleAuthority",
            "StaleAssessment",
            "NotControlled",
            "Capacity",
            "Exhausted",
            "DirectoryUnavailable",
            "StorageUnavailable",
            "Unsupported",
            "Runtime",
            "InvalidValue",
            "WrongMembership",
            "StaleValidation",
            "AssessmentExpired",
            "BusyRetiring",
        ],
        "FailureClass" => &[
            "LaunchUnavailable",
            "RuntimeUnavailable",
            "RuntimeFault",
            "ResourceLimit",
            "StorageUnavailable",
            "UnsafeStorage",
            "AuthorizationDenied",
            "DirectoryUnavailable",
            "InvalidRequest",
            "StaleAuthority",
            "Unsupported",
            "Internal",
        ],
        "PairingCommand" => &[
            "IssueDirectAuthorization",
            "Enroll",
            "AssessUnpair",
            "UnpairConfirmed",
            "SelfRevoke",
            "BindAdditionalClient",
        ],
        "PairingOutcome" => &[
            "AuthorizationReserved",
            "Enrolled",
            "ClientBound",
            "UnpairAssessed",
            "Revoked",
            "AlreadyRevoked",
            "Failed",
        ],
        "PairingReceiptBody" => &["Enrollment", "ClientBinding", "Revocation"],
        "PairingFailure" => &[
            "InvalidRequest",
            "LimitExceeded",
            "IdentityMismatch",
            "FingerprintMismatch",
            "AuthorizationRequired",
            "AuthorizationDenied",
            "AuthorizationUnavailable",
            "PairingRevoked",
            "ClientNotBound",
            "StaleAssessment",
            "AuthorizationExpired",
            "AuthorizationConsumed",
            "RequestIdentityReused",
            "Offline",
            "RouteChanged",
            "TimedOut",
            "Cancelled",
            "Incompatible",
            "UnsupportedTarget",
            "ServiceManagerUnavailable",
            "ExplicitStartRequired",
            "CredentialUnavailable",
            "CredentialIntegrity",
            "StorageUnavailable",
            "UnsafeStorage",
            "UnknownSchema",
            "InvalidDurableState",
            "RecoveryRequired",
            "ReceiptInvalid",
            "RevocationUnconfirmed",
        ],
        "GitCommand" => &["CreateConfirmed", "RemoveConfirmed"],
        "GitAssessmentKind" => &["Create", "Remove"],
        "GitMutationOutcomeParts" => &["Created", "Removed", "Failed", "Recovered"],
        "ExternalDisposition" => &[
            "NoEffectEstablished",
            "DesiredStateObserved",
            "PartialOrAmbiguous",
            "Unavailable",
        ],
        "RecoveryObservation" => &[
            "DesiredStateObserved",
            "PreStateObserved",
            "PartialOrAmbiguous",
            "Unavailable",
        ],
        "GitFailure" => &[
            "Unauthorized",
            "WrongServer",
            "TargetGone",
            "Offline",
            "Busy",
            "Capacity",
            "ExpiredAssessment",
            "StaleAssessment",
            "MainWorktree",
            "LockedWorktree",
            "UnlistedWorktree",
            "HoldsPinnedDirectory",
            "NoFallback",
            "InvalidBranch",
            "InvalidTemplate",
            "InvalidDirectory",
            "OccupiedTarget",
            "BranchExists",
            "BranchMissing",
            "BranchCheckedOut",
            "IdentityReplaced",
            "GitStateChanged",
            "Unchecked",
            "ToolMissing",
            "ToolTooOld",
            "CancelledRead",
            "TimedOutRead",
            "InvalidResponse",
            "OutputTooLarge",
            "WriteFailed",
            "ExternalStateUncertain",
            "RecoveryNeedsReview",
            "StorageLocked",
            "StorageUnavailable",
            "UnsafeStorage",
            "StorageFailure",
            "IndeterminateDurability",
            "BareRepositoryWithoutWorktree",
        ],
        _ => return Err(CodecError),
    })
}

#[derive(Clone, Copy, Debug)]
struct CodecError;
impl fmt::Display for CodecError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("invalid ledger binary encoding")
    }
}
impl std::error::Error for CodecError {}
impl ser::Error for CodecError {
    fn custom<T: fmt::Display>(_: T) -> Self {
        Self
    }
}
impl de::Error for CodecError {
    fn custom<T: fmt::Display>(_: T) -> Self {
        Self
    }
}
trait Sink {
    fn put(&mut self, bytes: &[u8]);
}
impl Sink for Vec<u8> {
    fn put(&mut self, bytes: &[u8]) {
        self.extend_from_slice(bytes);
    }
}
impl Sink for Sha256 {
    fn put(&mut self, bytes: &[u8]) {
        self.update(bytes);
    }
}
struct Encoder<S> {
    sink: S,
    bytes: usize,
    max: usize,
}
impl<S: Sink> Encoder<S> {
    fn new(sink: S, max: usize) -> Self {
        Self {
            sink,
            bytes: 0,
            max,
        }
    }
    fn raw(&mut self, bytes: &[u8]) -> Result<(), CodecError> {
        if bytes.len() > self.max.saturating_sub(self.bytes) {
            return Err(CodecError);
        }
        self.bytes += bytes.len();
        self.sink.put(bytes);
        Ok(())
    }
    fn value<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<(), CodecError> {
        value.serialize(&mut *self)
    }
    fn tag(&mut self, name: &'static str, variant: &'static str) -> Result<(), CodecError> {
        let index = variants(name)?
            .iter()
            .position(|v| *v == variant)
            .ok_or(CodecError)?;
        self.value(&((index + 1) as u16))
    }
}
struct Compound<'a, S>(&'a mut Encoder<S>);
impl<'a, S: Sink> ser::Serializer for &'a mut Encoder<S> {
    type Ok = ();
    type Error = CodecError;
    type SerializeSeq = Compound<'a, S>;
    type SerializeTuple = Compound<'a, S>;
    type SerializeTupleStruct = Compound<'a, S>;
    type SerializeTupleVariant = Compound<'a, S>;
    type SerializeMap = ser::Impossible<(), CodecError>;
    type SerializeStruct = Compound<'a, S>;
    type SerializeStructVariant = Compound<'a, S>;
    fn serialize_bool(self, v: bool) -> Result<(), CodecError> {
        self.raw(&[u8::from(v)])
    }
    fn serialize_i8(self, v: i8) -> Result<(), CodecError> {
        self.raw(&v.to_be_bytes())
    }
    fn serialize_i16(self, v: i16) -> Result<(), CodecError> {
        self.raw(&v.to_be_bytes())
    }
    fn serialize_i32(self, v: i32) -> Result<(), CodecError> {
        self.raw(&v.to_be_bytes())
    }
    fn serialize_i64(self, v: i64) -> Result<(), CodecError> {
        self.raw(&v.to_be_bytes())
    }
    fn serialize_u8(self, v: u8) -> Result<(), CodecError> {
        self.raw(&[v])
    }
    fn serialize_u16(self, v: u16) -> Result<(), CodecError> {
        self.raw(&v.to_be_bytes())
    }
    fn serialize_u32(self, v: u32) -> Result<(), CodecError> {
        self.raw(&v.to_be_bytes())
    }
    fn serialize_u64(self, v: u64) -> Result<(), CodecError> {
        self.raw(&v.to_be_bytes())
    }
    fn serialize_f32(self, _: f32) -> Result<(), CodecError> {
        Err(CodecError)
    }
    fn serialize_f64(self, _: f64) -> Result<(), CodecError> {
        Err(CodecError)
    }
    fn serialize_char(self, _: char) -> Result<(), CodecError> {
        Err(CodecError)
    }
    fn serialize_str(self, v: &str) -> Result<(), CodecError> {
        self.serialize_bytes(v.as_bytes())
    }
    fn serialize_bytes(self, v: &[u8]) -> Result<(), CodecError> {
        self.value(&u32::try_from(v.len()).map_err(|_| CodecError)?)?;
        self.raw(v)
    }
    fn serialize_none(self) -> Result<(), CodecError> {
        self.raw(&[0])
    }
    fn serialize_some<T: Serialize + ?Sized>(self, v: &T) -> Result<(), CodecError> {
        self.raw(&[1])?;
        self.value(v)
    }
    fn serialize_unit(self) -> Result<(), CodecError> {
        Ok(())
    }
    fn serialize_unit_struct(self, _: &'static str) -> Result<(), CodecError> {
        Ok(())
    }
    fn serialize_unit_variant(
        self,
        name: &'static str,
        _: u32,
        variant: &'static str,
    ) -> Result<(), CodecError> {
        self.tag(name, variant)
    }
    fn serialize_newtype_struct<T: Serialize + ?Sized>(
        self,
        _: &'static str,
        v: &T,
    ) -> Result<(), CodecError> {
        self.value(v)
    }
    fn serialize_newtype_variant<T: Serialize + ?Sized>(
        self,
        name: &'static str,
        _: u32,
        variant: &'static str,
        v: &T,
    ) -> Result<(), CodecError> {
        self.tag(name, variant)?;
        self.value(v)
    }
    fn serialize_seq(self, len: Option<usize>) -> Result<Self::SerializeSeq, CodecError> {
        self.value(&u32::try_from(len.ok_or(CodecError)?).map_err(|_| CodecError)?)?;
        Ok(Compound(self))
    }
    fn serialize_tuple(self, _: usize) -> Result<Self::SerializeTuple, CodecError> {
        Ok(Compound(self))
    }
    fn serialize_tuple_struct(
        self,
        _: &'static str,
        _: usize,
    ) -> Result<Self::SerializeTupleStruct, CodecError> {
        Ok(Compound(self))
    }
    fn serialize_tuple_variant(
        self,
        name: &'static str,
        _: u32,
        variant: &'static str,
        _: usize,
    ) -> Result<Self::SerializeTupleVariant, CodecError> {
        self.tag(name, variant)?;
        Ok(Compound(self))
    }
    fn serialize_map(self, _: Option<usize>) -> Result<Self::SerializeMap, CodecError> {
        Err(CodecError)
    }
    fn serialize_struct(
        self,
        _: &'static str,
        _: usize,
    ) -> Result<Self::SerializeStruct, CodecError> {
        Ok(Compound(self))
    }
    fn serialize_struct_variant(
        self,
        name: &'static str,
        _: u32,
        variant: &'static str,
        _: usize,
    ) -> Result<Self::SerializeStructVariant, CodecError> {
        self.tag(name, variant)?;
        Ok(Compound(self))
    }
    fn is_human_readable(&self) -> bool {
        false
    }
}
macro_rules! compound {
    ($trait:ident, $method:ident $(, $field:ident)?) => {
        impl<S: Sink> ser::$trait for Compound<'_, S> {
            type Ok = (); type Error = CodecError;
            fn $method<T: Serialize + ?Sized>(&mut self, $($field: &'static str,)? value: &T) -> Result<(), CodecError> { $(let _ = $field;)? self.0.value(value) }
            fn end(self) -> Result<(), CodecError> { Ok(()) }
        }
    };
}
compound!(SerializeSeq, serialize_element);
compound!(SerializeTuple, serialize_element);
compound!(SerializeTupleStruct, serialize_field);
compound!(SerializeTupleVariant, serialize_field);
compound!(SerializeStruct, serialize_field, key);
compound!(SerializeStructVariant, serialize_field, key);

struct Decoder<'de> {
    bytes: &'de [u8],
}
impl<'de> Decoder<'de> {
    fn new(bytes: &'de [u8]) -> Self {
        Self { bytes }
    }
    fn take(&mut self, n: usize) -> Result<&'de [u8], CodecError> {
        if n > self.bytes.len() {
            return Err(CodecError);
        }
        let (value, rest) = self.bytes.split_at(n);
        self.bytes = rest;
        Ok(value)
    }
    fn end(&self) -> Result<(), CodecError> {
        if self.bytes.is_empty() {
            Ok(())
        } else {
            Err(CodecError)
        }
    }
    fn header(&mut self, domain: &[u8], version: u32) -> Result<(), CodecError> {
        if self.take(domain.len())? != domain || u32::deserialize(&mut *self)? != version {
            return Err(CodecError);
        }
        Ok(())
    }
    fn len(&mut self) -> Result<usize, CodecError> {
        let n = u32::deserialize(&mut *self)? as usize;
        // Every allowed sequence element occupies at least one byte. Never hand
        // serde an untrusted, allocation-amplifying size hint.
        if n > self.bytes.len() {
            return Err(CodecError);
        }
        Ok(n)
    }
}
struct Sequence<'a, 'de> {
    decoder: &'a mut Decoder<'de>,
    remaining: usize,
}
impl<'de> de::SeqAccess<'de> for Sequence<'_, 'de> {
    type Error = CodecError;
    fn next_element_seed<T: de::DeserializeSeed<'de>>(
        &mut self,
        seed: T,
    ) -> Result<Option<T::Value>, CodecError> {
        if self.remaining == 0 {
            return Ok(None);
        }
        self.remaining -= 1;
        seed.deserialize(&mut *self.decoder).map(Some)
    }
    fn size_hint(&self) -> Option<usize> {
        Some(self.remaining.min(16))
    }
}
struct Variant<'a, 'de> {
    decoder: &'a mut Decoder<'de>,
    name: &'static str,
}
impl<'de, 'a> de::EnumAccess<'de> for Variant<'a, 'de> {
    type Error = CodecError;
    type Variant = Self;
    fn variant_seed<V: de::DeserializeSeed<'de>>(
        self,
        seed: V,
    ) -> Result<(V::Value, Self), CodecError> {
        let value = seed.deserialize(de::value::StrDeserializer::<CodecError>::new(self.name))?;
        Ok((value, self))
    }
}
impl<'de> de::VariantAccess<'de> for Variant<'_, 'de> {
    type Error = CodecError;
    fn unit_variant(self) -> Result<(), CodecError> {
        Ok(())
    }
    fn newtype_variant_seed<T: de::DeserializeSeed<'de>>(
        self,
        seed: T,
    ) -> Result<T::Value, CodecError> {
        seed.deserialize(self.decoder)
    }
    fn tuple_variant<V: de::Visitor<'de>>(
        self,
        len: usize,
        visitor: V,
    ) -> Result<V::Value, CodecError> {
        de::Deserializer::deserialize_tuple(self.decoder, len, visitor)
    }
    fn struct_variant<V: de::Visitor<'de>>(
        self,
        fields: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, CodecError> {
        de::Deserializer::deserialize_tuple(self.decoder, fields.len(), visitor)
    }
}
macro_rules! number {
    ($method:ident, $ty:ty, $visit:ident) => {
        fn $method<V: de::Visitor<'de>>(self, visitor: V) -> Result<V::Value, CodecError> {
            let value = <$ty>::from_be_bytes(
                self.take(std::mem::size_of::<$ty>())?
                    .try_into()
                    .map_err(|_| CodecError)?,
            );
            visitor.$visit(value)
        }
    };
}
impl<'de> de::Deserializer<'de> for &mut Decoder<'de> {
    type Error = CodecError;
    fn deserialize_any<V: de::Visitor<'de>>(self, _: V) -> Result<V::Value, CodecError> {
        Err(CodecError)
    }
    fn deserialize_bool<V: de::Visitor<'de>>(self, visitor: V) -> Result<V::Value, CodecError> {
        match self.take(1)?[0] {
            0 => visitor.visit_bool(false),
            1 => visitor.visit_bool(true),
            _ => Err(CodecError),
        }
    }
    number!(deserialize_i8, i8, visit_i8);
    number!(deserialize_i16, i16, visit_i16);
    number!(deserialize_i32, i32, visit_i32);
    number!(deserialize_i64, i64, visit_i64);
    number!(deserialize_u8, u8, visit_u8);
    number!(deserialize_u16, u16, visit_u16);
    number!(deserialize_u32, u32, visit_u32);
    number!(deserialize_u64, u64, visit_u64);
    fn deserialize_str<V: de::Visitor<'de>>(self, visitor: V) -> Result<V::Value, CodecError> {
        let len = self.len()?;
        let v = std::str::from_utf8(self.take(len)?).map_err(|_| CodecError)?;
        visitor.visit_borrowed_str(v)
    }
    fn deserialize_string<V: de::Visitor<'de>>(self, visitor: V) -> Result<V::Value, CodecError> {
        self.deserialize_str(visitor)
    }
    fn deserialize_bytes<V: de::Visitor<'de>>(self, visitor: V) -> Result<V::Value, CodecError> {
        let len = self.len()?;
        visitor.visit_borrowed_bytes(self.take(len)?)
    }
    fn deserialize_byte_buf<V: de::Visitor<'de>>(self, visitor: V) -> Result<V::Value, CodecError> {
        self.deserialize_bytes(visitor)
    }
    fn deserialize_option<V: de::Visitor<'de>>(self, visitor: V) -> Result<V::Value, CodecError> {
        match self.take(1)?[0] {
            0 => visitor.visit_none(),
            1 => visitor.visit_some(self),
            _ => Err(CodecError),
        }
    }
    fn deserialize_unit<V: de::Visitor<'de>>(self, visitor: V) -> Result<V::Value, CodecError> {
        visitor.visit_unit()
    }
    fn deserialize_unit_struct<V: de::Visitor<'de>>(
        self,
        _: &'static str,
        visitor: V,
    ) -> Result<V::Value, CodecError> {
        self.deserialize_unit(visitor)
    }
    fn deserialize_newtype_struct<V: de::Visitor<'de>>(
        self,
        _: &'static str,
        visitor: V,
    ) -> Result<V::Value, CodecError> {
        visitor.visit_newtype_struct(self)
    }
    fn deserialize_seq<V: de::Visitor<'de>>(self, visitor: V) -> Result<V::Value, CodecError> {
        let len = self.len()?;
        self.deserialize_tuple(len, visitor)
    }
    fn deserialize_tuple<V: de::Visitor<'de>>(
        self,
        len: usize,
        visitor: V,
    ) -> Result<V::Value, CodecError> {
        let mut sequence = Sequence {
            decoder: self,
            remaining: len,
        };
        let value = visitor.visit_seq(&mut sequence)?;
        if sequence.remaining != 0 {
            return Err(CodecError);
        }
        Ok(value)
    }
    fn deserialize_tuple_struct<V: de::Visitor<'de>>(
        self,
        _: &'static str,
        len: usize,
        visitor: V,
    ) -> Result<V::Value, CodecError> {
        self.deserialize_tuple(len, visitor)
    }
    fn deserialize_struct<V: de::Visitor<'de>>(
        self,
        _: &'static str,
        fields: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, CodecError> {
        self.deserialize_tuple(fields.len(), visitor)
    }
    fn deserialize_enum<V: de::Visitor<'de>>(
        self,
        name: &'static str,
        _: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, CodecError> {
        let tag = u16::deserialize(&mut *self)?;
        let variant = variants(name)?
            .get(usize::from(tag).checked_sub(1).ok_or(CodecError)?)
            .ok_or(CodecError)?;
        visitor.visit_enum(Variant {
            decoder: self,
            name: variant,
        })
    }
    fn deserialize_map<V: de::Visitor<'de>>(self, _: V) -> Result<V::Value, CodecError> {
        Err(CodecError)
    }
    fn deserialize_identifier<V: de::Visitor<'de>>(self, _: V) -> Result<V::Value, CodecError> {
        Err(CodecError)
    }
    fn deserialize_ignored_any<V: de::Visitor<'de>>(self, _: V) -> Result<V::Value, CodecError> {
        Err(CodecError)
    }
    fn deserialize_f32<V: de::Visitor<'de>>(self, _: V) -> Result<V::Value, CodecError> {
        Err(CodecError)
    }
    fn deserialize_f64<V: de::Visitor<'de>>(self, _: V) -> Result<V::Value, CodecError> {
        Err(CodecError)
    }
    fn deserialize_char<V: de::Visitor<'de>>(self, _: V) -> Result<V::Value, CodecError> {
        Err(CodecError)
    }
    fn is_human_readable(&self) -> bool {
        false
    }
}
