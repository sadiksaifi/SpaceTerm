//! Checked client-local view counters, representation bounds and failure classes.
//! None of these values is a wire identity, a grant or server authority.

use std::num::NonZeroU64;

use spaceterm_core::identity::WorkspaceId;
use thiserror::Error;

/// Attachments retained by one client view.
pub const MAX_VIEW_ATTACHMENTS: usize = 256;
/// Tabs represented across every attached Workspace.
pub const MAX_VIEW_TABS: usize = 8_192;
/// Panes represented across every attached Workspace.
pub const MAX_VIEW_PANES: usize = 65_536;
/// Panes represented in one Tab's personal arrangement.
pub const MAX_TAB_PANES: usize = 4_096;
/// Encoded retained view document.
pub const MAX_VIEW_DOCUMENT_BYTES: usize = 16 * 1024 * 1024;

/// A Workspace is attached by its exact server-qualified identity.
pub type WorkspaceKey = WorkspaceId;

macro_rules! revision {
    ($name:ident, $doc:literal) => {
        #[doc = $doc]
        #[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
        pub struct $name(u64);

        impl $name {
            /// The value before any local change.
            pub const INITIAL: Self = Self(0);

            pub const fn new(value: u64) -> Self {
                Self(value)
            }
            pub const fn get(self) -> u64 {
                self.0
            }
            /// Exhaustion must reject the change before any effect.
            pub fn checked_successor(self) -> Option<Self> {
                self.0.checked_add(1).map(Self)
            }
        }
    };
}

macro_rules! local_id {
    ($name:ident, $doc:literal) => {
        #[doc = $doc]
        #[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
        pub struct $name(NonZeroU64);

        impl $name {
            /// The first value a fresh allocator issues.
            pub const FIRST: Self = Self(NonZeroU64::MIN);

            /// Zero is never a valid local identity.
            pub const fn new(value: u64) -> Option<Self> {
                match NonZeroU64::new(value) {
                    Some(value) => Some(Self(value)),
                    None => None,
                }
            }
            pub const fn get(self) -> u64 {
                self.0.get()
            }
            /// Exhaustion must reject the allocation before any effect.
            pub fn checked_successor(self) -> Option<Self> {
                self.0.checked_add(1).map(Self)
            }
        }
    };
}

revision!(
    ViewRevision,
    "Local revision of the whole personal view. A detach plan prepared at a newer one is stale."
);
revision!(
    LayoutRevision,
    "Local geometry revision of one Pane arrangement. Measurements bind it."
);
local_id!(
    SplitId,
    "Personal Split identity, never reused within its arrangement."
);
local_id!(
    AttachmentIncarnation,
    "One local attachment of a Workspace. Reattaching allocates a new value."
);
local_id!(
    UseTicket,
    "Local explicit-use callback correlation, never a wire or startup identity."
);

/// A client representation limit. Server membership past it is never truncated.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ViewResource {
    Attachments,
    Tabs,
    Panes,
    TabPanes,
    DocumentBytes,
}

/// A checked local counter.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ViewCounter {
    ViewRevision,
    LayoutRevision,
    AttachmentIncarnation,
    SplitId,
    UseTicket,
}

impl ViewCounter {
    pub(crate) const fn exhausted(self) -> ViewError {
        ViewError::IdentityExhausted { counter: self }
    }
}

/// Typed view failure. Values carry classes, resources, counters or schema
/// versions only: never paths, names, terminal or clipboard contents, credentials,
/// raw native errors or the rejected document.
#[derive(Clone, Copy, Debug, Eq, Error, Hash, PartialEq)]
pub enum ViewError {
    #[error("the view target is missing")]
    TargetMissing,
    #[error("the view target belongs to another server")]
    WrongServer,
    #[error("the view target belongs to another membership scope")]
    WrongMembership,
    #[error("the view revision is stale")]
    StaleView,
    #[error("the replica observation is stale")]
    StaleObservation,
    #[error("the explicit-use intent is stale")]
    StaleIntent,
    #[error("the replica is not ready")]
    NotReady,
    #[error("the arrangement geometry is invalid")]
    InvalidGeometry,
    #[error("the split ratio is invalid")]
    InvalidRatio,
    #[error("the available space cannot fit the arrangement minimum")]
    InsufficientSpace,
    #[error("the creation topology is invalid")]
    InvalidCreationTopology,
    #[error("the retained view state is invalid")]
    InvalidPersistedState,
    #[error("retained view format version {version} is unsupported")]
    UnsupportedFormat { version: u32 },
    #[error("the view resource limit for {resource:?} was reached")]
    ResourceLimit { resource: ViewResource },
    #[error("the local {counter:?} counter is exhausted")]
    IdentityExhausted { counter: ViewCounter },
    #[error("view storage is unavailable")]
    StorageUnavailable,
    #[error("view storage is unsafe")]
    UnsafeStorage,
    #[error("view publication conflicts with another writer")]
    PublicationConflict,
    #[error("the view publication outcome is indeterminate")]
    IndeterminatePublication,
}
