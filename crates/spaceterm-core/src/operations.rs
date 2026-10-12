//! Durable operation identities and typed requests/outcomes, without a ledger reducer.
//!
//! Retry uses the original stream/sequence and canonical request digest. Epochs
//! and connections fence effects but are never part of the operation identity.

use std::io::{self, Write};
use std::num::NonZeroU64;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::authority::*;
use crate::control::*;
use crate::identity::*;
use crate::organization::*;

pub const MAX_MANAGEMENT_BYTES: usize = 64 * 1024;

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
pub struct OperationSequence(NonZeroU64);
impl OperationSequence {
    pub fn new(value: u64) -> Result<Self, IdentityError> {
        Ok(Self(NonZeroU64::new(value).ok_or(IdentityError::Zero)?))
    }
    pub const fn get(self) -> u64 {
        self.0.get()
    }
    pub fn checked_successor(self) -> Option<Self> {
        self.get()
            .checked_add(1)
            .and_then(NonZeroU64::new)
            .map(Self)
    }
}
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
pub struct OperationId {
    stream: OperationStreamId,
    sequence: OperationSequence,
}
impl OperationId {
    pub const fn new(stream: OperationStreamId, sequence: OperationSequence) -> Self {
        Self { stream, sequence }
    }
    pub const fn stream(self) -> OperationStreamId {
        self.stream
    }
    pub const fn sequence(self) -> OperationSequence {
        self.sequence
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RequestDigest([u8; 32]);
impl RequestDigest {
    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }
    pub const fn bytes(self) -> [u8; 32] {
        self.0
    }

    /// Canonical core command encoding v1: compact serde JSON in declared struct
    /// field order, enum tags as declared, no maps/floats, domain-separated SHA-256.
    /// Wire versions convert to this same domain request before binding. Changing
    /// this encoding requires explicit retained-ledger compatibility handling.
    /// Hashing streams into a bounded writer; it never allocates an encoded copy.
    pub fn for_command(command: &Command) -> Result<Self, RequestError> {
        let mut writer = DigestWriter {
            hasher: Sha256::new(),
            bytes: 0,
            exceeded: false,
        };
        writer.hasher.update(b"spaceterm-command-v1\0");
        if serde_json::to_writer(&mut writer, command).is_err() {
            return Err(if writer.exceeded {
                RequestError::TooLarge
            } else {
                RequestError::InvalidEncoding
            });
        }
        Ok(Self(writer.hasher.finalize().into()))
    }
}
struct DigestWriter {
    hasher: Sha256,
    bytes: usize,
    exceeded: bool,
}
impl Write for DigestWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() > MAX_MANAGEMENT_BYTES.saturating_sub(self.bytes) {
            self.exceeded = true;
            return Err(io::ErrorKind::InvalidData.into());
        }
        self.bytes += bytes.len();
        self.hasher.update(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum RequestError {
    #[error("operation request exceeds management limit")]
    TooLarge,
    #[error("operation request cannot be canonically encoded")]
    InvalidEncoding,
    #[error("operation request has inconsistent server or Client scope")]
    WrongScope,
    #[error("operation identity was reused with different contents")]
    IdentityReused,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum WorkspaceCreationSource {
    ServerHome { server: ServerId },
    Directory(ServerDirectory),
    Pane(PaneId),
}
impl WorkspaceCreationSource {
    fn server(&self) -> ServerId {
        match self {
            Self::ServerHome { server } => *server,
            Self::Directory(v) => v.server(),
            Self::Pane(v) => v.server(),
        }
    }
}
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum TabCreationSource {
    Pane(PaneId),
    Directory(ServerDirectory),
    Worktree(WorktreeId),
}
impl TabCreationSource {
    fn server(&self) -> ServerId {
        match self {
            Self::Pane(v) => v.server(),
            Self::Directory(v) => v.server(),
            Self::Worktree(v) => v.server(),
        }
    }
}
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RepositoryScope {
    pub server: ServerId,
    pub repository: RepositoryId,
}
impl RepositoryScope {
    pub fn validate(self) -> Result<(), RequestError> {
        if self.server != self.repository.server() {
            Err(RequestError::WrongScope)
        } else {
            Ok(())
        }
    }
}
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum CloseTarget {
    Pane(PaneId),
    Tab(TabId),
    Workspace(WorkspaceId),
}
impl CloseTarget {
    pub const fn server(self) -> ServerId {
        match self {
            Self::Pane(v) => v.server(),
            Self::Tab(v) => v.server(),
            Self::Workspace(v) => v.server(),
        }
    }
}
/// An exact assessment reference is comparison data. Server-held assessment state
/// binds affected membership, runtime/work generations and native Git proofs.
/// Revalidation is required immediately before any destructive effect.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct AssessmentRef {
    pub operation: OperationId,
    pub digest: RequestDigest,
}
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum AssessmentTarget {
    Close(CloseTarget),
    EndAllSessions(ServerId),
    RemoveWorktree {
        scope: RepositoryScope,
        worktree: WorktreeId,
    },
}
/// Server-internal comparison facts. Native proofs are retained by the host, not here.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct AssessedPane {
    pub workspace: WorkspaceId,
    pub tab: TabId,
    pub pane: PaneId,
    pub session_generation: SessionGeneration,
    pub control_generation: ControlGeneration,
    pub context_generation: ContextGeneration,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DestructiveAssessment {
    pub reference: AssessmentRef,
    pub epoch: ServerEpoch,
    pub target: AssessmentTarget,
    pub affected: Vec<AssessedPane>,
    pub repository_generation: Option<ContextGeneration>,
}

/// Every creation identifies its source. There is no server-global active target.
/// This enum is separate from the desktop's serialized keybinding Command IDs.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum Command {
    CreateWorkspace {
        server: ServerId,
        source: WorkspaceCreationSource,
        name: NameDraft,
    },
    CreateTab {
        workspace: WorkspaceId,
        source: TabCreationSource,
    },
    CreatePane {
        tab: TabId,
        source: PaneId,
        axis: SplitAxis,
        edge: SplitEdge,
    },
    RenameWorkspace {
        workspace: WorkspaceId,
        expected_version: FieldVersion,
        draft: NameDraft,
    },
    SetPinnedDirectory {
        workspace: WorkspaceId,
        expected_version: FieldVersion,
        value: Option<ServerDirectory>,
    },
    RepairPaneDirectory {
        pane: PaneId,
        expected_version: FieldVersion,
        value: ServerDirectory,
    },
    RetryPaneDirectory {
        pane: PaneId,
        expected_version: FieldVersion,
    },
    SetWorktreeLocation {
        server: ServerId,
        expected_version: FieldVersion,
        template: WorktreeLocationTemplate,
    },
    AssessClose {
        target: CloseTarget,
    },
    CloseConfirmed {
        target: CloseTarget,
        assessment: AssessmentRef,
    },
    KillWorkspaceConfirmed {
        workspace: WorkspaceId,
        assessment: AssessmentRef,
    },
    AssessEndAllSessions {
        server: ServerId,
    },
    EndAllSessions {
        server: ServerId,
        assessment: AssessmentRef,
    },
    CompareAcquire(CompareAcquireRequest),
    TakeOver(TakeOverRequest),
    ReleaseControl(ReleaseControlRequest),
    BeginStartup(StartupIntent),
    CompleteStartup(StartupMeasurement),
    CancelStartup {
        intent: OperationId,
    },
    RevokePairing {
        pairing: PairingId,
    },
    RefreshWorktrees {
        scope: RepositoryScope,
    },
    CreateWorktree {
        scope: RepositoryScope,
        branch: BranchName,
    },
    AssessRemoveWorktree {
        scope: RepositoryScope,
        worktree: WorktreeId,
    },
    RemoveWorktreeConfirmed {
        scope: RepositoryScope,
        worktree: WorktreeId,
        assessment: AssessmentRef,
        force: bool,
    },
}
impl Command {
    pub fn server(&self) -> ServerId {
        match self {
            Self::CreateWorkspace { server, .. }
            | Self::SetWorktreeLocation { server, .. }
            | Self::AssessEndAllSessions { server }
            | Self::EndAllSessions { server, .. } => *server,
            Self::CreateTab { workspace, .. }
            | Self::RenameWorkspace { workspace, .. }
            | Self::SetPinnedDirectory { workspace, .. }
            | Self::KillWorkspaceConfirmed { workspace, .. } => workspace.server(),
            Self::CreatePane { tab, .. } => tab.server(),
            Self::RepairPaneDirectory { pane, .. } | Self::RetryPaneDirectory { pane, .. } => {
                pane.server()
            }
            Self::AssessClose { target } | Self::CloseConfirmed { target, .. } => target.server(),
            Self::CompareAcquire(v) => v.pane.server(),
            Self::TakeOver(v) => v.pane.server(),
            Self::ReleaseControl(v) => v.grant.pane().server(),
            Self::BeginStartup(v) => v.fence().session().pane.server(),
            Self::CompleteStartup(v) => v.fence.session().pane.server(),
            Self::CancelStartup { intent } => intent.stream().server(),
            Self::RevokePairing { pairing } => pairing.server(),
            Self::RefreshWorktrees { scope }
            | Self::CreateWorktree { scope, .. }
            | Self::AssessRemoveWorktree { scope, .. }
            | Self::RemoveWorktreeConfirmed { scope, .. } => scope.server,
        }
    }

    /// Structural scope only. Composition authenticates the stream principal;
    /// reducers still check membership, field versions and current authority.
    pub fn validate_scope(&self, stream: OperationStreamId) -> Result<(), RequestError> {
        let server = stream.server();
        if self.server() != server {
            return Err(RequestError::WrongScope);
        }
        let valid = match self {
            Self::CreateWorkspace { source, .. } => source.server() == server,
            Self::CreateTab { source, .. } => source.server() == server,
            Self::CreatePane { source, .. } => source.server() == server,
            Self::SetPinnedDirectory { value, .. } => {
                value.as_ref().is_none_or(|value| value.server() == server)
            }
            Self::RepairPaneDirectory { value, .. } => value.server() == server,
            Self::CloseConfirmed { assessment, .. }
            | Self::KillWorkspaceConfirmed { assessment, .. }
            | Self::EndAllSessions { assessment, .. } => assessment.operation.stream() == stream,
            Self::CompareAcquire(v) => v.client == stream.client(),
            Self::TakeOver(v) => v.client == stream.client(),
            Self::ReleaseControl(v) => v.grant.client() == stream.client(),
            Self::BeginStartup(v) => v.id().stream() == stream,
            Self::CompleteStartup(v) => {
                v.intent.stream() == stream && v.fence.grant().client() == stream.client()
            }
            Self::CancelStartup { intent } => intent.stream() == stream,
            Self::RefreshWorktrees { scope } | Self::CreateWorktree { scope, .. } => {
                scope.validate().is_ok()
            }
            Self::AssessRemoveWorktree { scope, worktree } => {
                scope.validate().is_ok() && worktree.server() == server
            }
            Self::RemoveWorktreeConfirmed {
                scope,
                worktree,
                assessment,
                ..
            } => {
                scope.validate().is_ok()
                    && worktree.server() == server
                    && assessment.operation.stream() == stream
            }
            _ => true,
        };
        if valid {
            Ok(())
        } else {
            Err(RequestError::WrongScope)
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct OperationBinding {
    pub id: OperationId,
    pub digest: RequestDigest,
}
/// A canonical identity-content binding. Callers cannot change the command after binding.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OperationRequest {
    binding: OperationBinding,
    command: Command,
}
impl OperationRequest {
    pub fn new(id: OperationId, command: Command) -> Result<Self, RequestError> {
        command.validate_scope(id.stream())?;
        if matches!(&command, Command::BeginStartup(intent) if intent.id() != id) {
            return Err(RequestError::WrongScope);
        }
        let digest = RequestDigest::for_command(&command)?;
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
    pub fn verify_binding(&self, binding: OperationBinding) -> Result<(), RequestError> {
        if self.binding == binding {
            Ok(())
        } else {
            Err(RequestError::IdentityReused)
        }
    }
}

/// Typed current values prevent field/value mismatches. Rejected drafts stay client-local.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum FieldConflict {
    WorkspaceName {
        workspace: WorkspaceId,
        current_value: WorkspaceName,
        current_version: FieldVersion,
    },
    PinnedDirectory {
        workspace: WorkspaceId,
        current_value: Option<ServerDirectory>,
        current_version: FieldVersion,
    },
    PaneDirectory {
        pane: PaneId,
        current_value: ServerDirectory,
        current_version: FieldVersion,
    },
    WorktreeLocation {
        server: ServerId,
        current_value: WorktreeLocationTemplate,
        current_version: FieldVersion,
    },
}
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum OperationFailure {
    TargetDeleted,
    StaleAuthority,
    StaleAssessment,
    NotControlled,
    Capacity,
    Exhausted,
    DirectoryUnavailable,
    StorageUnavailable,
    Unsupported,
    Runtime(FailureClass),
}
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum Outcome {
    Applied,
    CreatedWorkspace {
        workspace: WorkspaceId,
        tab: TabId,
        pane: PaneId,
    },
    CreatedTab {
        tab: TabId,
        pane: PaneId,
    },
    CreatedPane {
        pane: PaneId,
    },
    CreatedWorktree {
        worktree: WorktreeId,
    },
    ControlAcquired(ControlGrant),
    ControlUnavailable(ControlRecord),
    StartupAwaitingMeasurement {
        intent: OperationId,
    },
    SessionStarted(SessionKey),
    Assessed {
        assessment: AssessmentRef,
        affected_panes: u32,
        running_sessions: u32,
    },
    Conflict(FieldConflict),
    Failed(OperationFailure),
}

/// Only NeverAdmitted permits fresh admission. Pending and completed retry their
/// original identity. Retired, gap and reuse states can never execute as new work.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum OperationLookup {
    NeverAdmitted,
    Pending,
    Completed {
        result: Outcome,
        publication_revision: OrganizationRevision,
    },
    AcknowledgedRetired,
    SequenceGap {
        expected: OperationSequence,
    },
    IdentityReusedWithDifferentContents,
}

impl Outcome {
    /// Validate retained historical results without requiring their targets to
    /// still exist. A deleted creation result must not recreate its old target.
    pub fn validate_scope(&self, stream: OperationStreamId) -> Result<(), RequestError> {
        let server = stream.server();
        let valid = match self {
            Self::Applied | Self::Failed(_) => true,
            Self::CreatedWorkspace {
                workspace,
                tab,
                pane,
            } => workspace.server() == server && tab.server() == server && pane.server() == server,
            Self::CreatedTab { tab, pane } => tab.server() == server && pane.server() == server,
            Self::CreatedPane { pane } => pane.server() == server,
            Self::CreatedWorktree { worktree } => worktree.server() == server,
            Self::ControlAcquired(grant) => {
                grant.pane().server() == server && grant.client() == stream.client()
            }
            Self::ControlUnavailable(ControlRecord::Unclaimed) => true,
            Self::ControlUnavailable(ControlRecord::Held { client, .. }) => {
                client.server() == server
            }
            Self::StartupAwaitingMeasurement { intent } => intent.stream() == stream,
            Self::SessionStarted(session) => session.pane.server() == server,
            Self::Assessed {
                assessment,
                running_sessions,
                affected_panes,
            } => assessment.operation.stream() == stream && running_sessions <= affected_panes,
            Self::Conflict(conflict) => match conflict {
                FieldConflict::WorkspaceName { workspace, .. } => workspace.server() == server,
                FieldConflict::PinnedDirectory {
                    workspace,
                    current_value,
                    ..
                } => {
                    workspace.server() == server
                        && current_value
                            .as_ref()
                            .is_none_or(|value| value.server() == server)
                }
                FieldConflict::PaneDirectory {
                    pane,
                    current_value,
                    ..
                } => pane.server() == server && current_value.server() == server,
                FieldConflict::WorktreeLocation { server: owner, .. } => *owner == server,
            },
        };
        if valid {
            Ok(())
        } else {
            Err(RequestError::WrongScope)
        }
    }
}
