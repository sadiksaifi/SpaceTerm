//! Shared Git confirmation and compact-outcome contracts.

use crate::authority::{OrganizationRevision, ServerEpoch};
use crate::identity::{ClientId, OperationStreamId, PairingId, ServerId, WorktreeId};
use serde::{Deserialize, Serialize};
use std::fmt;

/// Maximum exact removal scope. Smaller organization limits still apply.
pub const MAX_GIT_TABS: usize = 4096;
/// Maximum exact Pane scope; authorization scopes are never truncated.
pub const MAX_GIT_PANES: usize = 16_384;
/// Maximum unresolved Git operations admitted by one server.
pub const MAX_GIT_INTENTS: usize = 32;
/// Maximum retained repository/inventory records.
pub const MAX_GIT_REPOSITORIES: usize = 4096;
/// Aggregate retained scope ceiling, independent of wire paging.
pub const MAX_GIT_SCOPE_BYTES: usize = 16 * 1024 * 1024;

/// Payload-free failures. Native errors and user values must not be attached.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum GitFailure {
    Unauthorized,
    WrongServer,
    TargetGone,
    Offline,
    Busy,
    Capacity,
    ExpiredAssessment,
    StaleAssessment,
    MainWorktree,
    LockedWorktree,
    UnlistedWorktree,
    HoldsPinnedDirectory,
    NoFallback,
    InvalidBranch,
    InvalidTemplate,
    InvalidDirectory,
    OccupiedTarget,
    BranchExists,
    BranchMissing,
    BranchCheckedOut,
    IdentityReplaced,
    GitStateChanged,
    Unchecked,
    ToolMissing,
    ToolTooOld,
    CancelledRead,
    TimedOutRead,
    InvalidResponse,
    OutputTooLarge,
    WriteFailed,
    ExternalStateUncertain,
    RecoveryNeedsReview,
    StorageLocked,
    StorageUnavailable,
    UnsafeStorage,
    StorageFailure,
    IndeterminateDurability,
}
impl fmt::Display for GitFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Git operation failed ({self:?})")
    }
}
impl std::error::Error for GitFailure {}

/// A creation assessment cannot authorize removal or conversely.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum GitAssessmentKind {
    Create,
    Remove,
}

/// Untrusted comparison input. The checked handle is produced by `new`.
#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
pub struct GitAssessmentTokenParts {
    pub server: ServerId,
    pub epoch: ServerEpoch,
    pub pairing: PairingId,
    pub client: ClientId,
    pub kind: GitAssessmentKind,
    pub random: [u8; 32],
}

/// Opaque, server-issued reference to an exact cached assessment.
///
/// Deserialization establishes structural consistency only. The server checks
/// authenticated principal, cached scope, expiry and single consumption. The
/// token does not contain native identity or independently authorize an effect.
#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(try_from = "GitAssessmentTokenParts", into = "GitAssessmentTokenParts")]
pub struct GitAssessmentToken(GitAssessmentTokenParts);
impl GitAssessmentToken {
    pub fn new(parts: GitAssessmentTokenParts) -> Result<Self, GitFailure> {
        if parts.server != parts.pairing.server() || parts.server != parts.client.server() {
            return Err(GitFailure::WrongServer);
        }
        if parts.epoch.get() == 0 || parts.random == [0; 32] {
            return Err(GitFailure::InvalidResponse);
        }
        Ok(Self(parts))
    }
    pub fn parts(&self) -> &GitAssessmentTokenParts {
        &self.0
    }
    pub const fn server(&self) -> ServerId {
        self.0.server
    }
    pub const fn epoch(&self) -> ServerEpoch {
        self.0.epoch
    }
    pub const fn kind(&self) -> GitAssessmentKind {
        self.0.kind
    }
    pub fn validate_stream(&self, stream: OperationStreamId) -> Result<(), GitFailure> {
        if self.0.server != stream.server() {
            return Err(GitFailure::WrongServer);
        }
        if self.0.pairing != stream.pairing() || self.0.client != stream.client() {
            return Err(GitFailure::Unauthorized);
        }
        Ok(())
    }
}
impl fmt::Debug for GitAssessmentToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("GitAssessmentToken")
            .field("kind", &self.0.kind)
            .finish_non_exhaustive()
    }
}
impl TryFrom<GitAssessmentTokenParts> for GitAssessmentToken {
    type Error = GitFailure;
    fn try_from(parts: GitAssessmentTokenParts) -> Result<Self, Self::Error> {
        Self::new(parts)
    }
}
impl From<GitAssessmentToken> for GitAssessmentTokenParts {
    fn from(value: GitAssessmentToken) -> Self {
        value.0
    }
}

/// Identified writes carry only a server-issued exact confirmation.
/// No client-supplied force boolean or target path is accepted here.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum GitCommand {
    CreateConfirmed { assessment: GitAssessmentToken },
    RemoveConfirmed { assessment: GitAssessmentToken },
}
impl GitCommand {
    pub fn assessment(&self) -> &GitAssessmentToken {
        match self {
            Self::CreateConfirmed { assessment } | Self::RemoveConfirmed { assessment } => {
                assessment
            }
        }
    }
    pub fn server(&self) -> ServerId {
        self.assessment().server()
    }
    pub fn validate_stream(&self, stream: OperationStreamId) -> Result<(), GitFailure> {
        self.assessment().validate_stream(stream)?;
        let expected = match self {
            Self::CreateConfirmed { .. } => GitAssessmentKind::Create,
            Self::RemoveConfirmed { .. } => GitAssessmentKind::Remove,
        };
        if self.assessment().kind() != expected {
            return Err(GitFailure::StaleAssessment);
        }
        Ok(())
    }
}

/// Git/filesystem disposition, independent of SQLite commit disposition.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum ExternalDisposition {
    NoEffectEstablished,
    DesiredStateObserved,
    PartialOrAmbiguous,
    Unavailable,
}
/// Recovery observations do not establish historical successful execution.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum RecoveryObservation {
    DesiredStateObserved,
    PreStateObserved,
    PartialOrAmbiguous,
    Unavailable,
}

/// Untrusted compact-result input. No affected hierarchy is embedded in an outcome.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum GitMutationOutcomeParts {
    Created {
        worktree: WorktreeId,
        revision: OrganizationRevision,
    },
    Removed {
        worktree: WorktreeId,
        closed_tabs: u32,
        ended_sessions: u32,
        revision: OrganizationRevision,
    },
    Failed {
        server: ServerId,
        failure: GitFailure,
        ended_sessions: u32,
        external: ExternalDisposition,
    },
    Recovered {
        server: ServerId,
        observation: RecoveryObservation,
        revision: Option<OrganizationRevision>,
    },
}
/// Checked compact result. Success retains its operation-owned final revision.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(try_from = "GitMutationOutcomeParts", into = "GitMutationOutcomeParts")]
pub struct GitMutationOutcome(GitMutationOutcomeParts);
impl GitMutationOutcome {
    pub fn new(parts: GitMutationOutcomeParts) -> Result<Self, GitFailure> {
        match &parts {
            GitMutationOutcomeParts::Removed {
                closed_tabs,
                ended_sessions,
                ..
            } if *closed_tabs as usize > MAX_GIT_TABS
                || *ended_sessions as usize > MAX_GIT_PANES =>
            {
                return Err(GitFailure::Capacity);
            }
            GitMutationOutcomeParts::Failed { ended_sessions, .. }
                if *ended_sessions as usize > MAX_GIT_PANES =>
            {
                return Err(GitFailure::Capacity);
            }
            _ => {}
        }
        Ok(Self(parts))
    }
    pub fn parts(&self) -> &GitMutationOutcomeParts {
        &self.0
    }
    pub fn server(&self) -> ServerId {
        match &self.0 {
            GitMutationOutcomeParts::Created { worktree, .. }
            | GitMutationOutcomeParts::Removed { worktree, .. } => worktree.server(),
            GitMutationOutcomeParts::Failed { server, .. }
            | GitMutationOutcomeParts::Recovered { server, .. } => *server,
        }
    }
    pub fn worktree(&self) -> Option<WorktreeId> {
        match &self.0 {
            GitMutationOutcomeParts::Created { worktree, .. }
            | GitMutationOutcomeParts::Removed { worktree, .. } => Some(*worktree),
            _ => None,
        }
    }
    pub fn revision(&self) -> Option<OrganizationRevision> {
        match &self.0 {
            GitMutationOutcomeParts::Created { revision, .. }
            | GitMutationOutcomeParts::Removed { revision, .. } => Some(*revision),
            GitMutationOutcomeParts::Recovered { revision, .. } => *revision,
            GitMutationOutcomeParts::Failed { .. } => None,
        }
    }
    pub fn validate_scope(&self, server: ServerId) -> Result<(), GitFailure> {
        if self.server() != server {
            Err(GitFailure::WrongServer)
        } else {
            Ok(())
        }
    }
}
impl TryFrom<GitMutationOutcomeParts> for GitMutationOutcome {
    type Error = GitFailure;
    fn try_from(parts: GitMutationOutcomeParts) -> Result<Self, Self::Error> {
        Self::new(parts)
    }
}
impl From<GitMutationOutcome> for GitMutationOutcomeParts {
    fn from(value: GitMutationOutcome) -> Self {
        value.0
    }
}
