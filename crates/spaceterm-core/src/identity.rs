//! Durable identities. Values identify records; they never authenticate a caller.

use std::marker::PhantomData;
use std::num::NonZeroU64;

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Identity conversion or allocation failed without allocating an identity.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum IdentityError {
    #[error("identity must be nonzero")]
    Zero,
    #[error("identity space exhausted")]
    Exhausted,
    #[error("identity belongs to another server")]
    WrongServer,
}

macro_rules! global_identity {
    ($name:ident, $doc:literal) => {
        #[doc = $doc]
        #[derive(
            Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize,
        )]
        pub struct $name([u8; 16]);

        impl $name {
            pub const fn new(bytes: [u8; 16]) -> Self {
                Self(bytes)
            }

            pub const fn bytes(self) -> [u8; 16] {
                self.0
            }
        }
    };
}

global_identity!(
    ServerId,
    "Immutable 128-bit server identity, retained with its signing identity."
);
global_identity!(
    DeviceId,
    "Stable device identity. Multiple Clients on a device have distinct Client IDs."
);

mod sealed {
    pub trait Sealed {}
}

/// A server-issued nonzero identifier. Implementations are sealed by this crate.
pub trait LocalIdentity: sealed::Sealed + Copy {
    fn from_local(server: ServerId, local: NonZeroU64) -> Self;
}

macro_rules! local_identity {
    ($name:ident, $doc:literal) => {
        #[doc = $doc]
        #[derive(
            Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize,
        )]
        pub struct $name {
            server: ServerId,
            local: NonZeroU64,
        }

        impl $name {
            pub fn new(server: ServerId, local: u64) -> Result<Self, IdentityError> {
                Ok(Self {
                    server,
                    local: NonZeroU64::new(local).ok_or(IdentityError::Zero)?,
                })
            }

            pub const fn server(self) -> ServerId {
                self.server
            }

            pub const fn local(self) -> u64 {
                self.local.get()
            }
        }

        impl sealed::Sealed for $name {}
        impl LocalIdentity for $name {
            fn from_local(server: ServerId, local: NonZeroU64) -> Self {
                Self { server, local }
            }
        }
    };
}

local_identity!(
    WorkspaceId,
    "Server-qualified Workspace identity, never reused after deletion."
);
local_identity!(
    TabId,
    "Server-qualified Tab identity, distinct from every other entity kind."
);
local_identity!(
    PaneId,
    "Server-qualified Pane identity, distinct from every other entity kind."
);
local_identity!(
    WorktreeId,
    "Server-qualified retained Worktree handle; Git owns the Worktree itself."
);
local_identity!(
    RepositoryId,
    "Server-qualified repository scope for Git reads and writes."
);
local_identity!(
    PairingId,
    "One pairing incarnation. Re-enrollment receives a new identity."
);
local_identity!(
    ClientId,
    "Stable Client control identity, not a Device ID or a connection identity."
);
local_identity!(
    OperationStreamKey,
    "Server-issued stream handle. Use OperationStreamId to bind its principal."
);

/// A stream is permanently bound to a pairing and stable Client, not to a runtime.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(try_from = "StreamParts", into = "StreamParts")]
pub struct OperationStreamId {
    key: OperationStreamKey,
    pairing: PairingId,
    client: ClientId,
}

#[derive(Clone, Copy, Deserialize, Serialize)]
struct StreamParts {
    key: OperationStreamKey,
    pairing: PairingId,
    client: ClientId,
}

impl OperationStreamId {
    pub fn new(
        key: OperationStreamKey,
        pairing: PairingId,
        client: ClientId,
    ) -> Result<Self, IdentityError> {
        if key.server() != pairing.server() || key.server() != client.server() {
            return Err(IdentityError::WrongServer);
        }
        Ok(Self {
            key,
            pairing,
            client,
        })
    }

    pub const fn key(self) -> OperationStreamKey {
        self.key
    }
    pub const fn pairing(self) -> PairingId {
        self.pairing
    }
    pub const fn client(self) -> ClientId {
        self.client
    }
    pub const fn server(self) -> ServerId {
        self.key.server()
    }
}

impl TryFrom<StreamParts> for OperationStreamId {
    type Error = IdentityError;
    fn try_from(parts: StreamParts) -> Result<Self, Self::Error> {
        Self::new(parts.key, parts.pairing, parts.client)
    }
}

impl From<OperationStreamId> for StreamParts {
    fn from(id: OperationStreamId) -> Self {
        Self {
            key: id.key,
            pairing: id.pairing,
            client: id.client,
        }
    }
}

/// Checked allocation from a persisted high-water mark, including deleted IDs.
///
/// Allocate on a transaction-local copy. Persist the new high-water mark in the
/// same transaction as the entity before publishing the ID. Never rebuild the
/// high-water mark from surviving records. A failed allocation changes nothing.
#[derive(Clone, Debug)]
pub struct IdAllocator<T: LocalIdentity> {
    server: ServerId,
    high_water: u64,
    kind: PhantomData<T>,
}

impl<T: LocalIdentity> IdAllocator<T> {
    pub const fn from_high_water(server: ServerId, high_water: u64) -> Self {
        Self {
            server,
            high_water,
            kind: PhantomData,
        }
    }

    pub const fn high_water(&self) -> u64 {
        self.high_water
    }

    pub fn allocate(&mut self) -> Result<T, IdentityError> {
        let next = self
            .high_water
            .checked_add(1)
            .ok_or(IdentityError::Exhausted)?;
        let local = NonZeroU64::new(next).ok_or(IdentityError::Exhausted)?;
        self.high_water = next;
        Ok(T::from_local(self.server, local))
    }
}

/// Persisted server-wide marks, separately typed by entity kind.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct IdentityHighWater {
    pub workspace: u64,
    pub tab: u64,
    pub pane: u64,
    pub worktree: u64,
    pub repository: u64,
    pub pairing: u64,
    pub client: u64,
    pub operation_stream: u64,
    pub creation_ordinal: u64,
}

impl IdentityHighWater {
    /// Every mark must be monotonic, including across transactions with deletion.
    pub fn dominates(self, previous: Self) -> bool {
        self.workspace >= previous.workspace
            && self.tab >= previous.tab
            && self.pane >= previous.pane
            && self.worktree >= previous.worktree
            && self.repository >= previous.repository
            && self.pairing >= previous.pairing
            && self.client >= previous.client
            && self.operation_stream >= previous.operation_stream
            && self.creation_ordinal >= previous.creation_ordinal
    }
}
