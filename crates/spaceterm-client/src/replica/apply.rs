use crate::failure::{ClientFailure, invalid_peer};
use spaceterm_core::{
    organization::{
        OrganizationDelta, OrganizationSnapshot, OrganizationState, OrganizationWrite,
        PaneRuntimeRecord,
    },
    organization_retained::{OrganizationRetainedFacts, RetainedOrganizationWrite},
};

fn put<T, K: Copy + Eq>(values: &mut Vec<T>, value: T, key: impl Fn(&T) -> K) {
    if let Some(existing) = values
        .iter_mut()
        .find(|existing| key(existing) == key(&value))
    {
        *existing = value;
    } else {
        values.push(value);
    }
}
fn delete<T, K: Copy + Eq>(values: &mut Vec<T>, id: K, key: impl Fn(&T) -> K) {
    values.retain(|value| key(value) != id);
}

pub(super) fn apply(
    base: &OrganizationSnapshot,
    delta: &OrganizationDelta,
) -> Result<OrganizationSnapshot, ClientFailure> {
    let mut state = base.state().clone();
    let mut runtime = base.runtime().to_vec();
    let mut retained = state
        .retained
        .take()
        .ok_or_else(invalid_peer)?
        .parts()
        .clone();
    for write in delta.writes() {
        write
            .validate_scope(state.server)
            .map_err(|_| invalid_peer())?;
        match write {
            OrganizationWrite::PutWorkspace(v) => put(&mut state.workspaces, v.clone(), |v| v.id),
            OrganizationWrite::DeleteWorkspace(id) => delete(&mut state.workspaces, *id, |v| v.id),
            OrganizationWrite::PutTab(v) => put(&mut state.tabs, v.clone(), |v| v.id),
            OrganizationWrite::DeleteTab(id) => delete(&mut state.tabs, *id, |v| v.id),
            OrganizationWrite::PutPane(v) => put(&mut state.panes, v.clone(), |v| v.id),
            OrganizationWrite::DeletePane(id) => {
                delete(&mut state.panes, *id, |v| v.id);
                delete(&mut runtime, *id, |v| v.pane);
            }
            OrganizationWrite::PutRepository(v) => {
                put(&mut state.repositories, v.clone(), |v| v.id)
            }
            OrganizationWrite::DeleteRepository(id) => {
                delete(&mut state.repositories, *id, |v| v.id)
            }
            OrganizationWrite::PutWorktree(v) => put(&mut state.worktrees, v.clone(), |v| v.id),
            OrganizationWrite::DeleteWorktree(id) => delete(&mut state.worktrees, *id, |v| v.id),
            OrganizationWrite::SetServerSettings(v) => state.settings = v.clone(),
            OrganizationWrite::Retained(write) => match write {
                RetainedOrganizationWrite::PutWorkspaceNameBasis(v) => {
                    put(&mut retained.workspace_names, v.clone(), |v| v.workspace())
                }
                RetainedOrganizationWrite::DeleteWorkspaceNameBasis(id) => {
                    delete(&mut retained.workspace_names, *id, |v| v.workspace())
                }
                RetainedOrganizationWrite::PutCreationDefault(v) => {
                    put(&mut retained.creation_defaults, v.clone(), |v| v.tab())
                }
                RetainedOrganizationWrite::DeleteCreationDefault(id) => {
                    delete(&mut retained.creation_defaults, *id, |v| v.tab())
                }
                RetainedOrganizationWrite::SetPaneGenerationHighWater(v) => {
                    put(&mut retained.pane_generations, *v, |v| v.pane())
                }
                RetainedOrganizationWrite::DeletePaneGenerationHighWater(id) => {
                    delete(&mut retained.pane_generations, *id, |v| v.pane())
                }
                RetainedOrganizationWrite::SetTabMembershipVersion(v) => {
                    put(&mut retained.tab_membership_versions, *v, |v| v.id())
                }
                RetainedOrganizationWrite::DeleteTabMembershipVersion(id) => {
                    delete(&mut retained.tab_membership_versions, *id, |v| v.id())
                }
                RetainedOrganizationWrite::SetRepositoryInventoryVersion(v) => {
                    put(&mut retained.repository_inventory_versions, *v, |v| v.id())
                }
                RetainedOrganizationWrite::DeleteRepositoryInventoryVersion(id) => {
                    delete(&mut retained.repository_inventory_versions, *id, |v| v.id())
                }
                RetainedOrganizationWrite::SetWorktreeInventoryFacts(v) => {
                    put(&mut retained.worktree_inventory, *v, |v| v.worktree())
                }
                RetainedOrganizationWrite::DeleteWorktreeInventoryFacts(id) => {
                    delete(&mut retained.worktree_inventory, *id, |v| v.worktree())
                }
                RetainedOrganizationWrite::SetRemoteAccess(v) => retained.remote_access = v.clone(),
            },
        }
    }
    for record in delta.runtime() {
        put(&mut runtime, *record, |v| v.pane);
    }
    retained.workspace_names.sort_by_key(|v| v.workspace());
    retained.creation_defaults.sort_by_key(|v| v.tab());
    retained.pane_generations.sort_by_key(|v| v.pane());
    retained.tab_membership_versions.sort_by_key(|v| v.id());
    retained
        .repository_inventory_versions
        .sort_by_key(|v| v.id());
    retained.worktree_inventory.sort_by_key(|v| v.worktree());
    state.retained = Some(OrganizationRetainedFacts::new(retained).map_err(|_| invalid_peer())?);
    state.revision = delta.revision();
    OrganizationSnapshot::new(delta.epoch(), state, runtime).map_err(|_| invalid_peer())
}

pub(super) fn merge(
    state: &mut OrganizationState,
    runtime: &mut Vec<PaneRuntimeRecord>,
    next: OrganizationState,
    next_runtime: Vec<PaneRuntimeRecord>,
) -> Result<(), ClientFailure> {
    if state.server != next.server
        || state.revision != next.revision
        || state.settings != next.settings
    {
        return Err(invalid_peer());
    }
    let mut retained = state
        .retained
        .take()
        .ok_or_else(invalid_peer)?
        .parts()
        .clone();
    let new = next.retained.ok_or_else(invalid_peer)?;
    let mut new = new.parts().clone();
    if retained.remote_access != new.remote_access {
        return Err(invalid_peer());
    }
    retained.workspace_names.append(&mut new.workspace_names);
    retained
        .creation_defaults
        .append(&mut new.creation_defaults);
    retained.pane_generations.append(&mut new.pane_generations);
    retained
        .tab_membership_versions
        .append(&mut new.tab_membership_versions);
    retained
        .repository_inventory_versions
        .append(&mut new.repository_inventory_versions);
    retained
        .worktree_inventory
        .append(&mut new.worktree_inventory);
    retained.workspace_names.sort_by_key(|v| v.workspace());
    retained.creation_defaults.sort_by_key(|v| v.tab());
    retained.pane_generations.sort_by_key(|v| v.pane());
    retained.tab_membership_versions.sort_by_key(|v| v.id());
    retained
        .repository_inventory_versions
        .sort_by_key(|v| v.id());
    retained.worktree_inventory.sort_by_key(|v| v.worktree());
    state.retained = Some(OrganizationRetainedFacts::new(retained).map_err(|_| invalid_peer())?);
    state.workspaces.extend(next.workspaces);
    state.tabs.extend(next.tabs);
    state.panes.extend(next.panes);
    state.repositories.extend(next.repositories);
    state.worktrees.extend(next.worktrees);
    runtime.extend(next_runtime);
    Ok(())
}

pub(super) fn validate_fields(snapshot: &OrganizationSnapshot) -> Result<(), ClientFailure> {
    let state = snapshot.state();
    if snapshot.epoch().get() == 0
        || state.server.bytes() == [0; 16]
        || state.settings.worktree_location.version.get() == 0
        || state
            .retained
            .as_ref()
            .is_none_or(|r| r.remote_access().version.get() == 0)
        || state.workspaces.iter().any(|v| {
            v.name.version.get() == 0
                || v.pinned_directory.version.get() == 0
                || v.creation_ordinal.get() == 0
        })
        || state.tabs.iter().any(|v| v.creation_ordinal.get() == 0)
        || state.panes.iter().any(|v| {
            v.creation_ordinal.get() == 0 || v.confirmed_restoration_directory.version.get() == 0
        })
    {
        return Err(invalid_peer());
    }
    if snapshot.runtime().iter().any(|record| {
        matches!(record.phase, spaceterm_core::organization::SessionPhase::Running { generation } | spaceterm_core::organization::SessionPhase::Starting { generation } if generation.get() == 0)
            || matches!(record.control, spaceterm_core::organization::ControlRecord::Held { generation, .. } if generation.get() == 0)
    }) { return Err(invalid_peer()); }
    // Reconstructing checks runtime coverage, grants, normalized topology and Roots.
    state.validate().map_err(|_| invalid_peer())
}

pub(super) fn validate_transition(
    previous: &OrganizationSnapshot,
    current: &OrganizationSnapshot,
) -> Result<(), ClientFailure> {
    use spaceterm_core::organization::{SessionPhase, Versioned};
    use std::collections::{BTreeMap, BTreeSet};
    fn version<T: PartialEq>(
        previous: &Versioned<T>,
        current: &Versioned<T>,
    ) -> Result<(), ClientFailure> {
        if current.version < previous.version
            || (current.value != previous.value && current.version == previous.version)
        {
            Err(invalid_peer())
        } else {
            Ok(())
        }
    }
    if current.epoch() < previous.epoch() {
        return Err(invalid_peer());
    }
    if current.epoch() == previous.epoch()
        && current.state().revision == previous.state().revision
        && !equivalent(current, previous)
    {
        return Err(invalid_peer());
    }
    let before = previous.state();
    let after = current.state();
    version(
        &before.settings.worktree_location,
        &after.settings.worktree_location,
    )?;
    let old_workspaces: BTreeMap<_, _> = before.workspaces.iter().map(|v| (v.id, v)).collect();
    let old_tabs: BTreeMap<_, _> = before.tabs.iter().map(|v| (v.id, v)).collect();
    let old_panes: BTreeMap<_, _> = before.panes.iter().map(|v| (v.id, v)).collect();
    let surviving_tabs: BTreeSet<_> = after.tabs.iter().map(|v| v.id).collect();
    let surviving_panes: BTreeSet<_> = after.panes.iter().map(|v| v.id).collect();
    for record in &after.workspaces {
        if let Some(old) = old_workspaces.get(&record.id) {
            if record.creation_ordinal != old.creation_ordinal
                || (record.root_tab != old.root_tab && surviving_tabs.contains(&old.root_tab))
            {
                return Err(invalid_peer());
            }
            version(&old.name, &record.name)?;
            version(&old.pinned_directory, &record.pinned_directory)?;
        }
    }
    for record in &after.tabs {
        if let Some(old) = old_tabs.get(&record.id)
            && (record.creation_ordinal != old.creation_ordinal
                || record.workspace != old.workspace
                || (record.root_pane != old.root_pane && surviving_panes.contains(&old.root_pane)))
        {
            return Err(invalid_peer());
        }
    }
    for record in &after.panes {
        if let Some(old) = old_panes.get(&record.id) {
            if record.creation_ordinal != old.creation_ordinal || record.tab != old.tab {
                return Err(invalid_peer());
            }
            version(
                &old.confirmed_restoration_directory,
                &record.confirmed_restoration_directory,
            )?;
        }
    }
    let old_runtime: BTreeMap<_, _> = previous.runtime().iter().map(|v| (v.pane, v)).collect();
    for record in current.runtime() {
        if let Some(old) = old_runtime.get(&record.pane) {
            if record.session_generation < old.session_generation
                || record.control_generation < old.control_generation
            {
                return Err(invalid_peer());
            }
            if current.epoch() == previous.epoch()
                && record.control != old.control
                && record.control_generation == old.control_generation
            {
                return Err(invalid_peer());
            }
            if !matches!(
                old.phase,
                SessionPhase::Starting { .. } | SessionPhase::Running { .. }
            ) && matches!(
                record.phase,
                SessionPhase::Running { .. } | SessionPhase::Starting { .. }
            ) && record.session_generation == old.session_generation
            {
                return Err(invalid_peer());
            }
        }
    }
    let old_retained = before.retained.as_ref().ok_or_else(invalid_peer)?;
    let new_retained = after.retained.as_ref().ok_or_else(invalid_peer)?;
    version(old_retained.remote_access(), new_retained.remote_access())?;
    validate_retained_versions(before, after)?;
    Ok(())
}

fn validate_retained_versions(
    before: &OrganizationState,
    after: &OrganizationState,
) -> Result<(), ClientFailure> {
    use spaceterm_core::authority::FieldVersion;
    use std::collections::BTreeMap;
    fn check(old: FieldVersion, new: FieldVersion, changed: bool) -> Result<(), ClientFailure> {
        if new < old || (changed && new == old) {
            Err(invalid_peer())
        } else {
            Ok(())
        }
    }
    let old = before.retained.as_ref().ok_or_else(invalid_peer)?;
    let new = after.retained.as_ref().ok_or_else(invalid_peer)?;
    let old_tabs: BTreeMap<_, _> = before.tabs.iter().map(|r| (r.id, r)).collect();
    let new_tabs: BTreeMap<_, _> = after.tabs.iter().map(|r| (r.id, r)).collect();
    let old_membership: BTreeMap<_, _> = old
        .tab_membership_versions()
        .iter()
        .map(|r| (r.id(), r.version()))
        .collect();
    for version in new.tab_membership_versions() {
        if let Some(previous) = old_membership.get(&version.id()) {
            let tab = new_tabs.get(&version.id()).ok_or_else(invalid_peer)?;
            let previous_tab = old_tabs.get(&version.id()).ok_or_else(invalid_peer)?;
            check(
                *previous,
                version.version(),
                previous_tab.worktree != tab.worktree,
            )?;
        }
    }
    let old_worktrees: BTreeMap<_, _> = before.worktrees.iter().map(|r| (r.id, r)).collect();
    let new_worktrees: BTreeMap<_, _> = after.worktrees.iter().map(|r| (r.id, r)).collect();
    let old_facts: BTreeMap<_, _> = old
        .worktree_inventory()
        .iter()
        .map(|r| (r.worktree(), r))
        .collect();
    let new_facts: BTreeMap<_, _> = new
        .worktree_inventory()
        .iter()
        .map(|r| (r.worktree(), r))
        .collect();
    for (id, facts) in &new_facts {
        if let Some(previous) = old_facts.get(id) {
            check(
                previous.version(),
                facts.version(),
                previous.listed() != facts.listed()
                    || old_worktrees.get(id) != new_worktrees.get(id),
            )?;
        }
    }
    let old_repositories: BTreeMap<_, _> = before.repositories.iter().map(|r| (r.id, r)).collect();
    let new_repositories: BTreeMap<_, _> = after.repositories.iter().map(|r| (r.id, r)).collect();
    fn inventory<'a>(
        worktrees: &BTreeMap<
            spaceterm_core::identity::WorktreeId,
            &'a spaceterm_core::organization::WorktreeRecord,
        >,
        facts: &BTreeMap<
            spaceterm_core::identity::WorktreeId,
            &spaceterm_core::organization_retained::WorktreeInventoryFacts,
        >,
    ) -> BTreeMap<
        spaceterm_core::identity::RepositoryId,
        BTreeMap<
            spaceterm_core::identity::WorktreeId,
            (
                &'a spaceterm_core::organization::WorktreeRecord,
                Option<bool>,
            ),
        >,
    > {
        let mut inventory = BTreeMap::<_, BTreeMap<_, _>>::new();
        for (id, record) in worktrees {
            inventory
                .entry(record.repository)
                .or_default()
                .insert(*id, (*record, facts.get(id).map(|r| r.listed())));
        }
        inventory
    }
    let old_contents = inventory(&old_worktrees, &old_facts);
    let new_contents = inventory(&new_worktrees, &new_facts);
    let old_inventory: BTreeMap<_, _> = old
        .repository_inventory_versions()
        .iter()
        .map(|r| (r.id(), r.version()))
        .collect();
    for version in new.repository_inventory_versions() {
        if let Some(previous) = old_inventory.get(&version.id()) {
            let repository = new_repositories
                .get(&version.id())
                .ok_or_else(invalid_peer)?;
            let previous_repository = old_repositories
                .get(&version.id())
                .ok_or_else(invalid_peer)?;
            check(
                *previous,
                version.version(),
                previous_repository != repository
                    || old_contents.get(&version.id()) != new_contents.get(&version.id()),
            )?;
        }
    }
    Ok(())
}

// Inventory enumeration is not organization order. Retained creation defaults
// still compare their explicit replay instructions in the meaningful order.
fn equivalent(left: &OrganizationSnapshot, right: &OrganizationSnapshot) -> bool {
    use std::collections::BTreeMap;
    fn records<T: PartialEq, K: Ord>(left: &[T], right: &[T], key: impl Fn(&T) -> K) -> bool {
        left.iter().map(|r| (key(r), r)).collect::<BTreeMap<_, _>>()
            == right
                .iter()
                .map(|r| (key(r), r))
                .collect::<BTreeMap<_, _>>()
    }
    let a = left.state();
    let b = right.state();
    a.server == b.server
        && a.revision == b.revision
        && a.settings == b.settings
        && a.retained == b.retained
        && records(&a.workspaces, &b.workspaces, |r| r.id)
        && records(&a.tabs, &b.tabs, |r| r.id)
        && records(&a.panes, &b.panes, |r| r.id)
        && records(&a.repositories, &b.repositories, |r| r.id)
        && records(&a.worktrees, &b.worktrees, |r| r.id)
        && records(left.runtime(), right.runtime(), |r| r.pane)
}
