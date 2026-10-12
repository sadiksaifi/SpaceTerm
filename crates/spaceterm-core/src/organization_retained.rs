//! Additive retained organization facts. No layout normalization or mutation
//! policy lives here. Records contain comparison data, never live grants or PTYs.
//! Codecs must preflight counts and lengths before allocating recovery vectors.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::authority::{ControlGeneration, FieldVersion, SessionGeneration};
use crate::identity::*;
use crate::organization::{
    NameMode, NameText, OrganizationError, OrganizationState, PaneCreation, PaneRecord, Versioned,
};
use crate::remote_access::RemoteAccessPolicy;

/// Domain-value ceiling. Reducer OrganizationLimits can impose lower bounds.
/// Per-kind production quotas remain reducer policy, not inferred from this cap.
pub const MAX_RETAINED_ORGANIZATION_BYTES: usize = 64 * 1024 * 1024;

/// Retained automatic-name base and assignment, including while a custom name
/// is displayed. Recovery must not recalculate or renumber these values.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct WorkspaceNameBasis {
    workspace: WorkspaceId,
    automatic_base: NameText,
    assigned_automatic: NameText,
}

impl WorkspaceNameBasis {
    pub const fn new(
        workspace: WorkspaceId,
        automatic_base: NameText,
        assigned_automatic: NameText,
    ) -> Self {
        Self {
            workspace,
            automatic_base,
            assigned_automatic,
        }
    }

    pub const fn workspace(&self) -> WorkspaceId {
        self.workspace
    }
    pub fn automatic_base(&self) -> &NameText {
        &self.automatic_base
    }
    pub fn assigned_automatic(&self) -> &NameText {
        &self.assigned_automatic
    }
}

/// Explicit replay order for the surviving PaneCreation instructions in one Tab.
/// Replay order is independent of retained creation ordinals and Root Pane.
/// Every split source precedes its new Pane. There is no ratio, focus, zoom,
/// deleted source, or personal arrangement in this record.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(try_from = "TabCreationDefaultParts", into = "TabCreationDefaultParts")]
pub struct TabCreationDefault {
    tab: TabId,
    replay_order: Vec<PaneId>,
}

#[derive(Clone, Deserialize, Serialize)]
struct TabCreationDefaultParts {
    tab: TabId,
    replay_order: Vec<PaneId>,
}

impl TabCreationDefault {
    pub fn new(tab: TabId, replay_order: Vec<PaneId>) -> Result<Self, OrganizationError> {
        if replay_order.is_empty() || replay_order.len() > usize::from(u16::MAX) {
            return Err(OrganizationError::InvalidTopology);
        }
        let mut seen = BTreeSet::new();
        for pane in &replay_order {
            if pane.server() != tab.server() {
                return Err(OrganizationError::WrongServer);
            }
            if !seen.insert(*pane) {
                return Err(OrganizationError::Duplicate);
            }
        }
        Ok(Self { tab, replay_order })
    }

    pub const fn tab(&self) -> TabId {
        self.tab
    }
    pub fn replay_order(&self) -> &[PaneId] {
        &self.replay_order
    }

    /// Validate exact membership and reconstruction order, without choosing it.
    pub fn validate(&self, panes: &[PaneRecord]) -> Result<(), OrganizationError> {
        let records: BTreeMap<_, _> = panes
            .iter()
            .filter(|pane| pane.tab == self.tab)
            .map(|pane| (pane.id, pane))
            .collect();
        let count = panes.iter().filter(|pane| pane.tab == self.tab).count();
        if records.len() != count || records.len() != self.replay_order.len() {
            return Err(OrganizationError::InvalidTopology);
        }
        let mut seen = BTreeSet::new();
        for (index, id) in self.replay_order.iter().enumerate() {
            let pane = records.get(id).ok_or(OrganizationError::InvalidTopology)?;
            match pane.creation {
                PaneCreation::Initial if index == 0 => {}
                PaneCreation::SplitOf { source, .. } if index > 0 && seen.contains(&source) => {}
                _ => return Err(OrganizationError::InvalidTopology),
            }
            if !seen.insert(*id) {
                return Err(OrganizationError::InvalidTopology);
            }
        }
        Ok(())
    }
}

impl TryFrom<TabCreationDefaultParts> for TabCreationDefault {
    type Error = OrganizationError;
    fn try_from(parts: TabCreationDefaultParts) -> Result<Self, Self::Error> {
        Self::new(parts.tab, parts.replay_order)
    }
}
impl From<TabCreationDefault> for TabCreationDefaultParts {
    fn from(value: TabCreationDefault) -> Self {
        Self {
            tab: value.tab,
            replay_order: value.replay_order,
        }
    }
}

/// Numeric comparison high-water facts only. Ownership and process state are
/// cleared on new-epoch recovery. Zero remains the never-used comparison value.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PaneGenerationHighWater {
    pane: PaneId,
    session: SessionGeneration,
    control: ControlGeneration,
}

impl PaneGenerationHighWater {
    pub const fn new(pane: PaneId, session: SessionGeneration, control: ControlGeneration) -> Self {
        Self {
            pane,
            session,
            control,
        }
    }
    pub const fn pane(self) -> PaneId {
        self.pane
    }
    pub const fn session(self) -> SessionGeneration {
        self.session
    }
    pub const fn control(self) -> ControlGeneration {
        self.control
    }

    pub fn dominates(self, previous: Self) -> bool {
        self.pane == previous.pane
            && self.session >= previous.session
            && self.control >= previous.control
    }
}

macro_rules! comparison_record {
    ($name:ident, $id:ty, $doc:literal) => {
        #[doc = $doc]
        #[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
        pub struct $name {
            id: $id,
            version: FieldVersion,
        }
        impl $name {
            pub const fn new(id: $id, version: FieldVersion) -> Self {
                Self { id, version }
            }
            pub const fn id(self) -> $id {
                self.id
            }
            pub const fn version(self) -> FieldVersion {
                self.version
            }
        }
    };
}
comparison_record!(
    TabMembershipVersion,
    TabId,
    "Version of one Tab's Worktree membership."
);
comparison_record!(
    RepositoryInventoryVersion,
    RepositoryId,
    "Comparison version for an owning repository's committed inventory."
);

/// Listing is independent of WorktreeRecord::missing. Referenced unlisted or
/// missing Worktrees remain retained; inventory policy belongs to Git.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct WorktreeInventoryFacts {
    worktree: WorktreeId,
    listed: bool,
    version: FieldVersion,
}
impl WorktreeInventoryFacts {
    pub const fn new(worktree: WorktreeId, listed: bool, version: FieldVersion) -> Self {
        Self {
            worktree,
            listed,
            version,
        }
    }
    pub const fn worktree(self) -> WorktreeId {
        self.worktree
    }
    pub const fn listed(self) -> bool {
        self.listed
    }
    pub const fn version(self) -> FieldVersion {
        self.version
    }
}

/// Codec input. It is not live state until the checked constructor and complete
/// OrganizationState validation succeed. Collection order is stable ID order.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct OrganizationRetainedParts {
    pub server: ServerId,
    pub workspace_names: Vec<WorkspaceNameBasis>,
    pub creation_defaults: Vec<TabCreationDefault>,
    pub pane_generations: Vec<PaneGenerationHighWater>,
    pub tab_membership_versions: Vec<TabMembershipVersion>,
    pub repository_inventory_versions: Vec<RepositoryInventoryVersion>,
    pub worktree_inventory: Vec<WorktreeInventoryFacts>,
    pub remote_access: Versioned<RemoteAccessPolicy>,
}

/// The additional durable facts absent from Wave 1's OrganizationState.
/// Pending retirement descriptors are intentionally absent: the ledger owns them.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(
    try_from = "OrganizationRetainedParts",
    into = "OrganizationRetainedParts"
)]
pub struct OrganizationRetainedFacts(OrganizationRetainedParts);

fn ordered<I: Copy + Ord>(ids: impl Iterator<Item = I>) -> bool {
    let mut previous = None;
    for id in ids {
        if previous.is_some_and(|value| value >= id) {
            return false;
        }
        previous = Some(id);
    }
    true
}

fn charge(total: &mut usize, bytes: usize) -> Result<(), OrganizationError> {
    *total = total
        .checked_add(bytes)
        .ok_or(OrganizationError::QuotaExceeded)?;
    if *total > MAX_RETAINED_ORGANIZATION_BYTES {
        return Err(OrganizationError::QuotaExceeded);
    }
    Ok(())
}

impl OrganizationRetainedFacts {
    pub fn new(parts: OrganizationRetainedParts) -> Result<Self, OrganizationError> {
        // Every record contains at least one 24-byte server-qualified identity.
        // This cheap count preflight precedes collection traversal and indexing.
        let max_records = MAX_RETAINED_ORGANIZATION_BYTES / 24;
        if [
            parts.workspace_names.len(),
            parts.creation_defaults.len(),
            parts.pane_generations.len(),
            parts.tab_membership_versions.len(),
            parts.repository_inventory_versions.len(),
            parts.worktree_inventory.len(),
        ]
        .into_iter()
        .any(|count| count > max_records)
        {
            return Err(OrganizationError::QuotaExceeded);
        }
        if parts.remote_access.value.server() != parts.server
            || parts
                .workspace_names
                .iter()
                .any(|v| v.workspace().server() != parts.server)
            || parts
                .creation_defaults
                .iter()
                .any(|v| v.tab().server() != parts.server)
            || parts
                .pane_generations
                .iter()
                .any(|v| v.pane().server() != parts.server)
            || parts
                .tab_membership_versions
                .iter()
                .any(|v| v.id().server() != parts.server)
            || parts
                .repository_inventory_versions
                .iter()
                .any(|v| v.id().server() != parts.server)
            || parts
                .worktree_inventory
                .iter()
                .any(|v| v.worktree().server() != parts.server)
        {
            return Err(OrganizationError::WrongServer);
        }
        if !ordered(
            parts
                .workspace_names
                .iter()
                .map(WorkspaceNameBasis::workspace),
        ) || !ordered(parts.creation_defaults.iter().map(TabCreationDefault::tab))
            || !ordered(parts.pane_generations.iter().map(|v| v.pane()))
            || !ordered(parts.tab_membership_versions.iter().map(|v| v.id()))
            || !ordered(parts.repository_inventory_versions.iter().map(|v| v.id()))
            || !ordered(parts.worktree_inventory.iter().map(|v| v.worktree()))
        {
            return Err(OrganizationError::Duplicate);
        }
        let value = Self(parts);
        value.value_bytes()?;
        Ok(value)
    }

    pub const fn server(&self) -> ServerId {
        self.0.server
    }
    pub fn parts(&self) -> &OrganizationRetainedParts {
        &self.0
    }
    pub fn workspace_names(&self) -> &[WorkspaceNameBasis] {
        &self.0.workspace_names
    }
    pub fn creation_defaults(&self) -> &[TabCreationDefault] {
        &self.0.creation_defaults
    }
    pub fn pane_generations(&self) -> &[PaneGenerationHighWater] {
        &self.0.pane_generations
    }
    pub fn tab_membership_versions(&self) -> &[TabMembershipVersion] {
        &self.0.tab_membership_versions
    }
    pub fn repository_inventory_versions(&self) -> &[RepositoryInventoryVersion] {
        &self.0.repository_inventory_versions
    }
    pub fn worktree_inventory(&self) -> &[WorktreeInventoryFacts] {
        &self.0.worktree_inventory
    }
    pub fn remote_access(&self) -> &Versioned<RemoteAccessPolicy> {
        &self.0.remote_access
    }

    /// Conservative domain-value charge, independent of Rust memory layout or JSON.
    /// Codec and store allocation budgets are separate and may impose lower limits.
    pub fn value_bytes(&self) -> Result<usize, OrganizationError> {
        let mut total = 64;
        charge(
            &mut total,
            self.0
                .remote_access
                .value
                .endpoints()
                .len()
                .checked_mul(28)
                .ok_or(OrganizationError::QuotaExceeded)?,
        )?;
        for value in &self.0.workspace_names {
            charge(&mut total, 32)?;
            charge(&mut total, value.automatic_base().as_str().len())?;
            charge(&mut total, value.assigned_automatic().as_str().len())?;
        }
        for value in &self.0.creation_defaults {
            charge(&mut total, 32)?;
            charge(
                &mut total,
                value
                    .replay_order()
                    .len()
                    .checked_mul(24)
                    .ok_or(OrganizationError::QuotaExceeded)?,
            )?;
        }
        for (count, width) in [
            (self.0.pane_generations.len(), 40),
            (self.0.tab_membership_versions.len(), 32),
            (self.0.repository_inventory_versions.len(), 32),
            (self.0.worktree_inventory.len(), 40),
        ] {
            charge(
                &mut total,
                count
                    .checked_mul(width)
                    .ok_or(OrganizationError::QuotaExceeded)?,
            )?;
        }
        Ok(total)
    }

    /// Validate one-to-one projections against the complete surviving graph.
    /// This does not choose Roots, names, replay order, or inventory membership.
    pub fn validate_against(&self, state: &OrganizationState) -> Result<(), OrganizationError> {
        if self.server() != state.server {
            return Err(OrganizationError::WrongServer);
        }
        if self.0.workspace_names.len() != state.workspaces.len()
            || self.0.creation_defaults.len() != state.tabs.len()
            || self.0.pane_generations.len() != state.panes.len()
            || self.0.tab_membership_versions.len() != state.tabs.len()
            || self.0.repository_inventory_versions.len() != state.repositories.len()
            || self.0.worktree_inventory.len() != state.worktrees.len()
        {
            return Err(OrganizationError::InvalidRetention);
        }
        let mut bytes = self.value_bytes()?;
        charge(
            &mut bytes,
            state.settings.worktree_location.value.as_str().len(),
        )?;
        for value in &state.workspaces {
            if value.creation_ordinal.get() == 0 {
                return Err(OrganizationError::InvalidRetention);
            }
            charge(&mut bytes, 128)?;
            charge(&mut bytes, value.name.value.display.as_str().len())?;
            if let Some(directory) = &value.pinned_directory.value {
                charge(&mut bytes, directory.bytes().len())?;
            }
        }
        for value in &state.tabs {
            if value.creation_ordinal.get() == 0 {
                return Err(OrganizationError::InvalidRetention);
            }
            charge(&mut bytes, 128)?;
        }
        for value in &state.panes {
            if value.creation_ordinal.get() == 0 {
                return Err(OrganizationError::InvalidRetention);
            }
            charge(&mut bytes, 160)?;
            charge(&mut bytes, value.starting_directory.bytes().len())?;
            if let Some(directory) = &value.confirmed_restoration_directory.value {
                charge(&mut bytes, directory.bytes().len())?;
            }
        }
        for value in &state.repositories {
            charge(&mut bytes, 48)?;
            charge(&mut bytes, value.directory.bytes().len())?;
        }
        for value in &state.worktrees {
            charge(&mut bytes, 80)?;
            charge(&mut bytes, value.directory.bytes().len())?;
        }
        let workspaces: BTreeMap<_, _> = state.workspaces.iter().map(|v| (v.id, v)).collect();
        let tabs: BTreeSet<_> = state.tabs.iter().map(|v| v.id).collect();
        let panes: BTreeSet<_> = state.panes.iter().map(|v| v.id).collect();
        let repositories: BTreeSet<_> = state.repositories.iter().map(|v| v.id).collect();
        let worktrees: BTreeSet<_> = state.worktrees.iter().map(|v| v.id).collect();
        for value in &self.0.workspace_names {
            let record = workspaces
                .get(&value.workspace())
                .ok_or(OrganizationError::InvalidRetention)?;
            if record.name.value.mode == NameMode::Automatic
                && record.name.value.display != *value.assigned_automatic()
            {
                return Err(OrganizationError::InvalidRetention);
            }
        }
        for value in &self.0.creation_defaults {
            if !tabs.contains(&value.tab()) {
                return Err(OrganizationError::InvalidRetention);
            }
            value.validate(&state.panes)?;
        }
        if self
            .0
            .pane_generations
            .iter()
            .any(|v| !panes.contains(&v.pane()))
            || self
                .0
                .tab_membership_versions
                .iter()
                .any(|v| !tabs.contains(&v.id()))
            || self
                .0
                .repository_inventory_versions
                .iter()
                .any(|v| !repositories.contains(&v.id()))
            || self
                .0
                .worktree_inventory
                .iter()
                .any(|v| !worktrees.contains(&v.worktree()))
        {
            return Err(OrganizationError::InvalidRetention);
        }
        Ok(())
    }

    /// Coverage of comparison records by persisted identity marks. Generation
    /// dominance against previous state is checked by the store transaction.
    pub fn fits_high_water(&self, marks: IdentityHighWater) -> bool {
        self.0
            .workspace_names
            .iter()
            .all(|v| v.workspace().local() <= marks.workspace)
            && self.0.creation_defaults.iter().all(|v| {
                v.tab().local() <= marks.tab
                    && v.replay_order()
                        .iter()
                        .all(|pane| pane.local() <= marks.pane)
            })
            && self
                .0
                .pane_generations
                .iter()
                .all(|v| v.pane().local() <= marks.pane)
            && self
                .0
                .tab_membership_versions
                .iter()
                .all(|v| v.id().local() <= marks.tab)
            && self
                .0
                .repository_inventory_versions
                .iter()
                .all(|v| v.id().local() <= marks.repository)
            && self
                .0
                .worktree_inventory
                .iter()
                .all(|v| v.worktree().local() <= marks.worktree)
    }
}

impl TryFrom<OrganizationRetainedParts> for OrganizationRetainedFacts {
    type Error = OrganizationError;
    fn try_from(parts: OrganizationRetainedParts) -> Result<Self, Self::Error> {
        Self::new(parts)
    }
}
impl From<OrganizationRetainedFacts> for OrganizationRetainedParts {
    fn from(value: OrganizationRetainedFacts) -> Self {
        value.0
    }
}

/// Compact typed writes for the additional retained facts. Field versions and
/// generation dominance are compared by the store under expected StoreRevision.
/// These are shared organization writes, not an auxiliary second commit path.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum RetainedOrganizationWrite {
    PutWorkspaceNameBasis(WorkspaceNameBasis),
    DeleteWorkspaceNameBasis(WorkspaceId),
    PutCreationDefault(TabCreationDefault),
    DeleteCreationDefault(TabId),
    SetPaneGenerationHighWater(PaneGenerationHighWater),
    DeletePaneGenerationHighWater(PaneId),
    SetTabMembershipVersion(TabMembershipVersion),
    DeleteTabMembershipVersion(TabId),
    SetRepositoryInventoryVersion(RepositoryInventoryVersion),
    DeleteRepositoryInventoryVersion(RepositoryId),
    SetWorktreeInventoryFacts(WorktreeInventoryFacts),
    DeleteWorktreeInventoryFacts(WorktreeId),
    SetRemoteAccess(Versioned<RemoteAccessPolicy>),
}

impl RetainedOrganizationWrite {
    pub fn server(&self) -> ServerId {
        match self {
            Self::PutWorkspaceNameBasis(v) => v.workspace().server(),
            Self::DeleteWorkspaceNameBasis(v) => v.server(),
            Self::PutCreationDefault(v) => v.tab().server(),
            Self::DeleteCreationDefault(v) => v.server(),
            Self::SetPaneGenerationHighWater(v) => v.pane().server(),
            Self::DeletePaneGenerationHighWater(v) => v.server(),
            Self::SetTabMembershipVersion(v) => v.id().server(),
            Self::DeleteTabMembershipVersion(v) => v.server(),
            Self::SetRepositoryInventoryVersion(v) => v.id().server(),
            Self::DeleteRepositoryInventoryVersion(v) => v.server(),
            Self::SetWorktreeInventoryFacts(v) => v.worktree().server(),
            Self::DeleteWorktreeInventoryFacts(v) => v.server(),
            Self::SetRemoteAccess(v) => v.value.server(),
        }
    }

    pub fn validate_scope(&self, server: ServerId) -> Result<(), OrganizationError> {
        if self.server() != server {
            return Err(OrganizationError::WrongServer);
        }
        Ok(())
    }

    pub fn fits_high_water(&self, marks: IdentityHighWater) -> bool {
        match self {
            Self::PutWorkspaceNameBasis(v) => v.workspace().local() <= marks.workspace,
            Self::DeleteWorkspaceNameBasis(v) => v.local() <= marks.workspace,
            Self::PutCreationDefault(v) => {
                v.tab().local() <= marks.tab
                    && v.replay_order()
                        .iter()
                        .all(|pane| pane.local() <= marks.pane)
            }
            Self::DeleteCreationDefault(v) => v.local() <= marks.tab,
            Self::SetPaneGenerationHighWater(v) => v.pane().local() <= marks.pane,
            Self::DeletePaneGenerationHighWater(v) => v.local() <= marks.pane,
            Self::SetTabMembershipVersion(v) => v.id().local() <= marks.tab,
            Self::DeleteTabMembershipVersion(v) => v.local() <= marks.tab,
            Self::SetRepositoryInventoryVersion(v) => v.id().local() <= marks.repository,
            Self::DeleteRepositoryInventoryVersion(v) => v.local() <= marks.repository,
            Self::SetWorktreeInventoryFacts(v) => v.worktree().local() <= marks.worktree,
            Self::DeleteWorktreeInventoryFacts(v) => v.local() <= marks.worktree,
            Self::SetRemoteAccess(_) => true,
        }
    }
}
