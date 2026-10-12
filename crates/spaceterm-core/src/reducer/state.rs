use std::collections::{BTreeMap, BTreeSet};

use super::contracts::*;
use crate::authority::*;
use crate::identity::*;
use crate::operations::*;
use crate::organization::*;
use crate::organization_retained::*;
use crate::retirement::*;
use crate::worktrees::intent::*;

/// Lower policy ceilings, bounded by the frozen domain representations.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct OrganizationLimits {
    pub workspaces: usize,
    pub tabs: usize,
    pub tabs_per_workspace: usize,
    pub panes: usize,
    pub panes_per_tab: usize,
    pub worktrees: usize,
    pub repositories: usize,
    pub value_bytes: usize,
    pub name_bytes: usize,
    pub directory_bytes: usize,
    pub template_bytes: usize,
    pub assessments_per_client: usize,
    pub assessments: usize,
    pub assessment_bytes: usize,
    pub assessment_lifetime_ms: u64,
    pub close_work_max_age_ms: u64,
    pub retirement_batches: usize,
    pub retirement_bytes: usize,
}
impl Default for OrganizationLimits {
    fn default() -> Self {
        Self {
            workspaces: 256,
            tabs: 2048,
            tabs_per_workspace: 128,
            panes: 8192,
            panes_per_tab: 256,
            worktrees: 4096,
            repositories: 4096,
            value_bytes: MAX_RETAINED_ORGANIZATION_BYTES,
            name_bytes: 1024,
            directory_bytes: 4096,
            template_bytes: 1024,
            assessments_per_client: 16,
            assessments: 256,
            assessment_bytes: 32 * 1024 * 1024,
            assessment_lifetime_ms: 60_000,
            close_work_max_age_ms: 5_000,
            retirement_batches: 64,
            retirement_bytes: MAX_RETAINED_RETIREMENT_BYTES,
        }
    }
}
impl OrganizationLimits {
    pub fn validate(self) -> Result<Self, ReducerFailure> {
        let hard = Self::default();
        if self.workspaces > hard.workspaces
            || self.tabs > hard.tabs
            || self.panes > hard.panes
            || self.tabs_per_workspace > hard.tabs_per_workspace
            || self.panes_per_tab > hard.panes_per_tab
            || self.worktrees > hard.worktrees
            || self.repositories > hard.repositories
            || self.value_bytes > hard.value_bytes
            || self.name_bytes > hard.name_bytes
            || self.directory_bytes > hard.directory_bytes
            || self.template_bytes > hard.template_bytes
            || self.assessments > hard.assessments
            || self.assessments_per_client > hard.assessments_per_client
            || self.assessment_bytes > hard.assessment_bytes
            || self.assessment_lifetime_ms > hard.assessment_lifetime_ms
            || self.retirement_batches > hard.retirement_batches
            || self.retirement_bytes > hard.retirement_bytes
            || self.name_bytes == 0
            || self.directory_bytes == 0
            || self.template_bytes == 0
            || self.assessment_lifetime_ms == 0
            || self.close_work_max_age_ms == 0
        {
            return Err(ReducerFailure::InvalidFacts);
        }
        Ok(self)
    }
}

/// Recovery input and bounded retained read model. `server_home` is a native
/// composition input, not another persisted preference or filesystem proof.
/// Pending descriptors are projections from ledger/native recovery only.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RetainedOrganization {
    pub(crate) organization: OrganizationState,
    pub(crate) high_water: IdentityHighWater,
    pub(crate) server_home: ServerDirectory,
    pub(crate) retirements: Vec<RetirementDescriptor>,
    pub(crate) native_retirements: Vec<NativeRetirementDescriptor>,
    pub(crate) git_allocations: Vec<GitAllocationReservation>,
}
impl RetainedOrganization {
    pub fn new(
        organization: OrganizationState,
        high_water: IdentityHighWater,
        server_home: ServerDirectory,
    ) -> Self {
        Self {
            organization,
            high_water,
            server_home,
            retirements: Vec::new(),
            native_retirements: Vec::new(),
            git_allocations: Vec::new(),
        }
    }
    pub fn with_pending_retirements(mut self, values: Vec<RetirementDescriptor>) -> Self {
        self.retirements = values;
        self
    }
    pub fn with_native_retirements(mut self, values: Vec<NativeRetirementDescriptor>) -> Self {
        self.native_retirements = values;
        self
    }
    pub fn with_git_allocations(mut self, values: Vec<GitAllocationReservation>) -> Self {
        self.git_allocations = values;
        self
    }
    pub fn organization(&self) -> &OrganizationState {
        &self.organization
    }
    pub fn high_water(&self) -> IdentityHighWater {
        self.high_water
    }
    pub fn server_home(&self) -> &ServerDirectory {
        &self.server_home
    }
    pub fn pending_retirements(&self) -> &[RetirementDescriptor] {
        &self.retirements
    }
    pub fn native_retirements(&self) -> &[NativeRetirementDescriptor] {
        &self.native_retirements
    }
    pub fn git_allocations(&self) -> &[GitAllocationReservation] {
        &self.git_allocations
    }

    pub fn validate(&self, limits: OrganizationLimits) -> Result<(), ReducerFailure> {
        let limits = limits.validate()?;
        if self.server_home.server() != self.organization.server {
            return invalid(DurableInvariant::Graph);
        }
        check_limits(&self.organization, limits)
            .map_err(|_| ReducerFailure::InvalidDurableState(DurableInvariant::Limits))?;
        self.organization
            .validate()
            .map_err(|_| ReducerFailure::InvalidDurableState(DurableInvariant::Graph))?;
        let facts =
            self.organization
                .retained
                .as_ref()
                .ok_or(ReducerFailure::InvalidDurableState(
                    DurableInvariant::RetainedFacts,
                ))?;
        let marks = self.high_water;
        if !facts.fits_high_water(marks)
            || self.organization.workspaces.iter().any(|v| {
                v.id.local() > marks.workspace || v.creation_ordinal.get() > marks.creation_ordinal
            })
            || self.organization.tabs.iter().any(|v| {
                v.id.local() > marks.tab || v.creation_ordinal.get() > marks.creation_ordinal
            })
            || self.organization.panes.iter().any(|v| {
                v.id.local() > marks.pane || v.creation_ordinal.get() > marks.creation_ordinal
            })
            || self
                .organization
                .repositories
                .iter()
                .any(|v| v.id.local() > marks.repository)
            || self
                .organization
                .worktrees
                .iter()
                .any(|v| v.id.local() > marks.worktree)
        {
            return invalid(DurableInvariant::HighWater);
        }
        if self
            .organization
            .workspaces
            .iter()
            .any(|v| v.name.version.get() == 0 || v.pinned_directory.version.get() == 0)
            || self
                .organization
                .panes
                .iter()
                .any(|v| v.confirmed_restoration_directory.version.get() == 0)
            || self.organization.settings.worktree_location.version.get() == 0
            || facts.remote_access().version.get() == 0
            || facts
                .tab_membership_versions()
                .iter()
                .any(|v| v.version().get() == 0)
            || facts
                .repository_inventory_versions()
                .iter()
                .any(|v| v.version().get() == 0)
            || facts
                .worktree_inventory()
                .iter()
                .any(|v| v.version().get() == 0)
        {
            return invalid(DurableInvariant::FieldVersion);
        }
        if !super::server_settings::valid_template(
            self.organization.settings.worktree_location.value.as_str(),
            limits.template_bytes,
        ) {
            return invalid(DurableInvariant::WorktreeLocation);
        }
        let mut occupied: BTreeSet<_> = self
            .organization
            .workspaces
            .iter()
            .filter(|v| v.name.value.mode == NameMode::Custom)
            .map(|v| &v.name.value.display)
            .collect();
        if self
            .organization
            .workspaces
            .iter()
            .filter(|v| v.name.value.mode == NameMode::Automatic)
            .any(|v| !occupied.insert(&v.name.value.display))
        {
            return invalid(DurableInvariant::Names);
        }
        validate_pending(self, limits)?;
        super::lifecycle::validate_retirement_recovery(self)?;
        Ok(())
    }

    /// Preview the one pure deletion cascade without granting deletion or process
    /// authority. Close and lifecycle policy wrap this same cascade in a staged
    /// reduction after their own exact assessment/runtime checks.
    pub fn preview_removal(
        &self,
        panes: &[PaneId],
        limits: OrganizationLimits,
    ) -> Result<Self, ReducerFailure> {
        self.validate(limits)?;
        let previous_epoch = self
            .retirements
            .iter()
            .map(|v| v.batch().epoch())
            .chain(self.native_retirements.iter().map(|v| v.batch().epoch()))
            .max()
            .unwrap_or_default();
        let epoch = previous_epoch
            .checked_successor()
            .ok_or(ReducerFailure::CounterExhausted)?;
        let state = ReducerState::recover(self.clone(), StoreRevision::new(0), epoch, limits)?;
        let patch = super::remove_hierarchy(&state, panes)?;
        Ok(patch.candidate.retained())
    }
}

fn invalid<T>(code: DurableInvariant) -> Result<T, ReducerFailure> {
    Err(ReducerFailure::InvalidDurableState(code))
}

fn validate_pending(
    value: &RetainedOrganization,
    limits: OrganizationLimits,
) -> Result<(), ReducerFailure> {
    if value
        .retirements
        .len()
        .checked_add(value.native_retirements.len())
        .is_none_or(|n| n > limits.retirement_batches)
    {
        return invalid(DurableInvariant::Retirement);
    }
    let mut bytes = 0usize;
    let mut batches = BTreeSet::new();
    let mut operations = BTreeSet::new();
    let mut barriers = 0;
    for descriptor in &value.retirements {
        if descriptor.batch().server() != value.organization.server
            || descriptor.acceptance_revision() > value.organization.revision
            || !batches.insert(descriptor.batch())
            || !operations.insert(descriptor.binding().id)
            || !binding_fits(
                descriptor.binding(),
                value.high_water,
                value.organization.server,
            )
            || !descriptor.target().fits_high_water(value.high_water)
        {
            return invalid(DurableInvariant::Retirement);
        }
        bytes =
            bytes
                .checked_add(descriptor.value_bytes().map_err(|_| {
                    ReducerFailure::InvalidDurableState(DurableInvariant::Retirement)
                })?)
                .ok_or(ReducerFailure::CounterExhausted)?;
        barriers += usize::from(descriptor.server_wide_barrier());
        for pane in descriptor.panes() {
            if pane.pane().local() > value.high_water.pane
                || pane.tab().local() > value.high_water.tab
                || pane.workspace().local() > value.high_water.workspace
                || pane
                    .worktree()
                    .is_some_and(|id| id.local() > value.high_water.worktree)
                || value.organization.retained.as_ref().is_some_and(|facts| {
                    facts.pane_generations().iter().any(|mark| {
                        mark.pane() == pane.pane()
                            && mark.session() < pane.invalidation_generation()
                    })
                })
            {
                return invalid(DurableInvariant::Retirement);
            }
        }
    }
    for descriptor in &value.native_retirements {
        if descriptor.batch().server() != value.organization.server
            || !batches.insert(descriptor.batch())
            || descriptor.acceptance_revision() > value.organization.revision
            || descriptor.panes().iter().any(|v| {
                v.pane().local() > value.high_water.pane
                    || v.tab().local() > value.high_water.tab
                    || v.workspace().local() > value.high_water.workspace
                    || v.worktree()
                        .is_some_and(|id| id.local() > value.high_water.worktree)
            })
        {
            return invalid(DurableInvariant::Retirement);
        }
        let charge = descriptor
            .panes()
            .len()
            .checked_mul(112)
            .and_then(|n| {
                descriptor
                    .sessions()
                    .len()
                    .checked_mul(64)
                    .and_then(|s| n.checked_add(s))
            })
            .and_then(|n| n.checked_add(320))
            .ok_or(ReducerFailure::CounterExhausted)?;
        bytes = bytes
            .checked_add(charge)
            .ok_or(ReducerFailure::CounterExhausted)?;
    }
    if bytes > limits.retirement_bytes || barriers > 1 {
        return invalid(DurableInvariant::Retirement);
    }
    if value.git_allocations.len() > limits.retirement_batches {
        return invalid(DurableInvariant::Reservation);
    }
    let mut allocation_bytes = 0usize;
    let mut reserved = BTreeSet::new();
    let mut reserved_repositories = BTreeSet::new();
    let mut reserved_worktrees = BTreeSet::new();
    // Creation ordinals share one allocation domain across hierarchy records and
    // pending fallbacks. An absent reserved identity does not exempt its ordinal.
    let mut reserved_ordinals: BTreeSet<_> = value
        .organization
        .workspaces
        .iter()
        .map(|v| v.creation_ordinal)
        .chain(value.organization.tabs.iter().map(|v| v.creation_ordinal))
        .chain(value.organization.panes.iter().map(|v| v.creation_ordinal))
        .collect();
    let mut reserved_tabs = BTreeSet::new();
    let mut reserved_panes = BTreeSet::new();
    for reservation in &value.git_allocations {
        let parts = reservation.parts();
        if parts.fallbacks.len() > limits.workspaces {
            return invalid(DurableInvariant::Reservation);
        }
        allocation_bytes = allocation_bytes
            .checked_add(320)
            .ok_or(ReducerFailure::CounterExhausted)?;
        if parts
            .repository
            .is_some_and(|id| !reserved_repositories.insert(id))
            || parts
                .worktree
                .is_some_and(|id| !reserved_worktrees.insert(id))
        {
            return invalid(DurableInvariant::Reservation);
        }
        if !binding_fits(parts.binding, value.high_water, value.organization.server)
            || !value.high_water.dominates(parts.resulting_high_water)
            || !reserved.insert(parts.binding.id)
            || parts
                .repository
                .is_some_and(|id| value.organization.repositories.iter().any(|v| v.id == id))
            || parts
                .worktree
                .is_some_and(|id| value.organization.worktrees.iter().any(|v| v.id == id))
        {
            return invalid(DurableInvariant::Reservation);
        }
        for fallback in &parts.fallbacks {
            allocation_bytes = allocation_bytes
                .checked_add(160 + fallback.plan.directory.bytes().len())
                .ok_or(ReducerFailure::CounterExhausted)?;
            if allocation_bytes > limits.value_bytes {
                return invalid(DurableInvariant::Reservation);
            }
            if !reserved_ordinals.insert(fallback.tab_creation_ordinal)
                || !reserved_ordinals.insert(fallback.pane_creation_ordinal)
            {
                return invalid(DurableInvariant::Reservation);
            }
            if !reserved_tabs.insert(fallback.tab)
                || !reserved_panes.insert(fallback.pane)
                || value.organization.tabs.iter().any(|v| v.id == fallback.tab)
                || value
                    .organization
                    .panes
                    .iter()
                    .any(|v| v.id == fallback.pane)
            {
                return invalid(DurableInvariant::Reservation);
            }
        }
    }
    Ok(())
}

pub(crate) fn binding_fits(
    binding: OperationBinding,
    marks: IdentityHighWater,
    server: ServerId,
) -> bool {
    let stream = binding.id.stream();
    stream.server() == server
        && stream.key().local() <= marks.operation_stream
        && stream.client().local() <= marks.client
        && stream.pairing().local() <= marks.pairing
}

pub(crate) fn check_limits(
    org: &OrganizationState,
    limits: OrganizationLimits,
) -> Result<(), ReducerFailure> {
    if org.workspaces.len() > limits.workspaces
        || org.tabs.len() > limits.tabs
        || org.panes.len() > limits.panes
        || org.worktrees.len() > limits.worktrees
        || org.repositories.len() > limits.repositories
        || org.settings.worktree_location.value.as_str().len() > limits.template_bytes
    {
        return Err(ReducerFailure::QuotaExceeded);
    }
    let mut tabs = BTreeMap::new();
    let mut panes = BTreeMap::new();
    for tab in &org.tabs {
        *tabs.entry(tab.workspace).or_insert(0usize) += 1;
    }
    for pane in &org.panes {
        *panes.entry(pane.tab).or_insert(0usize) += 1;
    }
    if tabs.values().any(|n| *n > limits.tabs_per_workspace)
        || panes.values().any(|n| *n > limits.panes_per_tab)
    {
        return Err(ReducerFailure::QuotaExceeded);
    }
    let facts = org
        .retained
        .as_ref()
        .ok_or(ReducerFailure::InvalidDurableState(
            DurableInvariant::RetainedFacts,
        ))?;
    let mut bytes = facts
        .value_bytes()
        .map_err(|_| ReducerFailure::QuotaExceeded)?;
    let mut charge = |n: usize| -> Result<(), ReducerFailure> {
        bytes = bytes.checked_add(n).ok_or(ReducerFailure::QuotaExceeded)?;
        if bytes > limits.value_bytes {
            return Err(ReducerFailure::QuotaExceeded);
        }
        Ok(())
    };
    charge(org.settings.worktree_location.value.as_str().len())?;
    for v in &org.workspaces {
        if v.name.value.display.as_str().len() > limits.name_bytes {
            return Err(ReducerFailure::QuotaExceeded);
        }
        charge(128 + v.name.value.display.as_str().len())?;
        if let Some(dir) = &v.pinned_directory.value {
            check_directory(dir, limits)?;
            charge(dir.bytes().len())?;
        }
    }
    charge(
        org.tabs
            .len()
            .checked_mul(128)
            .ok_or(ReducerFailure::QuotaExceeded)?,
    )?;
    for v in &org.panes {
        check_directory(&v.starting_directory, limits)?;
        charge(160 + v.starting_directory.bytes().len())?;
        if let Some(dir) = &v.confirmed_restoration_directory.value {
            check_directory(dir, limits)?;
            charge(dir.bytes().len())?;
        }
    }
    for v in &org.repositories {
        check_directory(&v.directory, limits)?;
        charge(48 + v.directory.bytes().len())?;
    }
    for v in &org.worktrees {
        check_directory(&v.directory, limits)?;
        charge(80 + v.directory.bytes().len())?;
    }
    if facts.workspace_names().iter().any(|v| {
        v.automatic_base().as_str().len() > limits.name_bytes
            || v.assigned_automatic().as_str().len() > limits.name_bytes
    }) {
        return Err(ReducerFailure::QuotaExceeded);
    }
    Ok(())
}
fn check_directory(
    dir: &ServerDirectory,
    limits: OrganizationLimits,
) -> Result<(), ReducerFailure> {
    if dir.bytes().len() > limits.directory_bytes {
        Err(ReducerFailure::QuotaExceeded)
    } else {
        Ok(())
    }
}

#[derive(Clone)]
pub(crate) struct ReducerState {
    pub(crate) snapshot: OrganizationSnapshot,
    pub(crate) high_water: IdentityHighWater,
    pub(crate) server_home: ServerDirectory,
    pub(crate) store_revision: StoreRevision,
    pub(crate) limits: OrganizationLimits,
    pub(crate) transition: u64,
    // Private allocation identity plus the transition counter covers all staged
    // comparison facts, including non-published registry/observation changes.
    pub(crate) incarnation: std::sync::Arc<()>,
    pub(crate) suspended: bool,
    pub(crate) stopped: bool,
    pub(crate) observations: BTreeMap<PaneId, PaneObservations>,
    pub(crate) assessments: BTreeMap<OperationId, AssessmentRecord>,
    pub(crate) startup_reservations: BTreeMap<PaneId, StartupReservation>,
    pub(crate) runtime_cleanups: BTreeMap<PaneId, super::lifecycle::RuntimeCleanupOwner>,
    pub(crate) retirement_sequence: u64,
    pub(crate) retirements: BTreeMap<RetirementBatchId, RetirementDescriptor>,
    pub(crate) native_retirements: BTreeMap<RetirementBatchId, NativeRetirementDescriptor>,
    pub(crate) git_allocations: BTreeMap<OperationId, GitAllocationReservation>,
}
#[derive(Clone, Debug, Default)]
pub(crate) struct PaneObservations {
    pub(crate) display: Option<DisplayDirectoryObservation>,
    pub(crate) confirmed_watermark: Option<RuntimeObservationSequence>,
    pub(crate) close_work: Option<CloseWorkObservation>,
}
impl ReducerState {
    pub(crate) fn recover(
        retained: RetainedOrganization,
        store_revision: StoreRevision,
        epoch: ServerEpoch,
        limits: OrganizationLimits,
    ) -> Result<Self, ReducerFailure> {
        retained.validate(limits)?;
        if epoch.get() == 0
            || retained
                .retirements
                .iter()
                .any(|v| v.batch().epoch() >= epoch)
            || retained
                .native_retirements
                .iter()
                .any(|v| v.batch().epoch() >= epoch)
        {
            return Err(ReducerFailure::InvalidFacts);
        }
        let facts =
            retained
                .organization
                .retained
                .as_ref()
                .ok_or(ReducerFailure::InvalidDurableState(
                    DurableInvariant::RetainedFacts,
                ))?;
        let runtime = facts
            .pane_generations()
            .iter()
            .map(|v| PaneRuntimeRecord {
                pane: v.pane(),
                session_generation: v.session(),
                phase: SessionPhase::Ended {
                    reason: EndReason::ServerRestart,
                },
                control_generation: v.control(),
                control: ControlRecord::Unclaimed,
            })
            .collect();
        let mut organization = retained.organization;
        sort_organization(&mut organization);
        let snapshot = OrganizationSnapshot::new(epoch, organization, runtime)
            .map_err(|_| ReducerFailure::InvalidDurableState(DurableInvariant::Graph))?;
        Ok(Self {
            snapshot,
            high_water: retained.high_water,
            server_home: retained.server_home,
            store_revision,
            limits,
            assessments: BTreeMap::new(),
            startup_reservations: BTreeMap::new(),
            runtime_cleanups: BTreeMap::new(),
            retirement_sequence: 0,
            transition: 0,
            incarnation: std::sync::Arc::new(()),
            suspended: false,
            stopped: false,
            observations: BTreeMap::new(),
            retirements: retained
                .retirements
                .into_iter()
                .map(|v| (v.batch(), v))
                .collect(),
            native_retirements: retained
                .native_retirements
                .into_iter()
                .map(|v| (v.batch(), v))
                .collect(),
            git_allocations: retained
                .git_allocations
                .into_iter()
                .map(|v| (v.binding().id, v))
                .collect(),
        })
    }
    pub(crate) fn organization(&self) -> &OrganizationState {
        self.snapshot.state()
    }
    pub(crate) fn epoch(&self) -> ServerEpoch {
        self.snapshot.epoch()
    }
    pub(crate) fn retained(&self) -> RetainedOrganization {
        RetainedOrganization {
            organization: self.organization().clone(),
            high_water: self.high_water,
            server_home: self.server_home.clone(),
            retirements: self.retirements.values().cloned().collect(),
            native_retirements: self.native_retirements.values().cloned().collect(),
            git_allocations: self.git_allocations.values().cloned().collect(),
        }
    }
    pub(crate) fn check_admission(&self) -> Result<(), ReducerFailure> {
        if self.suspended {
            return Err(ReducerFailure::RecoveryRequired);
        }
        if self.assessments.len() > self.limits.assessments
            || self.startup_reservations.len() > self.organization().panes.len()
            || self.runtime_cleanups.len() > MAX_RETIREMENT_SESSIONS
        {
            return Err(ReducerFailure::InvalidFacts);
        }
        Ok(())
    }
    pub(crate) fn next_retirement_batch(&self) -> Result<RetirementBatchId, ReducerFailure> {
        let next = self
            .retirement_sequence
            .checked_add(1)
            .ok_or(ReducerFailure::CounterExhausted)?;
        RetirementBatchId::new(self.organization().server, self.epoch(), next)
            .map_err(|_| ReducerFailure::InvalidFacts)
    }
    pub(crate) fn creation_barrier(&self) -> bool {
        self.stopped
            || !self.native_retirements.is_empty()
            || self
                .retirements
                .values()
                .any(RetirementDescriptor::server_wide_barrier)
    }
    pub(crate) fn check_principal(&self, context: &ReductionContext) -> Result<(), ReducerFailure> {
        if context.principal.pairing != context.operation.stream().pairing()
            || context.principal.client != context.operation.stream().client()
            || !binding_fits(
                context.binding(),
                self.high_water,
                self.organization().server,
            )
        {
            return Err(ReducerFailure::WrongServer);
        }
        Ok(())
    }
    pub(crate) fn runtime(&self, pane: PaneId) -> Result<PaneRuntimeRecord, ReducerFailure> {
        self.snapshot
            .runtime()
            .iter()
            .find(|v| v.pane == pane)
            .copied()
            .ok_or(ReducerFailure::TargetNotFound)
    }
    pub(crate) fn replace_organization(
        &mut self,
        mut org: OrganizationState,
        mut runtime: Vec<PaneRuntimeRecord>,
    ) -> Result<(), ReducerFailure> {
        sort_organization(&mut org);
        runtime.sort_by_key(|v| v.pane);
        check_limits(&org, self.limits)?;
        self.snapshot = OrganizationSnapshot::new(self.epoch(), org, runtime)
            .map_err(|_| ReducerFailure::InvalidFacts)?;
        Ok(())
    }
    pub(crate) fn directory_basis(
        &self,
        command: &Command,
    ) -> Result<DirectoryBasis, ReducerFailure> {
        let org = self.organization();
        let (workspace, source, requested, worktree, member_tab) = match command {
            Command::CreateWorkspace { source, .. } => match source {
                WorkspaceCreationSource::ServerHome { .. } => (None, None, None, None, None),
                WorkspaceCreationSource::Directory(dir) => {
                    (None, None, Some(dir.clone()), None, None)
                }
                WorkspaceCreationSource::Pane(pane) => (None, Some(*pane), None, None, None),
            },
            Command::CreateTab { workspace, source } => match source {
                TabCreationSource::Pane(pane) => {
                    let p = org
                        .panes
                        .iter()
                        .find(|v| v.id == *pane)
                        .ok_or(ReducerFailure::TargetNotFound)?;
                    let t = org
                        .tabs
                        .iter()
                        .find(|v| v.id == p.tab)
                        .ok_or(ReducerFailure::InvalidFacts)?;
                    (Some(*workspace), Some(*pane), None, t.worktree, Some(t.id))
                }
                TabCreationSource::Directory(dir) => {
                    (Some(*workspace), None, Some(dir.clone()), None, None)
                }
                TabCreationSource::Worktree(worktree) => {
                    (Some(*workspace), None, None, Some(*worktree), None)
                }
            },
            Command::CreatePane { tab, source, .. } => {
                let t = org
                    .tabs
                    .iter()
                    .find(|v| v.id == *tab)
                    .ok_or(ReducerFailure::TargetNotFound)?;
                (
                    Some(t.workspace),
                    Some(*source),
                    None,
                    t.worktree,
                    Some(t.id),
                )
            }
            Command::SetPinnedDirectory {
                workspace, value, ..
            } => (Some(*workspace), None, value.clone(), None, None),
            Command::RepairPaneDirectory { pane, value, .. } => {
                (None, Some(*pane), Some(value.clone()), None, None)
            }
            _ => return Err(ReducerFailure::WrongReducer),
        };
        let facts = org.retained.as_ref().ok_or(ReducerFailure::InvalidFacts)?;
        let pin = workspace
            .map(|id| {
                org.workspaces
                    .iter()
                    .find(|v| v.id == id)
                    .map(|v| v.pinned_directory.clone())
                    .ok_or(ReducerFailure::TargetNotFound)
            })
            .transpose()?;
        let source = source
            .map(|id| {
                let pane = org
                    .panes
                    .iter()
                    .find(|v| v.id == id)
                    .ok_or(ReducerFailure::TargetNotFound)?;
                let display = self
                    .observations
                    .get(&id)
                    .and_then(|v| v.display.as_ref())
                    .filter(|v| {
                        v.presentation() == DirectoryPresentation::OwningServer
                            && v.session().generation
                                == self
                                    .runtime(id)
                                    .map(|r| r.session_generation)
                                    .unwrap_or_default()
                    });
                Ok(SourceDirectoryBasis {
                    pane: id,
                    tab: pane.tab,
                    generation: self.runtime(id)?.session_generation,
                    directory_version: pane.confirmed_restoration_directory.version,
                    observation: display.map(|v| v.sequence()),
                    directory: display
                        .map(|v| v.directory())
                        .unwrap_or(pane.restoration_directory())
                        .clone(),
                })
            })
            .transpose()?;
        let worktree = worktree
            .map(|id| {
                let record = org
                    .worktrees
                    .iter()
                    .find(|v| v.id == id)
                    .cloned()
                    .ok_or(ReducerFailure::TargetNotFound)?;
                Ok(WorktreeDirectoryBasis {
                    inventory: *facts
                        .worktree_inventory()
                        .iter()
                        .find(|v| v.worktree() == id)
                        .ok_or(ReducerFailure::InvalidFacts)?,
                    repository_version: *facts
                        .repository_inventory_versions()
                        .iter()
                        .find(|v| v.id() == record.repository)
                        .ok_or(ReducerFailure::InvalidFacts)?,
                    membership_version: member_tab.and_then(|tab| {
                        facts
                            .tab_membership_versions()
                            .iter()
                            .find(|v| v.id() == tab)
                            .copied()
                    }),
                    worktree: record,
                })
            })
            .transpose()?;
        Ok(DirectoryBasis {
            workspace,
            pin,
            source,
            worktree,
            requested,
        })
    }
}

pub(crate) fn sort_organization(org: &mut OrganizationState) {
    org.workspaces.sort_by_key(|v| v.id);
    org.tabs.sort_by_key(|v| v.id);
    org.panes.sort_by_key(|v| v.id);
    org.repositories.sort_by_key(|v| v.id);
    org.worktrees.sort_by_key(|v| v.id);
}

pub(crate) struct ReductionPatch {
    pub(crate) candidate: ReducerState,
    pub(crate) effects: Vec<OrganizationEffect>,
    pub(crate) resolution: Resolution,
    pub(crate) operations: Vec<crate::storage::OperationWrite>,
    pub(crate) accepted_creations: Vec<(TabId, PaneId, CreationOrdinal, PaneCreation)>,
}
impl ReductionPatch {
    pub(crate) fn unchanged(state: &ReducerState, outcome: Outcome) -> Self {
        Self {
            candidate: state.clone(),
            effects: Vec::new(),
            resolution: Resolution::Completed(outcome),
            operations: Vec::new(),
            accepted_creations: Vec::new(),
        }
    }
    pub(crate) fn rejected(state: &ReducerState, failure: OperationFailure) -> Self {
        Self::unchanged(state, Outcome::Failed(failure))
    }
}
