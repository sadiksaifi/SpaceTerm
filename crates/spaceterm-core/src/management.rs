//! Authenticated, bounded status comparisons. Native observations are explicitly
//! unverified until the native provider authenticates them. Status grants neither
//! pairing access nor permission to Start, Stop, update, enroll, or bind listeners.

use std::collections::BTreeSet;
use std::fmt;
use std::net::SocketAddr;
use std::num::NonZeroU64;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::authority::{ConnectionBinding, ServerEpoch};
use crate::identity::ServerId;
use crate::organization::{BoundedText, Versioned};
use crate::remote_access::RemoteAccessPolicy;

pub const MAX_SERVER_RUNTIME_OWNERS: usize = 64;
pub const MAX_STATUS_CAPABILITIES: usize = 16;
pub const MAX_STATUS_PROGRESS: usize = 8;
pub const MAX_STATUS_LISTENERS: usize = 8;
/// Native account-service supervision and lifecycle management availability.
pub const CAPABILITY_NATIVE_LIFECYCLE: u32 = 1;
/// Authenticated, independently installed server code selection availability.
pub const CAPABILITY_SIGNED_SERVER_INSTALLATION: u32 = 2;
pub const MAX_PUBLICATION_PARTS: u32 = 4096;
pub type ServerVersion = BoundedText<128>;

#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum StatusError {
    #[error("status comparison binding is invalid")]
    InvalidBinding,
    #[error("status record exceeds its bound")]
    Limit,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum NativeVerification {
    Unverified,
    Verified,
}
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum CapabilityAvailability {
    Available,
    AuthorizationRequired,
    Unavailable,
    Unsupported,
}
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CapabilityStatus {
    pub capability: u32,
    pub availability: CapabilityAvailability,
}
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum ListenerState {
    Binding,
    Listening,
    Closing,
    Unavailable,
}
/// A listener fact is separate from durable policy. The endpoint contains no
/// client-local filesystem authority. IPv6 remains explicitly IPv6-only.
#[derive(Clone, Copy, Deserialize, Eq, PartialEq, Serialize)]
pub struct ListenerStatus {
    pub generation: NonZeroU64,
    pub endpoint: SocketAddr,
    pub state: ListenerState,
}
impl fmt::Debug for ListenerStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ListenerStatus")
            .field("generation", &self.generation)
            .field("state", &self.state)
            .finish_non_exhaustive()
    }
}
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum ManagementProgressState {
    Pending,
    Draining,
    WaitingForCleanup,
    Completed,
    Unavailable,
    RecoveryRequired,
}
/// Original provider-issued comparison identity. Native request bytes are never
/// converted to a pairing-bound operation or used as an authorization capability.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum ManagementRequestReference {
    Operation(crate::operations::OperationBinding),
    /// Shared progress identifies work without possessing its execution digest.
    OperationReference(crate::operations::OperationId),
    Native([u8; 16]),
}
impl ManagementRequestReference {
    /// Comparison identity only. This never supplies an execution digest.
    pub const fn operation_id(self) -> Option<crate::operations::OperationId> {
        match self {
            Self::Operation(binding) => Some(binding.id),
            Self::OperationReference(id) => Some(id),
            Self::Native(_) => None,
        }
    }
}
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ManagementProgress {
    pub request: ManagementRequestReference,
    pub state: ManagementProgressState,
    pub completed_units: u32,
    pub total_units: u32,
}
/// The actor's storage observation. Unknown preserves older status records and
/// never implies that initialization, replacement or recovery is permitted.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub enum StorageStatus {
    #[default]
    Unknown,
    Ready,
    Locked,
    Unavailable,
    RecoveryRequired,
}
/// Durable native intent, separate from instance liveness. A signal or crash
/// does not imply deliberate Stop, and status never authorizes Start.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub enum ServerRunIntent {
    #[default]
    Unknown,
    Running,
    Stopped,
}
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct ServerRuntimeStatus {
    pub storage: StorageStatus,
    pub intent: ServerRunIntent,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ServerStatusParts {
    #[serde(default)]
    pub runtime: ServerRuntimeStatus,
    pub server: ServerId,
    pub epoch: ServerEpoch,
    pub revision: NonZeroU64,
    pub running: Option<ServerVersion>,
    pub approved: Option<ServerVersion>,
    pub staged: Option<ServerVersion>,
    pub native_verification: NativeVerification,
    pub capabilities: Vec<CapabilityStatus>,
    pub remote_access: Versioned<RemoteAccessPolicy>,
    pub listeners: Vec<ListenerStatus>,
    pub progress: Vec<ManagementProgress>,
}
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(try_from = "ServerStatusParts", into = "ServerStatusParts")]
pub struct ServerStatus(ServerStatusParts);
impl ServerStatus {
    pub fn new(parts: ServerStatusParts) -> Result<Self, StatusError> {
        if parts.epoch.get() == 0 || parts.remote_access.value.server() != parts.server {
            return Err(StatusError::InvalidBinding);
        }
        if parts.capabilities.len() > MAX_STATUS_CAPABILITIES
            || parts.listeners.len() > MAX_STATUS_LISTENERS
            || parts.progress.len() > MAX_STATUS_PROGRESS
        {
            return Err(StatusError::Limit);
        }
        let mut capabilities = BTreeSet::new();
        if parts
            .capabilities
            .iter()
            .any(|value| value.capability == 0 || !capabilities.insert(value.capability))
        {
            return Err(StatusError::InvalidBinding);
        }
        let mut listeners = BTreeSet::new();
        if parts
            .listeners
            .iter()
            .any(|value| !listeners.insert(value.endpoint))
        {
            return Err(StatusError::InvalidBinding);
        }
        for value in &parts.progress {
            if value.completed_units > value.total_units
                || match value.request {
                    ManagementRequestReference::Operation(binding) => {
                        binding.id.stream().server() != parts.server
                    }
                    ManagementRequestReference::OperationReference(id) => {
                        id.stream().server() != parts.server
                    }
                    ManagementRequestReference::Native(bytes) => bytes == [0; 16],
                }
            {
                return Err(StatusError::InvalidBinding);
            }
        }
        Ok(Self(parts))
    }
    pub fn parts(&self) -> &ServerStatusParts {
        &self.0
    }
}
impl TryFrom<ServerStatusParts> for ServerStatus {
    type Error = StatusError;
    fn try_from(parts: ServerStatusParts) -> Result<Self, Self::Error> {
        Self::new(parts)
    }
}
impl From<ServerStatus> for ServerStatusParts {
    fn from(value: ServerStatus) -> Self {
        value.0
    }
}

/// Read correlation is connection-scoped, never a durable OperationId.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StatusQuery {
    pub connection: ConnectionBinding,
    pub request: NonZeroU64,
    pub generation: u64,
    pub known_epoch: Option<ServerEpoch>,
    pub known_revision: Option<NonZeroU64>,
    pub subscribe: bool,
}
/// Epoch/revision gaps require a complete replacement status. A subscription
/// snapshot cannot establish ordinary effect reconciliation on its own.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum StatusPublication {
    Snapshot {
        request: NonZeroU64,
        generation: u64,
        status: ServerStatus,
    },
    Changed {
        request: NonZeroU64,
        generation: u64,
        base: NonZeroU64,
        status: ServerStatus,
    },
    ResyncRequired {
        request: NonZeroU64,
        generation: u64,
    },
}
