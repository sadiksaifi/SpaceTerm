//! Shared organization. Personal order, layout, focus, selection and scroll are absent.
//!
//! Records are data, not native filesystem authority. Recovery and publication
//! validate the complete graph. Reducers own mutation ordering and normalization.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::authority::*;
use crate::identity::*;

pub const MAX_NAME_BYTES: usize = 1024;
pub const MAX_DIRECTORY_BYTES: usize = 4096;
pub const MAX_TEMPLATE_BYTES: usize = 4096;

/// Bounded text. Debug output intentionally excludes its contents.
#[derive(Clone, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(try_from = "String", into = "String")]
pub struct BoundedText<const MAX: usize>(String);
impl<const MAX: usize> BoundedText<MAX> {
    pub fn new(value: String) -> Result<Self, OrganizationError> {
        if value.is_empty() || value.len() > MAX || value.chars().any(char::is_control) {
            return Err(OrganizationError::InvalidText);
        }
        Ok(Self(value))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}
impl<const MAX: usize> fmt::Debug for BoundedText<MAX> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("BoundedText([redacted])")
    }
}
impl<const MAX: usize> TryFrom<String> for BoundedText<MAX> {
    type Error = OrganizationError;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}
impl<const MAX: usize> From<BoundedText<MAX>> for String {
    fn from(value: BoundedText<MAX>) -> Self {
        value.0
    }
}
pub type NameText = BoundedText<MAX_NAME_BYTES>;
pub type WorktreeLocationTemplate = BoundedText<MAX_TEMPLATE_BYTES>;
pub type BranchName = BoundedText<MAX_NAME_BYTES>;

/// Opaque path bytes on one server. This never authorizes a client-local action.
/// The owning native adapter validates path form, existence and retained authority.
#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(try_from = "DirectoryParts", into = "DirectoryParts")]
pub struct ServerDirectory {
    server: ServerId,
    bytes: Vec<u8>,
}
#[derive(Clone, Deserialize, Serialize)]
struct DirectoryParts {
    server: ServerId,
    bytes: Vec<u8>,
}
impl ServerDirectory {
    pub fn new(server: ServerId, bytes: Vec<u8>) -> Result<Self, OrganizationError> {
        if bytes.is_empty() || bytes.len() > MAX_DIRECTORY_BYTES || bytes.contains(&0) {
            return Err(OrganizationError::InvalidDirectory);
        }
        Ok(Self { server, bytes })
    }
    pub const fn server(&self) -> ServerId {
        self.server
    }
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }
}
impl fmt::Debug for ServerDirectory {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ServerDirectory")
            .field("server", &self.server)
            .field("bytes", &"[redacted]")
            .finish()
    }
}
impl TryFrom<DirectoryParts> for ServerDirectory {
    type Error = OrganizationError;
    fn try_from(value: DirectoryParts) -> Result<Self, Self::Error> {
        Self::new(value.server, value.bytes)
    }
}
impl From<ServerDirectory> for DirectoryParts {
    fn from(value: ServerDirectory) -> Self {
        Self {
            server: value.server,
            bytes: value.bytes,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Versioned<T> {
    pub value: T,
    pub version: FieldVersion,
}
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum NameMode {
    Automatic,
    Custom,
}
/// The assigned automatic display name is retained, not regenerated on recovery.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct WorkspaceName {
    pub mode: NameMode,
    pub display: NameText,
}
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum NameDraft {
    Automatic,
    Custom(NameText),
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum SplitAxis {
    Horizontal,
    Vertical,
}
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum SplitEdge {
    Before,
    After,
}
/// One bounded link per surviving Pane. Deleted sources must be normalized away
/// in the transaction that deletes them. This is a default, not a shared layout.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum PaneCreation {
    Initial,
    SplitOf {
        source: PaneId,
        axis: SplitAxis,
        edge: SplitEdge,
    },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct WorkspaceRecord {
    pub id: WorkspaceId,
    pub name: Versioned<WorkspaceName>,
    pub pinned_directory: Versioned<Option<ServerDirectory>>,
    pub root_tab: TabId,
    pub creation_ordinal: CreationOrdinal,
}
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct TabRecord {
    pub id: TabId,
    pub workspace: WorkspaceId,
    pub worktree: Option<WorktreeId>,
    pub root_pane: PaneId,
    pub creation_ordinal: CreationOrdinal,
}
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PaneRecord {
    pub id: PaneId,
    pub tab: TabId,
    pub creation_ordinal: CreationOrdinal,
    pub creation: PaneCreation,
    pub starting_directory: ServerDirectory,
    /// Only owning-shell confirmation may populate this field. Prompt Owner and
    /// filesystem proofs stay process-local and never become fields in this DTO.
    pub confirmed_restoration_directory: Versioned<Option<ServerDirectory>>,
}
impl PaneRecord {
    pub fn restoration_directory(&self) -> &ServerDirectory {
        self.confirmed_restoration_directory
            .value
            .as_ref()
            .unwrap_or(&self.starting_directory)
    }
}
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RepositoryRecord {
    pub id: RepositoryId,
    pub directory: ServerDirectory,
}
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct WorktreeRecord {
    pub id: WorktreeId,
    pub repository: RepositoryId,
    pub directory: ServerDirectory,
    pub main: bool,
    pub missing: bool,
}
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ServerSettingsRecord {
    pub worktree_location: Versioned<WorktreeLocationTemplate>,
}

/// Classified failures contain no native error, path, terminal text or credentials.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum FailureClass {
    LaunchUnavailable,
    RuntimeUnavailable,
    RuntimeFault,
    ResourceLimit,
    StorageUnavailable,
    UnsafeStorage,
    AuthorizationDenied,
    DirectoryUnavailable,
    InvalidRequest,
    StaleAuthority,
    Unsupported,
    Internal,
}
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum EndReason {
    OrdinaryExit,
    EndAllSessions,
    Stop,
    RuntimeFailure,
    ServerRestart,
}
/// Worker retirement events must retain this reason. Only ordinary exit reduces
/// hierarchy. Explicit closure removes membership through its durable mutation.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum RetirementReason {
    OrdinaryExit,
    EndAllSessions,
    Stop,
    RuntimeFailure,
    ConfirmedClose,
    KillWorkspace,
}
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum SessionPhase {
    NotStarted,
    Running { generation: SessionGeneration },
    Ended { reason: EndReason },
    Failed { class: FailureClass },
    DirectoryUnavailable,
}
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum ControlRecord {
    Unclaimed,
    Held {
        client: ClientId,
        generation: ControlGeneration,
    },
}
/// Volatile facts published with organization, never recovered as live authority.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PaneRuntimeRecord {
    pub pane: PaneId,
    pub session_generation: SessionGeneration,
    pub phase: SessionPhase,
    /// Retained even when unclaimed, so compare-acquire detects a release race.
    pub control_generation: ControlGeneration,
    pub control: ControlRecord,
}

#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum OrganizationError {
    #[error("text is empty, too long, or contains control characters")]
    InvalidText,
    #[error("invalid server directory record")]
    InvalidDirectory,
    #[error("organization contains an identity from another server")]
    WrongServer,
    #[error("organization contains duplicate identities or creation ordinals")]
    Duplicate,
    #[error("organization membership or Root is invalid")]
    InvalidMembership,
    #[error("creation topology is cyclic or references a deleted Pane")]
    InvalidTopology,
    #[error("runtime facts are inconsistent")]
    InvalidRuntime,
    #[error("publication revision does not advance")]
    InvalidRevision,
}

/// Durable read model. Collection order is not client order. Runtime grants and
/// processes are intentionally separate. Hosts validate this after decoding.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct OrganizationState {
    pub server: ServerId,
    pub revision: OrganizationRevision,
    pub settings: ServerSettingsRecord,
    pub workspaces: Vec<WorkspaceRecord>,
    pub tabs: Vec<TabRecord>,
    pub panes: Vec<PaneRecord>,
    pub repositories: Vec<RepositoryRecord>,
    pub worktrees: Vec<WorktreeRecord>,
}

impl OrganizationState {
    /// Validate server scope, membership, Roots and bounded surviving topology.
    /// Empty organization is valid and never creates a replacement Workspace.
    pub fn validate(&self) -> Result<(), OrganizationError> {
        let mut ordinals = BTreeSet::new();
        let mut workspaces = BTreeMap::new();
        let mut tabs = BTreeMap::new();
        let mut panes = BTreeMap::new();
        let mut repositories = BTreeSet::new();
        let mut worktrees = BTreeMap::new();
        for record in &self.repositories {
            self.check_server(record.id.server())?;
            self.check_server(record.directory.server())?;
            if !repositories.insert(record.id) {
                return Err(OrganizationError::Duplicate);
            }
        }
        for record in &self.worktrees {
            self.check_server(record.id.server())?;
            self.check_server(record.directory.server())?;
            if !repositories.contains(&record.repository) {
                return Err(OrganizationError::InvalidMembership);
            }
            if worktrees.insert(record.id, record).is_some() {
                return Err(OrganizationError::Duplicate);
            }
        }
        for record in &self.workspaces {
            self.check_server(record.id.server())?;
            if let Some(directory) = &record.pinned_directory.value {
                self.check_server(directory.server())?;
            }
            if workspaces.insert(record.id, record).is_some()
                || !ordinals.insert(record.creation_ordinal)
            {
                return Err(OrganizationError::Duplicate);
            }
        }
        for record in &self.tabs {
            self.check_server(record.id.server())?;
            if !workspaces.contains_key(&record.workspace)
                || record
                    .worktree
                    .is_some_and(|id| !worktrees.contains_key(&id))
            {
                return Err(OrganizationError::InvalidMembership);
            }
            if tabs.insert(record.id, record).is_some() || !ordinals.insert(record.creation_ordinal)
            {
                return Err(OrganizationError::Duplicate);
            }
        }
        for record in &self.panes {
            self.check_server(record.id.server())?;
            self.check_server(record.starting_directory.server())?;
            if let Some(directory) = &record.confirmed_restoration_directory.value {
                self.check_server(directory.server())?;
            }
            if !tabs.contains_key(&record.tab) {
                return Err(OrganizationError::InvalidMembership);
            }
            if panes.insert(record.id, record).is_some()
                || !ordinals.insert(record.creation_ordinal)
            {
                return Err(OrganizationError::Duplicate);
            }
        }
        for workspace in workspaces.values() {
            if tabs
                .get(&workspace.root_tab)
                .is_none_or(|tab| tab.workspace != workspace.id)
            {
                return Err(OrganizationError::InvalidMembership);
            }
        }
        for tab in tabs.values() {
            if panes
                .get(&tab.root_pane)
                .is_none_or(|pane| pane.tab != tab.id)
            {
                return Err(OrganizationError::InvalidMembership);
            }
        }
        // Follow each bounded parent link once, with shared visited state. This
        // validates a forest in linear space without recursive stack growth.
        let mut complete = BTreeSet::new();
        let mut initial_tabs = BTreeSet::new();
        for pane in panes.values() {
            if matches!(pane.creation, PaneCreation::Initial) && !initial_tabs.insert(pane.tab) {
                return Err(OrganizationError::InvalidTopology);
            }
            let mut path = BTreeSet::new();
            let mut current = *pane;
            while !complete.contains(&current.id) {
                if !path.insert(current.id) {
                    return Err(OrganizationError::InvalidTopology);
                }
                match current.creation {
                    PaneCreation::Initial => break,
                    PaneCreation::SplitOf { source, .. } => {
                        current = panes
                            .get(&source)
                            .copied()
                            .filter(|source| source.tab == pane.tab)
                            .ok_or(OrganizationError::InvalidTopology)?;
                    }
                }
            }
            complete.extend(path);
        }
        if initial_tabs.len() != tabs.len() {
            return Err(OrganizationError::InvalidTopology);
        }
        Ok(())
    }

    fn check_server(&self, server: ServerId) -> Result<(), OrganizationError> {
        if server != self.server {
            Err(OrganizationError::WrongServer)
        } else {
            Ok(())
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OrganizationSnapshot {
    epoch: ServerEpoch,
    state: OrganizationState,
    runtime: Vec<PaneRuntimeRecord>,
}
impl OrganizationSnapshot {
    pub fn new(
        epoch: ServerEpoch,
        state: OrganizationState,
        runtime: Vec<PaneRuntimeRecord>,
    ) -> Result<Self, OrganizationError> {
        state.validate()?;
        let pane_ids: BTreeSet<_> = state.panes.iter().map(|pane| pane.id).collect();
        let mut seen = BTreeSet::new();
        for record in &runtime {
            validate_runtime(record, state.server)?;
            if !pane_ids.contains(&record.pane) || !seen.insert(record.pane) {
                return Err(OrganizationError::InvalidRuntime);
            }
        }
        if seen != pane_ids {
            return Err(OrganizationError::InvalidRuntime);
        }
        Ok(Self {
            epoch,
            state,
            runtime,
        })
    }
    pub const fn epoch(&self) -> ServerEpoch {
        self.epoch
    }
    pub fn state(&self) -> &OrganizationState {
        &self.state
    }
    pub fn runtime(&self) -> &[PaneRuntimeRecord] {
        &self.runtime
    }
}

fn validate_runtime(record: &PaneRuntimeRecord, server: ServerId) -> Result<(), OrganizationError> {
    if record.pane.server() != server {
        return Err(OrganizationError::WrongServer);
    }
    if let SessionPhase::Running { generation } = record.phase
        && generation != record.session_generation
    {
        return Err(OrganizationError::InvalidRuntime);
    }
    if let ControlRecord::Held { client, generation } = record.control
        && (client.server() != server || generation != record.control_generation)
    {
        return Err(OrganizationError::InvalidRuntime);
    }
    Ok(())
}

/// Durable typed changes. Each field carries its own version in its record.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum OrganizationWrite {
    PutWorkspace(WorkspaceRecord),
    DeleteWorkspace(WorkspaceId),
    PutTab(TabRecord),
    DeleteTab(TabId),
    PutPane(PaneRecord),
    DeletePane(PaneId),
    PutRepository(RepositoryRecord),
    DeleteRepository(RepositoryId),
    PutWorktree(WorktreeRecord),
    DeleteWorktree(WorktreeId),
    SetServerSettings(ServerSettingsRecord),
}
impl OrganizationWrite {
    pub fn server(&self) -> Option<ServerId> {
        Some(match self {
            Self::PutWorkspace(v) => v.id.server(),
            Self::DeleteWorkspace(v) => v.server(),
            Self::PutTab(v) => v.id.server(),
            Self::DeleteTab(v) => v.server(),
            Self::PutPane(v) => v.id.server(),
            Self::DeletePane(v) => v.server(),
            Self::PutRepository(v) => v.id.server(),
            Self::DeleteRepository(v) => v.server(),
            Self::PutWorktree(v) => v.id.server(),
            Self::DeleteWorktree(v) => v.server(),
            Self::SetServerSettings(_) => return None,
        })
    }
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OrganizationDelta {
    server: ServerId,
    epoch: ServerEpoch,
    base: OrganizationRevision,
    revision: OrganizationRevision,
    writes: Vec<OrganizationWrite>,
    runtime: Vec<PaneRuntimeRecord>,
}
impl OrganizationDelta {
    /// May batch contiguous publications. A missing base requires a new snapshot,
    /// not best-effort application. Client-local arrangements are never included.
    pub fn new(
        server: ServerId,
        epoch: ServerEpoch,
        base: OrganizationRevision,
        revision: OrganizationRevision,
        writes: Vec<OrganizationWrite>,
        runtime: Vec<PaneRuntimeRecord>,
    ) -> Result<Self, OrganizationError> {
        if revision <= base {
            return Err(OrganizationError::InvalidRevision);
        }
        for write in &writes {
            write.validate_scope(server)?;
        }
        let mut seen = BTreeSet::new();
        for record in &runtime {
            validate_runtime(record, server)?;
            if !seen.insert(record.pane) {
                return Err(OrganizationError::Duplicate);
            }
        }
        Ok(Self {
            server,
            epoch,
            base,
            revision,
            writes,
            runtime,
        })
    }
    pub const fn server(&self) -> ServerId {
        self.server
    }
    pub const fn epoch(&self) -> ServerEpoch {
        self.epoch
    }
    pub const fn base(&self) -> OrganizationRevision {
        self.base
    }
    pub const fn revision(&self) -> OrganizationRevision {
        self.revision
    }
    pub fn writes(&self) -> &[OrganizationWrite] {
        &self.writes
    }
    pub fn runtime(&self) -> &[PaneRuntimeRecord] {
        &self.runtime
    }
}

impl OrganizationWrite {
    /// Partial records still require server-qualified references. Complete graph
    /// validation is required after all writes are applied inside a transaction.
    pub fn validate_scope(&self, server: ServerId) -> Result<(), OrganizationError> {
        if self.server().is_some_and(|id| id != server) {
            return Err(OrganizationError::WrongServer);
        }
        let valid = match self {
            Self::PutWorkspace(v) => {
                v.root_tab.server() == server
                    && v.pinned_directory
                        .value
                        .as_ref()
                        .is_none_or(|directory| directory.server() == server)
            }
            Self::PutTab(v) => {
                v.workspace.server() == server
                    && v.root_pane.server() == server
                    && v.worktree.is_none_or(|id| id.server() == server)
            }
            Self::PutPane(v) => {
                v.tab.server() == server
                    && v.starting_directory.server() == server
                    && v.confirmed_restoration_directory
                        .value
                        .as_ref()
                        .is_none_or(|directory| directory.server() == server)
                    && !matches!(v.creation, PaneCreation::SplitOf { source, .. } if source.server() != server)
            }
            Self::PutRepository(v) => v.directory.server() == server,
            Self::PutWorktree(v) => {
                v.repository.server() == server && v.directory.server() == server
            }
            _ => true,
        };
        if valid {
            Ok(())
        } else {
            Err(OrganizationError::WrongServer)
        }
    }
}
