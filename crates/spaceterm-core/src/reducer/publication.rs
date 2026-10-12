use super::PreparedReduction;
use super::contracts::*;
use super::state::{ReducerState, ReductionPatch};
use crate::operation_ledger_contracts::{OperationReductionBasis, OperationReductionBasisParts};
use crate::operations::*;
use crate::organization::*;
use crate::organization_retained::*;
use crate::storage::OperationWrite;

pub(crate) fn prepare(
    state: &ReducerState,
    mut patch: ReductionPatch,
    binding: Option<OperationBinding>,
) -> Result<PreparedReduction, ReducerFailure> {
    state
        .store_revision
        .checked_successor()
        .ok_or(ReducerFailure::CounterExhausted)?;
    state
        .transition
        .checked_add(1)
        .ok_or(ReducerFailure::CounterExhausted)?;
    let writes = diff(state.organization(), patch.candidate.organization())?;
    let runtime_changed = state.snapshot.runtime() != patch.candidate.snapshot.runtime();
    let base = state.organization().revision;
    let revision = if writes.is_empty() && !runtime_changed {
        base
    } else {
        base.checked_successor()
            .ok_or(ReducerFailure::CounterExhausted)?
    };
    let mut org = patch.candidate.organization().clone();
    org.revision = revision;
    let runtime = patch.candidate.snapshot.runtime().to_vec();
    patch.candidate.replace_organization(org, runtime)?;
    match patch.candidate.retained().validate(state.limits) {
        Ok(()) => {}
        Err(ReducerFailure::InvalidDurableState(DurableInvariant::Limits)) => {
            return prepare(
                state,
                ReductionPatch::rejected(state, OperationFailure::Capacity),
                binding,
            );
        }
        Err(error) => return Err(error),
    }
    if let Some(binding) = binding {
        let basis = OperationReductionBasis::new(OperationReductionBasisParts {
            binding,
            base,
            resulting: revision,
        })
        .map_err(|_| ReducerFailure::InvalidFacts)?;
        patch.operations.push(OperationWrite::ReductionBasis(basis));
    }
    let projection = OrganizationWriteSet {
        server: state.organization().server,
        expected_store: state.store_revision,
        base,
        revision,
        epoch: state.epoch(),
        expected_marks: state.high_water,
        resulting_marks: patch.candidate.high_water,
        organization: writes,
        operations: patch.operations,
    };
    Ok(PreparedReduction {
        basis: state.transition,
        incarnation: state.incarnation.clone(),
        expected_store: state.store_revision,
        base_revision: base,
        candidate: patch.candidate,
        writes: projection,
        effects: patch.effects,
        resolution: patch.resolution,
        accepted_creations: patch.accepted_creations,
    })
}

pub(crate) fn delta(
    state: &ReducerState,
    prepared: &PreparedReduction,
) -> Result<Option<OrganizationDelta>, ReducerFailure> {
    if prepared.base_revision == prepared.resulting_organization_revision() {
        return Ok(None);
    }
    let old_runtime: std::collections::BTreeMap<_, _> = state
        .snapshot
        .runtime()
        .iter()
        .map(|v| (v.pane, v))
        .collect();
    let runtime = prepared
        .candidate
        .snapshot
        .runtime()
        .iter()
        .filter(|v| old_runtime.get(&v.pane).copied() != Some(*v))
        .copied()
        .collect();
    let delta = OrganizationDelta::new(
        state.organization().server,
        state.epoch(),
        prepared.base_revision,
        prepared.resulting_organization_revision(),
        prepared.writes.organization.clone(),
        runtime,
    )
    .map_err(|_| ReducerFailure::InvalidFacts)?;
    let creations = prepared
        .accepted_creations
        .iter()
        .map(|(tab, pane, ordinal, creation)| {
            AcceptedPaneCreation::new(&delta, *tab, *pane, *ordinal, *creation)
        })
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| ReducerFailure::InvalidFacts)?;
    Ok(Some(
        delta
            .with_accepted_creations(creations)
            .map_err(|_| ReducerFailure::InvalidFacts)?,
    ))
}

fn records<T: Eq + Clone, I: Copy + Ord>(
    old: &[T],
    new: &[T],
    id: impl Fn(&T) -> I,
    delete: impl Fn(I) -> OrganizationWrite,
    put: impl Fn(T) -> OrganizationWrite,
    deletes: &mut Vec<OrganizationWrite>,
    puts: &mut Vec<OrganizationWrite>,
) {
    let old: std::collections::BTreeMap<_, _> = old.iter().map(|v| (id(v), v)).collect();
    let new: std::collections::BTreeMap<_, _> = new.iter().map(|v| (id(v), v)).collect();
    for key in old.keys() {
        if !new.contains_key(key) {
            deletes.push(delete(*key));
        }
    }
    for (key, value) in new {
        if old.get(&key).copied() != Some(value) {
            puts.push(put(value.clone()));
        }
    }
}

/// Deterministic complete logical changes. Publication transport owns pagination.
fn diff(
    old: &OrganizationState,
    new: &OrganizationState,
) -> Result<Vec<OrganizationWrite>, ReducerFailure> {
    let mut deletes = Vec::new();
    let mut puts = Vec::new();
    let mut workspace_puts = Vec::new();
    let mut tab_puts = Vec::new();
    let mut pane_puts = Vec::new();
    records(
        &old.panes,
        &new.panes,
        |v| v.id,
        OrganizationWrite::DeletePane,
        OrganizationWrite::PutPane,
        &mut deletes,
        &mut pane_puts,
    );
    records(
        &old.tabs,
        &new.tabs,
        |v| v.id,
        OrganizationWrite::DeleteTab,
        OrganizationWrite::PutTab,
        &mut deletes,
        &mut tab_puts,
    );
    records(
        &old.workspaces,
        &new.workspaces,
        |v| v.id,
        OrganizationWrite::DeleteWorkspace,
        OrganizationWrite::PutWorkspace,
        &mut deletes,
        &mut workspace_puts,
    );
    records(
        &old.worktrees,
        &new.worktrees,
        |v| v.id,
        OrganizationWrite::DeleteWorktree,
        OrganizationWrite::PutWorktree,
        &mut deletes,
        &mut puts,
    );
    records(
        &old.repositories,
        &new.repositories,
        |v| v.id,
        OrganizationWrite::DeleteRepository,
        OrganizationWrite::PutRepository,
        &mut deletes,
        &mut puts,
    );
    deletes.extend(workspace_puts);
    deletes.extend(tab_puts);
    deletes.extend(pane_puts);
    deletes.extend(puts);
    if old.settings != new.settings {
        deletes.push(OrganizationWrite::SetServerSettings(new.settings.clone()));
    }
    let old = old.retained.as_ref().ok_or(ReducerFailure::InvalidFacts)?;
    let new = new.retained.as_ref().ok_or(ReducerFailure::InvalidFacts)?;
    let mut retained_deletes = Vec::new();
    let mut retained_puts = Vec::new();
    macro_rules! retained {
        ($getter:ident, $id:expr, $delete:ident, $put:ident) => {
            records(
                old.$getter(),
                new.$getter(),
                $id,
                |id| OrganizationWrite::Retained(RetainedOrganizationWrite::$delete(id)),
                |v| OrganizationWrite::Retained(RetainedOrganizationWrite::$put(v)),
                &mut retained_deletes,
                &mut retained_puts,
            );
        };
    }
    retained!(
        pane_generations,
        |v| v.pane(),
        DeletePaneGenerationHighWater,
        SetPaneGenerationHighWater
    );
    retained!(
        creation_defaults,
        |v: &TabCreationDefault| v.tab(),
        DeleteCreationDefault,
        PutCreationDefault
    );
    retained!(
        tab_membership_versions,
        |v| v.id(),
        DeleteTabMembershipVersion,
        SetTabMembershipVersion
    );
    retained!(
        workspace_names,
        |v: &WorkspaceNameBasis| v.workspace(),
        DeleteWorkspaceNameBasis,
        PutWorkspaceNameBasis
    );
    retained!(
        worktree_inventory,
        |v| v.worktree(),
        DeleteWorktreeInventoryFacts,
        SetWorktreeInventoryFacts
    );
    retained!(
        repository_inventory_versions,
        |v| v.id(),
        DeleteRepositoryInventoryVersion,
        SetRepositoryInventoryVersion
    );
    deletes.extend(retained_deletes);
    deletes.extend(retained_puts);
    if old.remote_access() != new.remote_access() {
        deletes.push(OrganizationWrite::Retained(
            RetainedOrganizationWrite::SetRemoteAccess(new.remote_access().clone()),
        ));
    }
    Ok(deletes)
}
