//! Comparison stamps and exact effect bindings, never authentication proofs.

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::identity::{ClientId, PaneId};

macro_rules! stamp {
    ($name:ident, $doc:literal) => {
        #[doc = $doc]
        #[derive(
            Clone,
            Copy,
            Debug,
            Default,
            Deserialize,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd,
            Serialize,
        )]
        pub struct $name(u64);
        impl $name {
            pub const fn new(value: u64) -> Self {
                Self(value)
            }
            pub const fn get(self) -> u64 {
                self.0
            }
            /// Exhaustion must reject the operation before any effect.
            pub fn checked_successor(self) -> Option<Self> {
                self.0.checked_add(1).map(Self)
            }
        }
    };
}

stamp!(
    ServerEpoch,
    "Runtime incarnation, advanced durably before each server startup."
);
stamp!(
    SessionGeneration,
    "Pane runtime incarnation, independent of organization identity."
);
stamp!(
    ControlGeneration,
    "Grant incarnation, advanced for each acquire, release, or takeover."
);
stamp!(
    ConnectionGeneration,
    "Authenticated connection incarnation for one stable Client."
);
stamp!(
    ContextGeneration,
    "Native or interaction context incarnation for delayed effects."
);
stamp!(
    ContentRevision,
    "Revision of authoritative terminal content within a Session."
);
stamp!(
    OrganizationRevision,
    "Publication revision for shared organization changes."
);
stamp!(
    StoreRevision,
    "Durable transaction revision, including ledger-only changes."
);
stamp!(
    FieldVersion,
    "Optimistic version of one shared field, not the whole organization."
);
stamp!(
    CreationOrdinal,
    "Retained server-wide creation order, not client display order."
);

/// A Session identity qualified by its server runtime and Pane incarnation.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
pub struct SessionKey {
    pub epoch: ServerEpoch,
    pub pane: PaneId,
    pub generation: SessionGeneration,
}

/// Comparison data describing an exact Pane control grant.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(try_from = "GrantParts", into = "GrantParts")]
pub struct ControlGrant {
    pane: PaneId,
    client: ClientId,
    generation: ControlGeneration,
}

#[derive(Clone, Copy, Deserialize, Serialize)]
struct GrantParts {
    pane: PaneId,
    client: ClientId,
    generation: ControlGeneration,
}

/// A binding is internally inconsistent. No user-controlled values are reported.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum FenceError {
    #[error("control grant belongs to another server")]
    WrongServer,
    #[error("effect fence has a different Pane or Client")]
    WrongBinding,
}

impl ControlGrant {
    pub fn new(
        pane: PaneId,
        client: ClientId,
        generation: ControlGeneration,
    ) -> Result<Self, FenceError> {
        if pane.server() != client.server() {
            return Err(FenceError::WrongServer);
        }
        Ok(Self {
            pane,
            client,
            generation,
        })
    }
    pub const fn pane(self) -> PaneId {
        self.pane
    }
    pub const fn client(self) -> ClientId {
        self.client
    }
    pub const fn generation(self) -> ControlGeneration {
        self.generation
    }
}

impl TryFrom<GrantParts> for ControlGrant {
    type Error = FenceError;
    fn try_from(parts: GrantParts) -> Result<Self, Self::Error> {
        Self::new(parts.pane, parts.client, parts.generation)
    }
}
impl From<ControlGrant> for GrantParts {
    fn from(grant: ControlGrant) -> Self {
        Self {
            pane: grant.pane,
            client: grant.client,
            generation: grant.generation,
        }
    }
}

/// Exact authenticated connection to which an effect was originally admitted.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ConnectionBinding {
    pub client: ClientId,
    pub generation: ConnectionGeneration,
}
impl ConnectionBinding {
    pub const fn new(client: ClientId, generation: ConnectionGeneration) -> Self {
        Self { client, generation }
    }
}

/// Fence captured at admission and rechecked against current authenticated state
/// immediately before an effect. Serialization grants no authority.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(try_from = "FenceParts", into = "FenceParts")]
pub struct EffectFence {
    session: SessionKey,
    grant: ControlGrant,
    connection: ConnectionBinding,
    context: ContextGeneration,
    content: Option<ContentRevision>,
}

#[derive(Clone, Copy, Deserialize, Serialize)]
struct FenceParts {
    session: SessionKey,
    grant: ControlGrant,
    connection: ConnectionBinding,
    context: ContextGeneration,
    content: Option<ContentRevision>,
}

impl EffectFence {
    pub fn new(
        session: SessionKey,
        grant: ControlGrant,
        connection: ConnectionBinding,
        context: ContextGeneration,
        content: Option<ContentRevision>,
    ) -> Result<Self, FenceError> {
        if session.pane != grant.pane || connection.client != grant.client {
            return Err(FenceError::WrongBinding);
        }
        Ok(Self {
            session,
            grant,
            connection,
            context,
            content,
        })
    }
    pub const fn session(self) -> SessionKey {
        self.session
    }
    pub const fn grant(self) -> ControlGrant {
        self.grant
    }
    pub const fn connection(self) -> ConnectionBinding {
        self.connection
    }
    pub const fn context(self) -> ContextGeneration {
        self.context
    }
    pub const fn content(self) -> Option<ContentRevision> {
        self.content
    }

    /// Exact comparison, including whether content dependence was captured.
    pub fn matches(&self, current: &Self) -> bool {
        self == current
    }

    /// Compare to current state. Content-independent input does not become stale
    /// merely because output advanced. Content-dependent requests require a revision.
    pub fn is_current(&self, current: &Self) -> bool {
        self.session == current.session
            && self.grant == current.grant
            && self.connection == current.connection
            && self.context == current.context
            && self
                .content
                .is_none_or(|revision| current.content == Some(revision))
    }
}
impl TryFrom<FenceParts> for EffectFence {
    type Error = FenceError;
    fn try_from(parts: FenceParts) -> Result<Self, Self::Error> {
        Self::new(
            parts.session,
            parts.grant,
            parts.connection,
            parts.context,
            parts.content,
        )
    }
}
impl From<EffectFence> for FenceParts {
    fn from(fence: EffectFence) -> Self {
        Self {
            session: fence.session,
            grant: fence.grant,
            connection: fence.connection,
            context: fence.context,
            content: fence.content,
        }
    }
}

stamp!(
    CloseWorkGeneration,
    "Content-free work comparison generation within an exact Session. Zero is initial Unknown."
);

/// The exact epoch and grant returned by a creation, acquisition, or takeover.
/// This is historical comparison data, not current control authority. A client
/// must retain this epoch when it records a deferred exact-generation release.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(try_from = "GrantedControlParts", into = "GrantedControlParts")]
pub struct GrantedControl {
    epoch: ServerEpoch,
    grant: ControlGrant,
}

#[derive(Clone, Copy, Deserialize, Serialize)]
struct GrantedControlParts {
    epoch: ServerEpoch,
    grant: ControlGrant,
}

impl GrantedControl {
    /// Actual grants require an established epoch and a nonzero grant generation.
    /// The existing ControlGrant constructor continues to permit comparison zero.
    pub fn new(epoch: ServerEpoch, grant: ControlGrant) -> Result<Self, FenceError> {
        if epoch.get() == 0 || grant.generation().get() == 0 {
            return Err(FenceError::WrongBinding);
        }
        Ok(Self { epoch, grant })
    }

    pub const fn epoch(self) -> ServerEpoch {
        self.epoch
    }

    pub const fn grant(self) -> ControlGrant {
        self.grant
    }
}

impl TryFrom<GrantedControlParts> for GrantedControl {
    type Error = FenceError;

    fn try_from(parts: GrantedControlParts) -> Result<Self, Self::Error> {
        Self::new(parts.epoch, parts.grant)
    }
}

impl From<GrantedControl> for GrantedControlParts {
    fn from(value: GrantedControl) -> Self {
        Self {
            epoch: value.epoch,
            grant: value.grant,
        }
    }
}
