//! Versioned store-owned binary fields. Ledger requests and outcomes use LedgerCodec.
//!
//! V1 records are "STOS", u16 version, u16 kind, u32 payload length, then payload.
//! Integers and lengths are big-endian. Tags below are retained format constants.
//! SQL relational counters use exactly eight bytes, without a record envelope.
//! Record framing establishes byte shape, not domain or native authority.

use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr, SocketAddrV4, SocketAddrV6};
use std::num::NonZeroU64;

use spaceterm_core::authority::{
    CloseWorkGeneration, ContextGeneration, ControlGeneration, CreationOrdinal, FieldVersion,
    OrganizationRevision, ServerEpoch, SessionGeneration, SessionKey,
};
use spaceterm_core::identity::{
    ClientId, IdentityHighWater, LocalIdentity, OperationStreamId, OperationStreamKey, PairingId,
    PaneId, RepositoryId, ServerId, TabId, WorktreeId,
};
use spaceterm_core::native_management::{NativeManagementRequestRef, NativeRetirementAction};
use spaceterm_core::operations::{
    AssessedPane, OperationBinding, OperationId, OperationSequence, RepositoryScope, RequestDigest,
};
use spaceterm_core::organization::{
    BoundedText, BranchName, FailureClass, MAX_DIRECTORY_BYTES, MAX_NAME_BYTES, PaneCreation,
    RetirementReason, ServerDirectory, SplitAxis, SplitEdge, TabRecord,
};
use spaceterm_core::organization_retained::TabCreationDefault;
use spaceterm_core::pairing::{
    ClientRecord, EnrollmentIssuer, PairingClientStreams, PairingClientStreamsParts, PairingKeys,
    PairingKeysParts,
};
use spaceterm_core::remote_access::{MAX_REMOTE_BIND_ENDPOINTS, RemoteAccessPolicy};
use spaceterm_core::retirement::{
    ConfirmedCleanupClass, MAX_RETIREMENT_PANES, MAX_RETIREMENT_SESSIONS,
    NativeRetirementDescriptor, NativeRetirementDescriptorParts, RetiredSession, RetirementBatchId,
    RetirementCleanupState, RetirementPane,
};
use spaceterm_core::worktrees::intent::{
    ApprovedRemoval, BranchReference, CreationTargetObservation, GitAllocationReservation,
    GitAllocationReservationParts, GitObjectId, GitReservedFallback, RepositoryLocator,
    RepositoryLocatorParts, RetainedAffectedPane, RetainedExternalResult, RetainedGitAction,
    RetainedGitHead, RetainedRemovalScope, RetainedRemovalScopeParts, WorkspaceFallbackPlan,
    WorktreeBranchChoice,
};
use spaceterm_core::worktrees::types::{
    ExternalDisposition, GitFailure, MAX_GIT_PANES, MAX_GIT_TABS,
};
use thiserror::Error;

const MAGIC: &[u8; 4] = b"STOS";
const VERSION: u16 = 1;
const HEADER_BYTES: usize = 12;

/// Content-free codec classification. It never wraps native or parsing errors.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum CodecFailure {
    /// Unsupported retained format. Never recover by rewriting the bytes.
    #[error("stored field encoding is unsupported")]
    UnsupportedVersion,
    /// Unknown tag, invalid value, truncation, wrong length, or trailing bytes.
    #[error("stored field encoding is invalid")]
    InvalidEncoding,
    /// A server-qualified value disagrees with its enclosing store.
    #[error("stored field belongs to another server")]
    WrongServer,
    /// Encoded count/length or allocation exceeds its bound.
    #[error("stored field exceeds its resource limit")]
    ResourceLimit,
}

/// Explicit store-owned envelope tags. They do not encode a ledger Outcome.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u16)]
pub enum RecordKind {
    /// Server-qualified opaque directory bytes.
    Directory = 1,
    /// Ordered IPv4/IPv6 remote-access policy.
    RemoteAccess = 2,
    /// Tab identity and normalized surviving replay order.
    CreationDefault = 3,
    /// Repository scope and descriptive locators.
    RepositoryLocator = 4,
    /// Distinct device and pairing authentication public keys.
    PairingKeys = 5,
    /// One Client's immutable ordinary/restricted stream pair.
    PairingClientStreams = 6,
    /// Typed create/remove Git action.
    GitAction = 7,
    /// Pairing-owned enrollment issuer comparison fields.
    EnrollmentIssuer = 8,
    /// Exact affected Git hierarchy and fallback plans.
    GitRemovalScope = 9,
    /// Operation-bound allocated identities. No competing allocator is implied.
    GitAllocation = 10,
    /// Classified external Git completion facts.
    GitExternalResult = 11,
    /// Native management retirement descriptor.
    NativeRetirement = 12,
    /// Retirement comparison descriptor inside ledger pending external progress.
    Retirement = 13,
}
impl RecordKind {
    /// Maximum payload bytes, excluding the fixed envelope header.
    pub const fn maximum_payload(self) -> usize {
        match self {
            Self::Directory => 16 + 4 + MAX_DIRECTORY_BYTES,
            Self::RemoteAccess => 16 + 4 + MAX_REMOTE_BIND_ENDPOINTS * 27,
            Self::CreationDefault => 16 + 8 + 4 + u16::MAX as usize * 8,
            Self::RepositoryLocator => 16 + 8 + 2 * (4 + MAX_DIRECTORY_BYTES),
            Self::PairingKeys => 64,
            Self::PairingClientStreams => 48,
            Self::GitAction => 16 * 1024 - HEADER_BYTES,
            Self::EnrollmentIssuer => 160 - HEADER_BYTES,
            Self::GitExternalResult => 128 - HEADER_BYTES,
            Self::GitRemovalScope
            | Self::GitAllocation
            | Self::NativeRetirement
            | Self::Retirement => 16 * 1024 * 1024 - HEADER_BYTES,
        }
    }
}

/// Encodes the complete u64 range as an exactly eight-byte relational BLOB.
pub const fn encode_u64(value: u64) -> [u8; 8] {
    value.to_be_bytes()
}
/// Rejects every alternate-width representation instead of truncating counters.
pub fn decode_u64(bytes: &[u8]) -> Result<u64, CodecFailure> {
    Ok(u64::from_be_bytes(
        bytes
            .try_into()
            .map_err(|_| CodecFailure::InvalidEncoding)?,
    ))
}

/// Bounded encoder for store-owned record fields. Debug never prints payloads.
pub struct Writer {
    bytes: Vec<u8>,
    maximum: usize,
}
impl std::fmt::Debug for Writer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Writer(..)")
    }
}
impl Writer {
    /// Appends fixed-width bytes after checking capacity.
    pub fn fixed(&mut self, bytes: &[u8]) -> Result<(), CodecFailure> {
        let length = self
            .bytes
            .len()
            .checked_add(bytes.len())
            .filter(|length| *length <= self.maximum)
            .ok_or(CodecFailure::ResourceLimit)?;
        self.bytes
            .try_reserve(length - self.bytes.len())
            .map_err(|_| CodecFailure::ResourceLimit)?;
        self.bytes.extend_from_slice(bytes);
        Ok(())
    }
    /// Encodes a closed byte tag. The owning field codec selects the tag.
    pub fn tag(&mut self, value: u8) -> Result<(), CodecFailure> {
        self.fixed(&[value])
    }
    /// Encodes a full-width counter or numeric comparison field.
    pub fn u64(&mut self, value: u64) -> Result<(), CodecFailure> {
        self.fixed(&encode_u64(value))
    }
    /// Encodes a bounded collection count before its elements.
    pub fn count(&mut self, count: usize, maximum: usize) -> Result<(), CodecFailure> {
        if count > maximum {
            return Err(CodecFailure::ResourceLimit);
        }
        self.fixed(
            &u32::try_from(count)
                .map_err(|_| CodecFailure::ResourceLimit)?
                .to_be_bytes(),
        )
    }
    /// Encodes a length-delimited byte field without lossy text conversion.
    pub fn bytes(&mut self, bytes: &[u8], maximum: usize) -> Result<(), CodecFailure> {
        self.count(bytes.len(), maximum)?;
        self.fixed(bytes)
    }
    /// Encodes an immutable operation binding, including its canonical digest.
    pub fn operation_binding(&mut self, binding: OperationBinding) -> Result<(), CodecFailure> {
        write_stream(self, binding.id.stream())?;
        self.u64(binding.id.sequence().get())?;
        self.fixed(&binding.digest.bytes())
    }
}

/// Borrowed bounded decoder. It allocates nothing while checking framing/lengths.
#[derive(Clone, Copy)]
pub struct Reader<'a> {
    bytes: &'a [u8],
    position: usize,
}
impl std::fmt::Debug for Reader<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Reader(..)")
    }
}
impl<'a> Reader<'a> {
    /// Borrows exactly the requested bytes, rejecting truncation and overflow.
    pub fn fixed(&mut self, count: usize) -> Result<&'a [u8], CodecFailure> {
        let end = self
            .position
            .checked_add(count)
            .ok_or(CodecFailure::ResourceLimit)?;
        let value = self
            .bytes
            .get(self.position..end)
            .ok_or(CodecFailure::InvalidEncoding)?;
        self.position = end;
        Ok(value)
    }
    /// Decodes one byte. Field codecs must reject unknown tags.
    pub fn tag(&mut self) -> Result<u8, CodecFailure> {
        Ok(self.fixed(1)?[0])
    }
    /// Decodes exactly eight bytes as a full-width counter.
    pub fn u64(&mut self) -> Result<u64, CodecFailure> {
        decode_u64(self.fixed(8)?)
    }
    /// Preflights count and minimum encoded width before allocating a collection.
    pub fn count(&mut self, maximum: usize, minimum_width: usize) -> Result<usize, CodecFailure> {
        let count = u32::from_be_bytes(
            self.fixed(4)?
                .try_into()
                .map_err(|_| CodecFailure::InvalidEncoding)?,
        ) as usize;
        if count > maximum {
            return Err(CodecFailure::ResourceLimit);
        }
        let length = count
            .checked_mul(minimum_width)
            .ok_or(CodecFailure::ResourceLimit)?;
        if length > self.bytes.len() - self.position {
            return Err(CodecFailure::InvalidEncoding);
        }
        Ok(count)
    }
    /// Borrows a bounded length-delimited field. No vector is allocated here.
    pub fn bytes(&mut self, maximum: usize) -> Result<&'a [u8], CodecFailure> {
        let length = self.count(maximum, 1)?;
        self.fixed(length)
    }
    /// Decodes an operation binding through checked identity constructors.
    pub fn operation_binding(&mut self) -> Result<OperationBinding, CodecFailure> {
        let stream = read_stream(self)?;
        let sequence =
            OperationSequence::new(self.u64()?).map_err(|_| CodecFailure::InvalidEncoding)?;
        Ok(OperationBinding {
            id: OperationId::new(stream, sequence),
            digest: RequestDigest::from_bytes(array(self.fixed(32)?)?),
        })
    }
    fn finish(self) -> Result<(), CodecFailure> {
        if self.position != self.bytes.len() {
            return Err(CodecFailure::InvalidEncoding);
        }
        Ok(())
    }
}

/// Encodes store-owned fields with explicit version, kind and payload length.
/// Domain owners use the bounded writer for auxiliary fields, never serde memory layout.
pub fn encode_record(
    kind: RecordKind,
    encode: impl FnOnce(&mut Writer) -> Result<(), CodecFailure>,
) -> Result<Vec<u8>, CodecFailure> {
    let mut writer = Writer {
        bytes: Vec::new(),
        maximum: kind.maximum_payload() + HEADER_BYTES,
    };
    writer.fixed(MAGIC)?;
    writer.fixed(&VERSION.to_be_bytes())?;
    writer.fixed(&(kind as u16).to_be_bytes())?;
    writer.fixed(&[0; 4])?;
    encode(&mut writer)?;
    let length = u32::try_from(writer.bytes.len() - HEADER_BYTES)
        .map_err(|_| CodecFailure::ResourceLimit)?;
    writer.bytes[8..12].copy_from_slice(&length.to_be_bytes());
    Ok(writer.bytes)
}

/// Validates framing and complete consumption before returning decoded fields.
/// The owning auxiliary codec must additionally run its checked core constructor.
pub fn decode_record<T>(
    kind: RecordKind,
    bytes: &[u8],
    decode: impl FnOnce(&mut Reader<'_>) -> Result<T, CodecFailure>,
) -> Result<T, CodecFailure> {
    if bytes.len() > kind.maximum_payload() + HEADER_BYTES {
        return Err(CodecFailure::ResourceLimit);
    }
    let mut header = Reader { bytes, position: 0 };
    if header.fixed(4)? != MAGIC {
        return Err(CodecFailure::InvalidEncoding);
    }
    if header.fixed(2)? != VERSION.to_be_bytes() {
        return Err(CodecFailure::UnsupportedVersion);
    }
    if header.fixed(2)? != (kind as u16).to_be_bytes() {
        return Err(CodecFailure::InvalidEncoding);
    }
    let length = header.count(kind.maximum_payload(), 1)?;
    if length != bytes.len() - HEADER_BYTES {
        return Err(CodecFailure::InvalidEncoding);
    }
    let mut reader = Reader {
        bytes: header.fixed(length)?,
        position: 0,
    };
    let value = decode(&mut reader)?;
    reader.finish()?;
    Ok(value)
}

fn array<const N: usize>(bytes: &[u8]) -> Result<[u8; N], CodecFailure> {
    bytes.try_into().map_err(|_| CodecFailure::InvalidEncoding)
}
fn copy_bytes(bytes: &[u8]) -> Result<Vec<u8>, CodecFailure> {
    let mut result = Vec::new();
    result
        .try_reserve_exact(bytes.len())
        .map_err(|_| CodecFailure::ResourceLimit)?;
    result.extend_from_slice(bytes);
    Ok(result)
}
fn server(reader: &mut Reader<'_>, expected: ServerId) -> Result<ServerId, CodecFailure> {
    let server = ServerId::new(array(reader.fixed(16)?)?);
    if server != expected {
        return Err(CodecFailure::WrongServer);
    }
    Ok(server)
}
fn text<const MAX: usize>(reader: &mut Reader<'_>) -> Result<BoundedText<MAX>, CodecFailure> {
    let bytes = reader.bytes(MAX)?;
    let value = std::str::from_utf8(bytes).map_err(|_| CodecFailure::InvalidEncoding)?;
    BoundedText::new(
        String::from_utf8(copy_bytes(value.as_bytes())?)
            .map_err(|_| CodecFailure::InvalidEncoding)?,
    )
    .map_err(|_| CodecFailure::InvalidEncoding)
}

/// Encodes one opaque server directory without converting its native bytes.
pub fn encode_directory(value: &ServerDirectory) -> Result<Vec<u8>, CodecFailure> {
    encode_record(RecordKind::Directory, |writer| {
        writer.fixed(&value.server().bytes())?;
        writer.bytes(value.bytes(), MAX_DIRECTORY_BYTES)
    })
}
/// Decodes and checks the exact server scope and native directory representation.
pub fn decode_directory(bytes: &[u8], expected: ServerId) -> Result<ServerDirectory, CodecFailure> {
    decode_record(RecordKind::Directory, bytes, |reader| {
        let server = server(reader, expected)?;
        ServerDirectory::new(server, copy_bytes(reader.bytes(MAX_DIRECTORY_BYTES)?)?)
            .map_err(|_| CodecFailure::InvalidEncoding)
    })
}

/// Encodes surviving replay order exactly. It never sorts by creation ordinal.
pub fn encode_creation_default(value: &TabCreationDefault) -> Result<Vec<u8>, CodecFailure> {
    encode_record(RecordKind::CreationDefault, |writer| {
        writer.fixed(&value.tab().server().bytes())?;
        writer.u64(value.tab().local())?;
        writer.count(value.replay_order().len(), u16::MAX as usize)?;
        for pane in value.replay_order() {
            writer.u64(pane.local())?;
        }
        Ok(())
    })
}
/// Preflights the complete replay count before allocating or indexing its Panes.
pub fn decode_creation_default(
    bytes: &[u8],
    expected: ServerId,
) -> Result<TabCreationDefault, CodecFailure> {
    decode_record(RecordKind::CreationDefault, bytes, |reader| {
        let server = server(reader, expected)?;
        let tab = TabId::new(server, reader.u64()?).map_err(|_| CodecFailure::InvalidEncoding)?;
        let count = reader.count(u16::MAX as usize, 8)?;
        let mut panes = Vec::new();
        panes
            .try_reserve_exact(count)
            .map_err(|_| CodecFailure::ResourceLimit)?;
        for _ in 0..count {
            panes.push(
                PaneId::new(server, reader.u64()?).map_err(|_| CodecFailure::InvalidEncoding)?,
            );
        }
        TabCreationDefault::new(tab, panes).map_err(|_| CodecFailure::InvalidEncoding)
    })
}

/// Converts the closed relational PaneCreation tags without retaining ancestry.
pub fn decode_pane_creation(
    server: ServerId,
    kind: u8,
    source: Option<&[u8]>,
    axis: Option<u8>,
    edge: Option<u8>,
) -> Result<PaneCreation, CodecFailure> {
    match (kind, source, axis, edge) {
        (0, None, None, None) => Ok(PaneCreation::Initial),
        (1, Some(source), Some(axis), Some(edge)) => Ok(PaneCreation::SplitOf {
            source: PaneId::new(server, decode_u64(source)?)
                .map_err(|_| CodecFailure::InvalidEncoding)?,
            axis: match axis {
                0 => SplitAxis::Horizontal,
                1 => SplitAxis::Vertical,
                _ => return Err(CodecFailure::InvalidEncoding),
            },
            edge: match edge {
                0 => SplitEdge::Before,
                1 => SplitEdge::After,
                _ => return Err(CodecFailure::InvalidEncoding),
            },
        }),
        _ => Err(CodecFailure::InvalidEncoding),
    }
}

/// Preserves endpoint order, address family, IPv6 flowinfo and scope ID.
pub fn encode_remote_access(value: &RemoteAccessPolicy) -> Result<Vec<u8>, CodecFailure> {
    encode_record(RecordKind::RemoteAccess, |writer| {
        writer.fixed(&value.server().bytes())?;
        writer.count(value.endpoints().len(), MAX_REMOTE_BIND_ENDPOINTS)?;
        for endpoint in value.endpoints() {
            match endpoint {
                SocketAddr::V4(value) => {
                    writer.tag(4)?;
                    writer.fixed(&value.ip().octets())?;
                    writer.fixed(&value.port().to_be_bytes())?;
                }
                SocketAddr::V6(value) => {
                    writer.tag(6)?;
                    writer.fixed(&value.ip().octets())?;
                    writer.fixed(&value.port().to_be_bytes())?;
                    writer.fixed(&value.flowinfo().to_be_bytes())?;
                    writer.fixed(&value.scope_id().to_be_bytes())?;
                }
            }
        }
        Ok(())
    })
}
/// Applies the domain's eight-endpoint bound, not storage's sixteen-row headroom.
pub fn decode_remote_access(
    bytes: &[u8],
    expected: ServerId,
) -> Result<RemoteAccessPolicy, CodecFailure> {
    decode_record(RecordKind::RemoteAccess, bytes, |reader| {
        let server = server(reader, expected)?;
        let count = reader.count(MAX_REMOTE_BIND_ENDPOINTS, 7)?;
        let mut endpoints = Vec::new();
        endpoints
            .try_reserve_exact(count)
            .map_err(|_| CodecFailure::ResourceLimit)?;
        for _ in 0..count {
            endpoints.push(match reader.tag()? {
                4 => SocketAddr::V4(SocketAddrV4::new(
                    Ipv4Addr::from(array::<4>(reader.fixed(4)?)?),
                    u16::from_be_bytes(array(reader.fixed(2)?)?),
                )),
                6 => SocketAddr::V6(SocketAddrV6::new(
                    Ipv6Addr::from(array::<16>(reader.fixed(16)?)?),
                    u16::from_be_bytes(array(reader.fixed(2)?)?),
                    u32::from_be_bytes(array(reader.fixed(4)?)?),
                    u32::from_be_bytes(array(reader.fixed(4)?)?),
                )),
                _ => return Err(CodecFailure::InvalidEncoding),
            });
        }
        if endpoints.is_empty() {
            return Ok(RemoteAccessPolicy::disabled(server));
        }
        RemoteAccessPolicy::enabled(server, endpoints).map_err(|_| CodecFailure::InvalidEncoding)
    })
}

fn write_stream(writer: &mut Writer, stream: OperationStreamId) -> Result<(), CodecFailure> {
    writer.fixed(&stream.server().bytes())?;
    writer.u64(stream.key().local())?;
    writer.u64(stream.pairing().local())?;
    writer.u64(stream.client().local())
}
fn read_stream(reader: &mut Reader<'_>) -> Result<OperationStreamId, CodecFailure> {
    let server = ServerId::new(array(reader.fixed(16)?)?);
    let key = OperationStreamKey::new(server, reader.u64()?)
        .map_err(|_| CodecFailure::InvalidEncoding)?;
    let pairing =
        PairingId::new(server, reader.u64()?).map_err(|_| CodecFailure::InvalidEncoding)?;
    let client = ClientId::new(server, reader.u64()?).map_err(|_| CodecFailure::InvalidEncoding)?;
    OperationStreamId::new(key, pairing, client).map_err(|_| CodecFailure::InvalidEncoding)
}

/// Keeps both public-key roles distinct and rejects collapsed keys on encode.
pub fn encode_pairing_keys(value: &PairingKeys) -> Result<Vec<u8>, CodecFailure> {
    value
        .validate()
        .map_err(|_| CodecFailure::InvalidEncoding)?;
    encode_record(RecordKind::PairingKeys, |writer| {
        writer.fixed(&value.parts().device_identity_public_key)?;
        writer.fixed(&value.parts().pairing_authentication_public_key)
    })
}
/// Uses the pairing owner's checked constructor, without authenticating the keys.
pub fn decode_pairing_keys(bytes: &[u8]) -> Result<PairingKeys, CodecFailure> {
    decode_record(RecordKind::PairingKeys, bytes, |reader| {
        PairingKeys::new(PairingKeysParts {
            device_identity_public_key: array(reader.fixed(32)?)?,
            pairing_authentication_public_key: array(reader.fixed(32)?)?,
        })
        .map_err(|_| CodecFailure::InvalidEncoding)
    })
}

/// Encodes one distinct Client binding, never a device-global restricted stream.
pub fn encode_pairing_client_streams(
    value: &PairingClientStreams,
) -> Result<Vec<u8>, CodecFailure> {
    value
        .validate()
        .map_err(|_| CodecFailure::InvalidEncoding)?;
    let value = value.parts();
    encode_record(RecordKind::PairingClientStreams, |writer| {
        writer.fixed(&value.client.id.server().bytes())?;
        writer.u64(value.client.pairing.local())?;
        writer.u64(value.client.id.local())?;
        writer.u64(value.ordinary.key().local())?;
        writer.u64(value.self_revocation.key().local())
    })
}
/// Reconstructs both exact immutable stream descriptors from their retained keys.
pub fn decode_pairing_client_streams(
    bytes: &[u8],
    expected: ServerId,
) -> Result<PairingClientStreams, CodecFailure> {
    decode_record(RecordKind::PairingClientStreams, bytes, |reader| {
        let server = server(reader, expected)?;
        let pairing =
            PairingId::new(server, reader.u64()?).map_err(|_| CodecFailure::InvalidEncoding)?;
        let client =
            ClientId::new(server, reader.u64()?).map_err(|_| CodecFailure::InvalidEncoding)?;
        let mut stream = || {
            let key = OperationStreamKey::new(server, reader.u64()?)
                .map_err(|_| CodecFailure::InvalidEncoding)?;
            OperationStreamId::new(key, pairing, client).map_err(|_| CodecFailure::InvalidEncoding)
        };
        let ordinary = stream()?;
        let self_revocation = stream()?;
        PairingClientStreams::new(PairingClientStreamsParts {
            client: ClientRecord {
                id: client,
                pairing,
            },
            ordinary,
            self_revocation,
        })
        .map_err(|_| CodecFailure::InvalidEncoding)
    })
}

/// Encodes descriptive repository locators. No filesystem proof is persisted.
pub fn encode_repository_locator(value: &RepositoryLocator) -> Result<Vec<u8>, CodecFailure> {
    encode_record(RecordKind::RepositoryLocator, |writer| {
        writer.fixed(&value.scope().server.bytes())?;
        writer.u64(value.scope().repository.local())?;
        writer.bytes(value.parts().main_root.bytes(), MAX_DIRECTORY_BYTES)?;
        writer.bytes(value.parts().common_directory.bytes(), MAX_DIRECTORY_BYTES)
    })
}
/// Checks scope with the Git owner's constructor, without stat or Git execution.
pub fn decode_repository_locator(
    bytes: &[u8],
    expected: ServerId,
) -> Result<RepositoryLocator, CodecFailure> {
    decode_record(RecordKind::RepositoryLocator, bytes, |reader| {
        let server = server(reader, expected)?;
        let repository =
            RepositoryId::new(server, reader.u64()?).map_err(|_| CodecFailure::InvalidEncoding)?;
        let mut directory = || {
            ServerDirectory::new(server, copy_bytes(reader.bytes(MAX_DIRECTORY_BYTES)?)?)
                .map_err(|_| CodecFailure::InvalidEncoding)
        };
        let main_root = directory()?;
        let common_directory = directory()?;
        RepositoryLocator::new(RepositoryLocatorParts {
            scope: RepositoryScope { server, repository },
            main_root,
            common_directory,
        })
        .map_err(|_| CodecFailure::InvalidEncoding)
    })
}

fn write_branch(writer: &mut Writer, value: &BranchName) -> Result<(), CodecFailure> {
    writer.bytes(value.as_str().as_bytes(), MAX_NAME_BYTES)
}
fn write_reference(writer: &mut Writer, value: &BranchReference) -> Result<(), CodecFailure> {
    match value {
        BranchReference::Local(name) => {
            writer.tag(0)?;
            write_branch(writer, name)
        }
        BranchReference::Remote(name) => {
            writer.tag(1)?;
            write_branch(writer, name)
        }
    }
}
fn read_reference(reader: &mut Reader<'_>) -> Result<BranchReference, CodecFailure> {
    match reader.tag()? {
        0 => Ok(BranchReference::Local(text(reader)?)),
        1 => Ok(BranchReference::Remote(text(reader)?)),
        _ => Err(CodecFailure::InvalidEncoding),
    }
}

/// Encodes typed Git operands. Shell commands, argv and environment are absent.
pub fn encode_git_action(value: &RetainedGitAction) -> Result<Vec<u8>, CodecFailure> {
    value
        .validate_scope(value.worktree().server())
        .map_err(|_| CodecFailure::WrongServer)?;
    encode_record(RecordKind::GitAction, |writer| {
        writer.fixed(&value.worktree().server().bytes())?;
        writer.u64(value.worktree().local())?;
        writer.bytes(value.directory().bytes(), MAX_DIRECTORY_BYTES)?;
        match value {
            RetainedGitAction::Create {
                branch,
                target,
                template_version,
                ..
            } => {
                writer.tag(0)?;
                match branch {
                    WorktreeBranchChoice::New { name, base } => {
                        writer.tag(0)?;
                        write_branch(writer, name)?;
                        write_reference(writer, base)?;
                    }
                    WorktreeBranchChoice::Existing { name } => {
                        writer.tag(1)?;
                        write_branch(writer, name)?;
                    }
                    WorktreeBranchChoice::Remote { remote, name } => {
                        writer.tag(2)?;
                        write_branch(writer, remote)?;
                        write_branch(writer, name)?;
                    }
                }
                writer.tag(match target {
                    CreationTargetObservation::Absent => 0,
                    CreationTargetObservation::EmptyDirectory => 1,
                })?;
                match template_version {
                    None => writer.tag(0),
                    Some(version) => {
                        writer.tag(1)?;
                        writer.u64(version.get())
                    }
                }
            }
            RetainedGitAction::Remove { head, approval, .. } => {
                writer.tag(1)?;
                write_head(writer, head.as_ref())?;
                writer.tag(match approval {
                    ApprovedRemoval::Clean => 0,
                    ApprovedRemoval::DiscardChanges => 1,
                    ApprovedRemoval::DeleteSubmoduleCheckouts => 2,
                    ApprovedRemoval::DiscardChangesAndDeleteSubmoduleCheckouts => 3,
                    ApprovedRemoval::Missing => 4,
                })
            }
        }
    })
}
/// Rejects unknown action, branch, target, optional-value and force-policy tags.
pub fn decode_git_action(
    bytes: &[u8],
    expected: ServerId,
) -> Result<RetainedGitAction, CodecFailure> {
    decode_record(RecordKind::GitAction, bytes, |reader| {
        let server = server(reader, expected)?;
        let worktree =
            WorktreeId::new(server, reader.u64()?).map_err(|_| CodecFailure::InvalidEncoding)?;
        let directory =
            ServerDirectory::new(server, copy_bytes(reader.bytes(MAX_DIRECTORY_BYTES)?)?)
                .map_err(|_| CodecFailure::InvalidEncoding)?;
        match reader.tag()? {
            0 => {
                let branch = match reader.tag()? {
                    0 => WorktreeBranchChoice::New {
                        name: text(reader)?,
                        base: read_reference(reader)?,
                    },
                    1 => WorktreeBranchChoice::Existing {
                        name: text(reader)?,
                    },
                    2 => WorktreeBranchChoice::Remote {
                        remote: text(reader)?,
                        name: text(reader)?,
                    },
                    _ => return Err(CodecFailure::InvalidEncoding),
                };
                let target = match reader.tag()? {
                    0 => CreationTargetObservation::Absent,
                    1 => CreationTargetObservation::EmptyDirectory,
                    _ => return Err(CodecFailure::InvalidEncoding),
                };
                let template_version = match reader.tag()? {
                    0 => None,
                    1 => Some(FieldVersion::new(reader.u64()?)),
                    _ => return Err(CodecFailure::InvalidEncoding),
                };
                Ok(RetainedGitAction::Create {
                    worktree,
                    directory,
                    branch,
                    target,
                    template_version,
                })
            }
            1 => {
                let head = read_head(reader)?;
                let approval = match reader.tag()? {
                    0 => ApprovedRemoval::Clean,
                    1 => ApprovedRemoval::DiscardChanges,
                    2 => ApprovedRemoval::DeleteSubmoduleCheckouts,
                    3 => ApprovedRemoval::DiscardChangesAndDeleteSubmoduleCheckouts,
                    4 => ApprovedRemoval::Missing,
                    _ => return Err(CodecFailure::InvalidEncoding),
                };
                Ok(RetainedGitAction::Remove {
                    worktree,
                    directory,
                    head,
                    approval,
                })
            }
            _ => Err(CodecFailure::InvalidEncoding),
        }
    })
}

fn write_object(writer: &mut Writer, value: &GitObjectId) -> Result<(), CodecFailure> {
    match value {
        GitObjectId::Sha1(bytes) => {
            writer.tag(0)?;
            writer.fixed(bytes)
        }
        GitObjectId::Sha256(bytes) => {
            writer.tag(1)?;
            writer.fixed(bytes)
        }
    }
}
fn read_object(reader: &mut Reader<'_>) -> Result<GitObjectId, CodecFailure> {
    match reader.tag()? {
        0 => Ok(GitObjectId::Sha1(array(reader.fixed(20)?)?)),
        1 => Ok(GitObjectId::Sha256(array(reader.fixed(32)?)?)),
        _ => Err(CodecFailure::InvalidEncoding),
    }
}
fn write_head(writer: &mut Writer, value: Option<&RetainedGitHead>) -> Result<(), CodecFailure> {
    match value {
        None => writer.tag(0),
        Some(RetainedGitHead::Branch { name, commit }) => {
            writer.tag(1)?;
            write_branch(writer, name)?;
            write_object(writer, commit)
        }
        Some(RetainedGitHead::Detached(commit)) => {
            writer.tag(2)?;
            write_object(writer, commit)
        }
        Some(RetainedGitHead::Unborn(name)) => {
            writer.tag(3)?;
            write_branch(writer, name)
        }
    }
}
fn read_head(reader: &mut Reader<'_>) -> Result<Option<RetainedGitHead>, CodecFailure> {
    match reader.tag()? {
        0 => Ok(None),
        1 => Ok(Some(RetainedGitHead::Branch {
            name: text(reader)?,
            commit: read_object(reader)?,
        })),
        2 => Ok(Some(RetainedGitHead::Detached(read_object(reader)?))),
        3 => Ok(Some(RetainedGitHead::Unborn(text(reader)?))),
        _ => Err(CodecFailure::InvalidEncoding),
    }
}

fn local<T: LocalIdentity>(reader: &mut Reader<'_>, server: ServerId) -> Result<T, CodecFailure> {
    let value = NonZeroU64::new(reader.u64()?).ok_or(CodecFailure::InvalidEncoding)?;
    Ok(T::from_local(server, value))
}
fn optional_local<T: LocalIdentity>(
    reader: &mut Reader<'_>,
    server: ServerId,
) -> Result<Option<T>, CodecFailure> {
    match reader.tag()? {
        0 => Ok(None),
        1 => Ok(Some(local(reader, server)?)),
        _ => Err(CodecFailure::InvalidEncoding),
    }
}
fn write_optional_local(writer: &mut Writer, value: Option<u64>) -> Result<(), CodecFailure> {
    match value {
        None => writer.tag(0),
        Some(value) => {
            writer.tag(1)?;
            writer.u64(value)
        }
    }
}
fn vector<T>(
    reader: &mut Reader<'_>,
    max: usize,
    min_width: usize,
    mut decode: impl FnMut(&mut Reader<'_>) -> Result<T, CodecFailure>,
) -> Result<Vec<T>, CodecFailure> {
    let count = reader.count(max, min_width)?;
    let mut values = Vec::new();
    values
        .try_reserve_exact(count)
        .map_err(|_| CodecFailure::ResourceLimit)?;
    for _ in 0..count {
        values.push(decode(reader)?);
    }
    Ok(values)
}

/// Encodes only issuer comparison data, never bearer enrollment authority.
pub fn encode_enrollment_issuer(value: &EnrollmentIssuer) -> Result<Vec<u8>, CodecFailure> {
    encode_record(RecordKind::EnrollmentIssuer, |writer| match value {
        EnrollmentIssuer::Paired {
            operation,
            authorization_version,
        } => {
            if authorization_version.get() == 0 {
                return Err(CodecFailure::InvalidEncoding);
            }
            writer.tag(0)?;
            writer.operation_binding(*operation)?;
            writer.u64(authorization_version.get())
        }
        EnrollmentIssuer::AccountOwner {
            native_request,
            native_request_digest,
        } => {
            if *native_request == [0; 16] {
                return Err(CodecFailure::InvalidEncoding);
            }
            writer.tag(1)?;
            writer.fixed(native_request)?;
            writer.fixed(&native_request_digest.bytes())
        }
    })
}
/// Rejects a foreign paired issuer and a fabricated zero native request identity.
pub fn decode_enrollment_issuer(
    bytes: &[u8],
    expected: ServerId,
) -> Result<EnrollmentIssuer, CodecFailure> {
    decode_record(RecordKind::EnrollmentIssuer, bytes, |reader| {
        match reader.tag()? {
            0 => {
                let operation = reader.operation_binding()?;
                if operation.id.stream().server() != expected {
                    return Err(CodecFailure::WrongServer);
                }
                let authorization_version = FieldVersion::new(reader.u64()?);
                if authorization_version.get() == 0 {
                    return Err(CodecFailure::InvalidEncoding);
                }
                Ok(EnrollmentIssuer::Paired {
                    operation,
                    authorization_version,
                })
            }
            1 => {
                let native_request = array(reader.fixed(16)?)?;
                if native_request == [0; 16] {
                    return Err(CodecFailure::InvalidEncoding);
                }
                Ok(EnrollmentIssuer::AccountOwner {
                    native_request,
                    native_request_digest: RequestDigest::from_bytes(array(reader.fixed(32)?)?),
                })
            }
            _ => Err(CodecFailure::InvalidEncoding),
        }
    })
}

fn write_marks(writer: &mut Writer, marks: IdentityHighWater) -> Result<(), CodecFailure> {
    for value in [
        marks.workspace,
        marks.tab,
        marks.pane,
        marks.worktree,
        marks.repository,
        marks.pairing,
        marks.client,
        marks.operation_stream,
        marks.creation_ordinal,
    ] {
        writer.u64(value)?;
    }
    Ok(())
}
fn read_marks(reader: &mut Reader<'_>) -> Result<IdentityHighWater, CodecFailure> {
    Ok(IdentityHighWater {
        workspace: reader.u64()?,
        tab: reader.u64()?,
        pane: reader.u64()?,
        worktree: reader.u64()?,
        repository: reader.u64()?,
        pairing: reader.u64()?,
        client: reader.u64()?,
        operation_stream: reader.u64()?,
        creation_ordinal: reader.u64()?,
    })
}
fn write_fallback(writer: &mut Writer, plan: &WorkspaceFallbackPlan) -> Result<(), CodecFailure> {
    writer.u64(plan.workspace.local())?;
    writer.u64(plan.worktree.local())?;
    writer.bytes(plan.directory.bytes(), MAX_DIRECTORY_BYTES)
}
fn read_fallback(
    reader: &mut Reader<'_>,
    server: ServerId,
) -> Result<WorkspaceFallbackPlan, CodecFailure> {
    Ok(WorkspaceFallbackPlan {
        workspace: local(reader, server)?,
        worktree: local(reader, server)?,
        directory: ServerDirectory::new(server, copy_bytes(reader.bytes(MAX_DIRECTORY_BYTES)?)?)
            .map_err(|_| CodecFailure::InvalidEncoding)?,
    })
}

/// Preserves exact operation-bound allocated IDs and both original high-water sets.
pub fn encode_git_allocation(value: &GitAllocationReservation) -> Result<Vec<u8>, CodecFailure> {
    let value = value.parts();
    encode_record(RecordKind::GitAllocation, |writer| {
        writer.operation_binding(value.binding)?;
        write_marks(writer, value.expected_high_water)?;
        write_marks(writer, value.resulting_high_water)?;
        write_optional_local(writer, value.repository.map(|id| id.local()))?;
        write_optional_local(writer, value.worktree.map(|id| id.local()))?;
        writer.count(value.fallbacks.len(), MAX_GIT_TABS)?;
        for fallback in &value.fallbacks {
            write_fallback(writer, &fallback.plan)?;
            writer.u64(fallback.tab.local())?;
            writer.u64(fallback.pane.local())?;
            writer.u64(fallback.tab_creation_ordinal.get())?;
            writer.u64(fallback.pane_creation_ordinal.get())?;
        }
        Ok(())
    })
}
/// Runs the Git owner's reservation constructor. Decoding allocates no identities.
pub fn decode_git_allocation(
    bytes: &[u8],
    expected: ServerId,
) -> Result<GitAllocationReservation, CodecFailure> {
    decode_record(RecordKind::GitAllocation, bytes, |reader| {
        let binding = reader.operation_binding()?;
        if binding.id.stream().server() != expected {
            return Err(CodecFailure::WrongServer);
        }
        let expected_high_water = read_marks(reader)?;
        let resulting_high_water = read_marks(reader)?;
        let repository = optional_local(reader, expected)?;
        let worktree = optional_local(reader, expected)?;
        let fallbacks = vector(reader, MAX_GIT_TABS, 53, |reader| {
            Ok(GitReservedFallback {
                plan: read_fallback(reader, expected)?,
                tab: local(reader, expected)?,
                pane: local(reader, expected)?,
                tab_creation_ordinal: CreationOrdinal::new(reader.u64()?),
                pane_creation_ordinal: CreationOrdinal::new(reader.u64()?),
            })
        })?;
        GitAllocationReservation::new(GitAllocationReservationParts {
            binding,
            expected_high_water,
            resulting_high_water,
            repository,
            worktree,
            fallbacks,
        })
        .map_err(|_| CodecFailure::InvalidEncoding)
    })
}

fn write_session(writer: &mut Writer, value: SessionKey) -> Result<(), CodecFailure> {
    writer.u64(value.epoch.get())?;
    writer.u64(value.pane.local())?;
    writer.u64(value.generation.get())
}
fn read_session(reader: &mut Reader<'_>, server: ServerId) -> Result<SessionKey, CodecFailure> {
    let epoch = ServerEpoch::new(reader.u64()?);
    let pane = local(reader, server)?;
    let generation = SessionGeneration::new(reader.u64()?);
    if epoch.get() == 0 || generation.get() == 0 {
        return Err(CodecFailure::InvalidEncoding);
    }
    Ok(SessionKey {
        epoch,
        pane,
        generation,
    })
}
fn write_tab(writer: &mut Writer, value: &TabRecord) -> Result<(), CodecFailure> {
    writer.u64(value.id.local())?;
    writer.u64(value.workspace.local())?;
    write_optional_local(writer, value.worktree.map(|id| id.local()))?;
    writer.u64(value.root_pane.local())?;
    writer.u64(value.creation_ordinal.get())
}
fn read_tab(reader: &mut Reader<'_>, server: ServerId) -> Result<TabRecord, CodecFailure> {
    Ok(TabRecord {
        id: local(reader, server)?,
        workspace: local(reader, server)?,
        worktree: optional_local(reader, server)?,
        root_pane: local(reader, server)?,
        creation_ordinal: CreationOrdinal::new(reader.u64()?),
    })
}

/// Encodes exact all-Workspace removal scope, without attachments or native proofs.
pub fn encode_git_removal_scope(value: &RetainedRemovalScope) -> Result<Vec<u8>, CodecFailure> {
    let value = value.parts();
    encode_record(RecordKind::GitRemovalScope, |writer| {
        writer.fixed(&value.server.bytes())?;
        writer.count(value.tabs.len(), MAX_GIT_TABS)?;
        for tab in &value.tabs {
            write_tab(writer, tab)?;
        }
        writer.count(value.panes.len(), MAX_GIT_PANES)?;
        for pane in &value.panes {
            let h = pane.hierarchy;
            for number in [
                h.workspace.local(),
                h.tab.local(),
                h.pane.local(),
                h.session_generation.get(),
                h.control_generation.get(),
                h.context_generation.get(),
            ] {
                writer.u64(number)?;
            }
            match pane.session {
                None => writer.tag(0)?,
                Some(session) => {
                    writer.tag(1)?;
                    write_session(writer, session)?;
                }
            }
            writer.tag(u8::from(pane.startup_reserved))?;
            writer.u64(pane.work_generation.get())?;
        }
        writer.count(value.fallbacks.len(), MAX_GIT_TABS)?;
        for fallback in &value.fallbacks {
            write_fallback(writer, fallback)?;
        }
        Ok(())
    })
}
/// Preflights each bounded collection and validates exact scope with the Git owner.
pub fn decode_git_removal_scope(
    bytes: &[u8],
    expected: ServerId,
) -> Result<RetainedRemovalScope, CodecFailure> {
    decode_record(RecordKind::GitRemovalScope, bytes, |reader| {
        let server = server(reader, expected)?;
        let tabs = vector(reader, MAX_GIT_TABS, 33, |reader| read_tab(reader, server))?;
        let panes = vector(reader, MAX_GIT_PANES, 58, |reader| {
            let hierarchy = AssessedPane {
                workspace: local(reader, server)?,
                tab: local(reader, server)?,
                pane: local(reader, server)?,
                session_generation: SessionGeneration::new(reader.u64()?),
                control_generation: ControlGeneration::new(reader.u64()?),
                context_generation: ContextGeneration::new(reader.u64()?),
            };
            let session = match reader.tag()? {
                0 => None,
                1 => Some(read_session(reader, server)?),
                _ => return Err(CodecFailure::InvalidEncoding),
            };
            let startup_reserved = match reader.tag()? {
                0 => false,
                1 => true,
                _ => return Err(CodecFailure::InvalidEncoding),
            };
            Ok(RetainedAffectedPane {
                hierarchy,
                session,
                startup_reserved,
                work_generation: CloseWorkGeneration::new(reader.u64()?),
            })
        })?;
        let fallbacks = vector(reader, MAX_GIT_TABS, 21, |reader| {
            read_fallback(reader, server)
        })?;
        RetainedRemovalScope::new(RetainedRemovalScopeParts {
            server,
            tabs,
            panes,
            fallbacks,
        })
        .map_err(|_| CodecFailure::InvalidEncoding)
    })
}

macro_rules! tags {
    ($encode:ident, $decode:ident, $type:ident, {$($tag:literal => $variant:ident),+ $(,)?}) => {
        fn $encode(value: $type) -> u8 { match value { $($type::$variant => $tag),+ } }
        fn $decode(tag: u8) -> Result<$type, CodecFailure> { match tag { $($tag => Ok($type::$variant)),+, _ => Err(CodecFailure::InvalidEncoding) } }
    };
}
tags!(failure_tag, read_failure, FailureClass, { 0 => LaunchUnavailable, 1 => RuntimeUnavailable, 2 => RuntimeFault, 3 => ResourceLimit, 4 => StorageUnavailable, 5 => UnsafeStorage, 6 => AuthorizationDenied, 7 => DirectoryUnavailable, 8 => InvalidRequest, 9 => StaleAuthority, 10 => Unsupported, 11 => Internal });
tags!(retirement_reason_tag, read_retirement_reason, RetirementReason, { 0 => OrdinaryExit, 1 => EndAllSessions, 2 => Stop, 3 => RuntimeFailure, 4 => ConfirmedClose, 5 => KillWorkspace, 6 => WorktreeRemoval, 7 => ServerUpdate, 8 => ServerRecovery });
tags!(external_tag, read_external, ExternalDisposition, { 0 => NoEffectEstablished, 1 => DesiredStateObserved, 2 => PartialOrAmbiguous, 3 => Unavailable });
tags!(git_failure_tag, read_git_failure, GitFailure, {
    0 => Unauthorized, 1 => WrongServer, 2 => TargetGone, 3 => Offline, 4 => Busy, 5 => Capacity, 6 => ExpiredAssessment, 7 => StaleAssessment, 8 => MainWorktree, 9 => LockedWorktree, 10 => UnlistedWorktree, 11 => HoldsPinnedDirectory, 12 => NoFallback, 13 => InvalidBranch, 14 => InvalidTemplate, 15 => InvalidDirectory, 16 => OccupiedTarget, 17 => BranchExists, 18 => BranchMissing, 19 => BranchCheckedOut, 20 => IdentityReplaced, 21 => GitStateChanged, 22 => Unchecked, 23 => ToolMissing, 24 => ToolTooOld, 25 => CancelledRead, 26 => TimedOutRead, 27 => InvalidResponse, 28 => OutputTooLarge, 29 => WriteFailed, 30 => ExternalStateUncertain, 31 => RecoveryNeedsReview, 32 => StorageLocked, 33 => StorageUnavailable, 34 => UnsafeStorage, 35 => StorageFailure, 36 => IndeterminateDurability
});

/// Encodes classified external observations without turning them into success.
pub fn encode_git_external_result(value: &RetainedExternalResult) -> Result<Vec<u8>, CodecFailure> {
    if value.ended_sessions as usize > MAX_GIT_PANES {
        return Err(CodecFailure::ResourceLimit);
    }
    encode_record(RecordKind::GitExternalResult, |writer| {
        writer.tag(external_tag(value.disposition))?;
        match value.failure {
            None => writer.tag(0)?,
            Some(failure) => {
                writer.tag(1)?;
                writer.tag(git_failure_tag(failure))?;
            }
        }
        writer.fixed(&value.ended_sessions.to_be_bytes())
    })
}
/// Decodes content-free observation classes and checks the ended-Pane bound.
pub fn decode_git_external_result(bytes: &[u8]) -> Result<RetainedExternalResult, CodecFailure> {
    decode_record(RecordKind::GitExternalResult, bytes, |reader| {
        let disposition = read_external(reader.tag()?)?;
        let failure = match reader.tag()? {
            0 => None,
            1 => Some(read_git_failure(reader.tag()?)?),
            _ => return Err(CodecFailure::InvalidEncoding),
        };
        let ended_sessions = u32::from_be_bytes(array(reader.fixed(4)?)?);
        if ended_sessions as usize > MAX_GIT_PANES {
            return Err(CodecFailure::ResourceLimit);
        }
        Ok(RetainedExternalResult {
            disposition,
            failure,
            ended_sessions,
        })
    })
}

fn write_retirement_pane(writer: &mut Writer, pane: RetirementPane) -> Result<(), CodecFailure> {
    writer.u64(pane.workspace().local())?;
    writer.u64(pane.tab().local())?;
    writer.u64(pane.pane().local())?;
    write_optional_local(writer, pane.worktree().map(|id| id.local()))?;
    writer.u64(pane.invalidation_generation().get())
}
fn read_retirement_pane(
    reader: &mut Reader<'_>,
    server: ServerId,
) -> Result<RetirementPane, CodecFailure> {
    RetirementPane::new(
        local(reader, server)?,
        local(reader, server)?,
        local(reader, server)?,
        optional_local(reader, server)?,
        SessionGeneration::new(reader.u64()?),
    )
    .map_err(|_| CodecFailure::InvalidEncoding)
}
fn write_retired_session(writer: &mut Writer, session: RetiredSession) -> Result<(), CodecFailure> {
    write_session(writer, session.session())?;
    writer.tag(retirement_reason_tag(session.first_reason()))?;
    match session.cleanup() {
        RetirementCleanupState::AwaitingEvidence => writer.tag(0),
        RetirementCleanupState::Confirmed(class) => {
            writer.tag(1)?;
            writer.tag(match class {
                ConfirmedCleanupClass::ReapedAndReleased => 0,
                ConfirmedCleanupClass::AlreadyAbsentWithConfirmedOwnership => 1,
            })
        }
        RetirementCleanupState::Failed(class) => {
            writer.tag(2)?;
            writer.tag(failure_tag(class))
        }
    }
}
fn read_retired_session(
    reader: &mut Reader<'_>,
    server: ServerId,
) -> Result<RetiredSession, CodecFailure> {
    let session = read_session(reader, server)?;
    let first_reason = read_retirement_reason(reader.tag()?)?;
    let cleanup = match reader.tag()? {
        0 => RetirementCleanupState::AwaitingEvidence,
        1 => RetirementCleanupState::Confirmed(match reader.tag()? {
            0 => ConfirmedCleanupClass::ReapedAndReleased,
            1 => ConfirmedCleanupClass::AlreadyAbsentWithConfirmedOwnership,
            _ => return Err(CodecFailure::InvalidEncoding),
        }),
        2 => RetirementCleanupState::Failed(read_failure(reader.tag()?)?),
        _ => return Err(CodecFailure::InvalidEncoding),
    };
    RetiredSession::new(session, first_reason, cleanup).map_err(|_| CodecFailure::InvalidEncoding)
}

/// Encodes the native request's exact identity, with no fabricated pairing stream.
pub fn encode_native_retirement(
    value: &NativeRetirementDescriptor,
) -> Result<Vec<u8>, CodecFailure> {
    let value = value.parts();
    encode_record(RecordKind::NativeRetirement, |writer| {
        writer.fixed(&value.request.server().bytes())?;
        writer.fixed(&value.request.id_bytes())?;
        writer.fixed(&value.request.digest().bytes())?;
        writer.u64(value.batch.epoch().get())?;
        writer.u64(value.batch.sequence())?;
        writer.tag(match value.action {
            NativeRetirementAction::Stop => 0,
            NativeRetirementAction::RestartApproved => 1,
            NativeRetirementAction::ActivateUpdate => 2,
        })?;
        writer.u64(value.acceptance_revision.get())?;
        writer.count(value.panes.len(), MAX_RETIREMENT_PANES)?;
        for pane in &value.panes {
            write_retirement_pane(writer, *pane)?;
        }
        writer.count(value.sessions.len(), MAX_RETIREMENT_SESSIONS)?;
        for session in &value.sessions {
            write_retired_session(writer, *session)?;
        }
        Ok(())
    })
}
/// Checks comparison-only cleanup records. A restart is not new cleanup evidence.
pub fn decode_native_retirement(
    bytes: &[u8],
    expected: ServerId,
) -> Result<NativeRetirementDescriptor, CodecFailure> {
    decode_record(RecordKind::NativeRetirement, bytes, |reader| {
        let server = server(reader, expected)?;
        let request = NativeManagementRequestRef::new(
            server,
            array(reader.fixed(16)?)?,
            RequestDigest::from_bytes(array(reader.fixed(32)?)?),
        )
        .map_err(|_| CodecFailure::InvalidEncoding)?;
        let batch = RetirementBatchId::new(server, ServerEpoch::new(reader.u64()?), reader.u64()?)
            .map_err(|_| CodecFailure::InvalidEncoding)?;
        let action = match reader.tag()? {
            0 => NativeRetirementAction::Stop,
            1 => NativeRetirementAction::RestartApproved,
            2 => NativeRetirementAction::ActivateUpdate,
            _ => return Err(CodecFailure::InvalidEncoding),
        };
        let acceptance_revision = OrganizationRevision::new(reader.u64()?);
        let panes = vector(reader, MAX_RETIREMENT_PANES, 33, |reader| {
            read_retirement_pane(reader, server)
        })?;
        let sessions = vector(reader, MAX_RETIREMENT_SESSIONS, 26, |reader| {
            read_retired_session(reader, server)
        })?;
        NativeRetirementDescriptor::new(NativeRetirementDescriptorParts {
            request,
            batch,
            action,
            acceptance_revision,
            panes,
            sessions,
        })
        .map_err(|_| CodecFailure::InvalidEncoding)
    })
}
