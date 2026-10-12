use super::contracts::*;
use super::state::{ReducerState, ReductionPatch};
use crate::authority::*;
use crate::identity::*;
use crate::operations::*;
use crate::organization::*;
use crate::organization_retained::*;
use crate::retirement::RetirementCleanupState;
use crate::storage::OperationWrite;
use crate::worktrees::intent::*;

pub(crate) fn plan(
    state: &ReducerState,
    command: &Command,
    context: &ReductionContext,
) -> Result<ReductionPatch, ReducerFailure> {
    if state.creation_barrier() {
        return Ok(ReductionPatch::rejected(
            state,
            OperationFailure::BusyRetiring,
        ));
    }
    let org = state.organization();
    match command {
        Command::CreateTab { workspace, source } => {
            if !org.workspaces.iter().any(|v| v.id == *workspace) {
                return Ok(ReductionPatch::rejected(
                    state,
                    OperationFailure::TargetDeleted,
                ));
            }
            if let TabCreationSource::Pane(id) = source {
                let Some(pane) = org.panes.iter().find(|v| v.id == *id) else {
                    return Ok(ReductionPatch::rejected(
                        state,
                        OperationFailure::TargetDeleted,
                    ));
                };
                if !org
                    .tabs
                    .iter()
                    .any(|v| v.id == pane.tab && v.workspace == *workspace)
                {
                    return Ok(ReductionPatch::rejected(
                        state,
                        OperationFailure::WrongMembership,
                    ));
                }
            }
        }
        Command::CreatePane { tab, source, .. } => {
            if !org.tabs.iter().any(|v| v.id == *tab) {
                return Ok(ReductionPatch::rejected(
                    state,
                    OperationFailure::TargetDeleted,
                ));
            }
            let Some(pane) = org.panes.iter().find(|v| v.id == *source) else {
                return Ok(ReductionPatch::rejected(
                    state,
                    OperationFailure::TargetDeleted,
                ));
            };
            if pane.tab != *tab {
                return Ok(ReductionPatch::rejected(
                    state,
                    OperationFailure::WrongMembership,
                ));
            }
        }
        Command::CreateWorkspace {
            source: WorkspaceCreationSource::Pane(id),
            ..
        } if !org.panes.iter().any(|v| v.id == *id) => {
            return Ok(ReductionPatch::rejected(
                state,
                OperationFailure::TargetDeleted,
            ));
        }
        _ => {}
    }
    let basis = match state.directory_basis(command) {
        Ok(basis) => basis,
        Err(ReducerFailure::TargetNotFound) => {
            return Ok(ReductionPatch::rejected(
                state,
                OperationFailure::TargetDeleted,
            ));
        }
        Err(error) => return Err(error),
    };
    if state.retirements.values().any(|descriptor| {
        descriptor.panes().iter().any(|pane| basis.source.as_ref().is_some_and(|source| source.tab == pane.tab()))
            || matches!(descriptor.target(), AssessmentTarget::RemoveWorktree { worktree, .. } if basis.worktree.as_ref().is_some_and(|scope| scope.worktree.id == worktree))
    }) {
        return Ok(ReductionPatch::rejected(state, OperationFailure::BusyRetiring));
    }
    let directory = match select_directory(state, command, context) {
        Ok(value) => value,
        Err(failure) => return Ok(ReductionPatch::rejected(state, failure)),
    };
    let mut patch = ReductionPatch::unchanged(state, Outcome::Applied);
    let mut org = org.clone();
    let mut facts = org
        .retained
        .as_ref()
        .ok_or(ReducerFailure::InvalidFacts)?
        .parts()
        .clone();
    let mut runtime = state.snapshot.runtime().to_vec();
    let marks = &mut patch.candidate.high_water;
    let workspace = match command {
        Command::CreateWorkspace { .. } => {
            Some(allocate::<WorkspaceId>(org.server, &mut marks.workspace)?)
        }
        _ => None,
    };
    let tab = match command {
        Command::CreateWorkspace { .. } | Command::CreateTab { .. } => {
            allocate::<TabId>(org.server, &mut marks.tab)?
        }
        Command::CreatePane { tab, .. } => *tab,
        _ => return Err(ReducerFailure::WrongReducer),
    };
    let pane = allocate::<PaneId>(org.server, &mut marks.pane)?;
    let initial = FieldVersion::new(1);
    if let Some(workspace) = workspace {
        let base =
            super::naming::directory_name(&directory, &state.server_home, state.limits.name_bytes)?;
        let name = match command {
            Command::CreateWorkspace {
                name: NameDraft::Custom(value),
                ..
            } => {
                let text = value.as_str().trim();
                if text.len() > state.limits.name_bytes {
                    return Ok(ReductionPatch::rejected(
                        state,
                        OperationFailure::InvalidValue,
                    ));
                }
                if text.is_empty() {
                    WorkspaceName {
                        mode: NameMode::Automatic,
                        display: base.clone(),
                    }
                } else {
                    WorkspaceName {
                        mode: NameMode::Custom,
                        display: NameText::new(text.to_owned())
                            .map_err(|_| ReducerFailure::InvalidFacts)?,
                    }
                }
            }
            _ => WorkspaceName {
                mode: NameMode::Automatic,
                display: base.clone(),
            },
        };
        org.workspaces.push(WorkspaceRecord {
            id: workspace,
            name: Versioned {
                value: name,
                version: initial,
            },
            pinned_directory: Versioned {
                value: match command {
                    Command::CreateWorkspace {
                        source: WorkspaceCreationSource::Directory(dir),
                        ..
                    } => Some(dir.clone()),
                    _ => None,
                },
                version: initial,
            },
            root_tab: tab,
            creation_ordinal: ordinal(marks)?,
        });
        facts
            .workspace_names
            .push(WorkspaceNameBasis::new(workspace, base.clone(), base));
    }
    if !matches!(command, Command::CreatePane { .. }) {
        let owner = workspace
            .or(match command {
                Command::CreateTab { workspace, .. } => Some(*workspace),
                _ => None,
            })
            .ok_or(ReducerFailure::InvalidFacts)?;
        let worktree = context
            .facts
            .directories
            .as_ref()
            .and_then(|v| v.basis.worktree.as_ref())
            .map(|v| v.worktree.id);
        org.tabs.push(TabRecord {
            id: tab,
            workspace: owner,
            worktree,
            root_pane: pane,
            creation_ordinal: ordinal(marks)?,
        });
        facts
            .tab_membership_versions
            .push(TabMembershipVersion::new(tab, initial));
        facts.creation_defaults.push(
            TabCreationDefault::new(tab, vec![pane]).map_err(|_| ReducerFailure::InvalidFacts)?,
        );
    }
    let creation = match command {
        Command::CreatePane {
            source, axis, edge, ..
        } => PaneCreation::SplitOf {
            source: *source,
            axis: *axis,
            edge: *edge,
        },
        _ => PaneCreation::Initial,
    };
    let pane_ordinal = ordinal(marks)?;
    org.panes.push(PaneRecord {
        id: pane,
        tab,
        creation_ordinal: pane_ordinal,
        creation,
        starting_directory: directory,
        confirmed_restoration_directory: Versioned {
            value: None,
            version: initial,
        },
    });
    if matches!(command, Command::CreatePane { .. }) {
        let default = facts
            .creation_defaults
            .iter_mut()
            .find(|v| v.tab() == tab)
            .ok_or(ReducerFailure::InvalidFacts)?;
        let mut order = default.replay_order().to_vec();
        order.push(pane);
        *default = TabCreationDefault::new(tab, order).map_err(|_| ReducerFailure::InvalidFacts)?;
        patch
            .accepted_creations
            .push((tab, pane, pane_ordinal, creation));
    }
    let generation = ControlGeneration::new(1);
    let grant = ControlGrant::new(pane, context.principal.client, generation)
        .map_err(|_| ReducerFailure::InvalidFacts)?;
    facts.pane_generations.push(PaneGenerationHighWater::new(
        pane,
        SessionGeneration::new(0),
        generation,
    ));
    runtime.push(PaneRuntimeRecord {
        pane,
        session_generation: SessionGeneration::new(0),
        phase: SessionPhase::NotStarted,
        control_generation: generation,
        control: ControlRecord::Held {
            client: context.principal.client,
            generation,
        },
    });
    install_facts(&mut org, facts)?;
    super::naming::refresh_root_names(state, &mut org)?;
    // Initial assignment is one creation, not a second name-field transition.
    if let Some(id) = workspace {
        org.workspaces
            .iter_mut()
            .find(|v| v.id == id)
            .ok_or(ReducerFailure::InvalidFacts)?
            .name
            .version = initial;
    }
    let entities = match workspace {
        Some(workspace) => CreatedEntities::Workspace {
            workspace,
            tab,
            pane,
        },
        None if matches!(command, Command::CreateTab { .. }) => CreatedEntities::Tab { tab, pane },
        None => CreatedEntities::Pane(pane),
    };
    let control =
        GrantedControl::new(state.epoch(), grant).map_err(|_| ReducerFailure::InvalidFacts)?;
    patch.resolution = Resolution::Completed(Outcome::CreatedWithControl(
        CreatedOrganization::new(entities, control).map_err(|_| ReducerFailure::InvalidFacts)?,
    ));
    patch.effects.push(OrganizationEffect::RegisterCreatedPane {
        pane,
        initial_grant: grant,
    });
    patch.candidate.replace_organization(org, runtime)?;
    Ok(patch)
}

pub(crate) fn allocate<T: LocalIdentity>(
    server: ServerId,
    mark: &mut u64,
) -> Result<T, ReducerFailure> {
    let mut allocator = IdAllocator::<T>::from_high_water(server, *mark);
    let id = allocator
        .allocate()
        .map_err(|_| ReducerFailure::CounterExhausted)?;
    *mark = allocator.high_water();
    Ok(id)
}
pub(crate) fn ordinal(marks: &mut IdentityHighWater) -> Result<CreationOrdinal, ReducerFailure> {
    marks.creation_ordinal = marks
        .creation_ordinal
        .checked_add(1)
        .ok_or(ReducerFailure::CounterExhausted)?;
    Ok(CreationOrdinal::new(marks.creation_ordinal))
}
pub(crate) fn install_facts(
    org: &mut OrganizationState,
    mut facts: OrganizationRetainedParts,
) -> Result<(), ReducerFailure> {
    facts
        .workspace_names
        .sort_by_key(WorkspaceNameBasis::workspace);
    facts.creation_defaults.sort_by_key(TabCreationDefault::tab);
    facts.pane_generations.sort_by_key(|v| v.pane());
    facts.tab_membership_versions.sort_by_key(|v| v.id());
    facts.repository_inventory_versions.sort_by_key(|v| v.id());
    facts.worktree_inventory.sort_by_key(|v| v.worktree());
    org.retained =
        Some(OrganizationRetainedFacts::new(facts).map_err(|_| ReducerFailure::InvalidFacts)?);
    Ok(())
}

pub(crate) fn validation<'a>(
    state: &ReducerState,
    command: &Command,
    context: &'a ReductionContext,
) -> Result<&'a DirectoryValidation, OperationFailure> {
    let facts = context
        .facts
        .directories
        .as_ref()
        .ok_or(OperationFailure::StaleValidation)?;
    let basis = state
        .directory_basis(command)
        .map_err(|_| OperationFailure::StaleValidation)?;
    if facts.binding != context.binding()
        || facts.basis != basis
        || facts.home.directory != state.server_home
    {
        return Err(OperationFailure::StaleValidation);
    }
    let matches = |value: Option<&DirectoryFact>, expected: Option<&ServerDirectory>| {
        value.map(|v| &v.directory) == expected
    };
    if !matches(
        facts.pin.as_ref(),
        basis.pin.as_ref().and_then(|v| v.value.as_ref()),
    ) || !matches(
        facts.source.as_ref(),
        basis.source.as_ref().map(|v| &v.directory),
    ) || !matches(facts.requested.as_ref(), basis.requested.as_ref())
        || !matches(
            facts.worktree.as_ref(),
            basis.worktree.as_ref().map(|v| &v.worktree.directory),
        )
        || [
            &facts.home,
            facts.pin.as_ref().unwrap_or(&facts.home),
            facts.source.as_ref().unwrap_or(&facts.home),
            facts.requested.as_ref().unwrap_or(&facts.home),
            facts.worktree.as_ref().unwrap_or(&facts.home),
        ]
        .iter()
        .any(|v| {
            v.directory.server() != state.organization().server
                || v.physically_inside
                    .is_some_and(|id| Some(id) != basis.worktree.as_ref().map(|v| v.worktree.id))
        })
    {
        return Err(OperationFailure::StaleValidation);
    }
    Ok(facts)
}
fn select_directory(
    state: &ReducerState,
    command: &Command,
    context: &ReductionContext,
) -> Result<ServerDirectory, OperationFailure> {
    let facts = validation(state, command, context)?;
    if facts.pin.as_ref().is_some_and(|v| !v.available) {
        return Err(OperationFailure::DirectoryUnavailable);
    }
    if let Some(worktree) = &facts.basis.worktree {
        if worktree.worktree.missing || !worktree.inventory.listed() || facts.worktree_bare {
            return Err(OperationFailure::DirectoryUnavailable);
        }
        let root = facts
            .worktree
            .as_ref()
            .filter(|v| v.available)
            .ok_or(OperationFailure::DirectoryUnavailable)?;
        if let Some(pin) = facts
            .pin
            .as_ref()
            .filter(|v| v.physically_inside == Some(worktree.worktree.id))
        {
            return Ok(pin.directory.clone());
        }
        if let Some(source) = facts
            .source
            .as_ref()
            .filter(|v| v.available && v.physically_inside == Some(worktree.worktree.id))
        {
            return Ok(source.directory.clone());
        }
        return Ok(root.directory.clone());
    }
    let selected = facts
        .pin
        .as_ref()
        .or(facts.requested.as_ref())
        .or(facts.source.as_ref())
        .unwrap_or(&facts.home);
    if !selected.available {
        return Err(OperationFailure::DirectoryUnavailable);
    }
    Ok(selected.directory.clone())
}

pub(crate) fn plan_git_inventory(
    state: &ReducerState,
    inventory: &GitInventoryPatch,
    memberships: &[GitInventoryMembership],
    context: &ReductionContext,
) -> Result<ReductionPatch, ReducerFailure> {
    let old = state.organization();
    let server = old.server;
    if inventory.directory.server() != server
        || inventory.repository.is_some_and(|id| id.server() != server)
        || inventory
            .worktrees
            .iter()
            .any(|v| v.directory.server() != server || v.id.is_some_and(|id| id.server() != server))
    {
        return Err(ReducerFailure::WrongServer);
    }
    if state.creation_barrier() {
        return Ok(ReductionPatch::rejected(
            state,
            OperationFailure::BusyRetiring,
        ));
    }
    if inventory.worktrees.len() > state.limits.worktrees || memberships.len() > state.limits.tabs {
        return Ok(ReductionPatch::rejected(state, OperationFailure::Capacity));
    }
    if inventory
        .repository
        .is_some_and(|id| !old.repositories.iter().any(|v| v.id == id))
        || inventory.worktrees.iter().any(|v| {
            v.id.is_some_and(|id| !old.worktrees.iter().any(|w| w.id == id))
        })
    {
        return Ok(ReductionPatch::rejected(
            state,
            OperationFailure::TargetDeleted,
        ));
    }
    if inventory.worktrees.iter().any(|v| {
        v.id.is_some_and(|id| {
            old.worktrees
                .iter()
                .any(|w| w.id == id && Some(w.repository) != inventory.repository)
        })
    }) {
        return Ok(ReductionPatch::rejected(
            state,
            OperationFailure::WrongMembership,
        ));
    }
    let facts = old.retained.as_ref().ok_or(ReducerFailure::InvalidFacts)?;
    let version = facts
        .repository_inventory_versions()
        .iter()
        .find(|v| Some(v.id()) == inventory.repository)
        .map(|v| v.version());
    if version != inventory.expected_inventory_version {
        return Ok(ReductionPatch::rejected(
            state,
            OperationFailure::StaleValidation,
        ));
    }
    let mut marks = state.high_water;
    let repository = match inventory.repository {
        Some(id) => id,
        None => allocate::<RepositoryId>(server, &mut marks.repository)?,
    };
    let mut worktrees = Vec::with_capacity(inventory.worktrees.len());
    for entry in &inventory.worktrees {
        let id = match entry.id {
            Some(id) => id,
            None => allocate::<WorktreeId>(server, &mut marks.worktree)?,
        };
        worktrees.push((
            WorktreeRecord {
                id,
                repository,
                directory: entry.directory.clone(),
                main: entry.main,
                missing: entry.missing,
            },
            entry.listed,
        ));
    }
    let memberships = memberships
        .iter()
        .map(|edit| {
            let worktree = edit
                .destination
                .map(|index| {
                    worktrees
                        .get(index)
                        .map(|(worktree, _)| worktree.id)
                        .ok_or(ReducerFailure::InvalidFacts)
                })
                .transpose()?;
            Ok(WorktreeMembershipEdit {
                tab: edit.tab,
                expected_version: edit.expected_version,
                worktree,
            })
        })
        .collect::<Result<Vec<_>, ReducerFailure>>()?;
    let patch = WorktreeOrganizationPatch {
        binding: context.binding(),
        scope: RepositoryScope { server, repository },
        expected_inventory_version: inventory.expected_inventory_version,
        repositories: vec![RepositoryRecord {
            id: repository,
            directory: inventory.directory.clone(),
        }],
        worktrees,
        memberships,
        settlement: None,
    };
    plan_worktree_patch_with_allocation(state, &patch, Some(marks))
}

pub(crate) fn plan_worktree_patch(
    state: &ReducerState,
    patch: &WorktreeOrganizationPatch,
) -> Result<ReductionPatch, ReducerFailure> {
    plan_worktree_patch_with_allocation(state, patch, None)
}

/// Only inventory planning can supply fresh marks here. Those identities are
/// allocated and inserted in this transaction, never borrowed from a reservation.
fn plan_worktree_patch_with_allocation(
    state: &ReducerState,
    patch: &WorktreeOrganizationPatch,
    fresh_marks: Option<IdentityHighWater>,
) -> Result<ReductionPatch, ReducerFailure> {
    if patch.repositories.len() > state.limits.repositories
        || patch.worktrees.len() > state.limits.worktrees
        || patch.memberships.len() > state.limits.tabs
    {
        return Ok(ReductionPatch::rejected(state, OperationFailure::Capacity));
    }
    if patch
        .repositories
        .iter()
        .map(|v| v.id)
        .collect::<std::collections::BTreeSet<_>>()
        .len()
        != patch.repositories.len()
        || patch
            .worktrees
            .iter()
            .map(|(v, _)| v.id)
            .collect::<std::collections::BTreeSet<_>>()
            .len()
            != patch.worktrees.len()
        || patch
            .memberships
            .iter()
            .map(|v| v.tab)
            .collect::<std::collections::BTreeSet<_>>()
            .len()
            != patch.memberships.len()
    {
        return Err(ReducerFailure::InvalidFacts);
    }
    if patch.scope.validate().is_err()
        || patch.scope.server != state.organization().server
        || !super::state::binding_fits(patch.binding, state.high_water, patch.scope.server)
    {
        return Err(ReducerFailure::WrongServer);
    }
    let mut reduction = ReductionPatch::unchanged(state, Outcome::Applied);
    if let Some(marks) = fresh_marks {
        if patch.settlement.is_some() || !marks.dominates(state.high_water) {
            return Err(ReducerFailure::InvalidFacts);
        }
        reduction.candidate.high_water = marks;
    }
    let old = state.organization();
    let old_facts = old.retained.as_ref().ok_or(ReducerFailure::InvalidFacts)?;
    if old_facts
        .repository_inventory_versions()
        .iter()
        .find(|v| v.id() == patch.scope.repository)
        .map(|v| v.version())
        != patch.expected_inventory_version
    {
        return Ok(ReductionPatch::rejected(
            state,
            OperationFailure::StaleValidation,
        ));
    }
    if let Some(settlement) = &patch.settlement {
        if !matches!(
            settlement.phase,
            GitIntentPhase::Started | GitIntentPhase::ExternalFinished
        ) || settlement.reservation.binding() != patch.binding
            || state.git_allocations.get(&patch.binding.id) != Some(&settlement.reservation)
        {
            return Ok(ReductionPatch::rejected(
                state,
                OperationFailure::StaleValidation,
            ));
        }
        if settlement
            .remove_tabs
            .iter()
            .any(|tab| old.tabs.iter().find(|v| v.id == tab.id) != Some(tab))
        {
            return Ok(ReductionPatch::rejected(
                state,
                OperationFailure::StaleAssessment,
            ));
        }
        let remove: Vec<_> = old
            .panes
            .iter()
            .filter(|v| settlement.remove_tabs.iter().any(|t| t.id == v.tab))
            .map(|v| v.id)
            .collect();
        // Surviving targets blocked by a different accepted destructive operation
        // cannot be resurrected through an older Git settlement.
        if state
            .retirements
            .values()
            .filter(|v| v.binding() != patch.binding)
            .any(|v| v.panes().iter().any(|p| remove.contains(&p.pane())))
        {
            return Ok(ReductionPatch::rejected(
                state,
                OperationFailure::BusyRetiring,
            ));
        }
        // Native Stop retains transferred owners after their phase becomes Ended.
        // Only affected Sessions fence settlement, not unrelated native cleanup.
        let native_cleanup_pending = state
            .native_retirements
            .values()
            .flat_map(|v| v.sessions())
            .any(|v| {
                remove.contains(&v.session().pane)
                    && !matches!(v.cleanup(), RetirementCleanupState::Confirmed(_))
            });
        if native_cleanup_pending
            || remove.iter().any(|pane| {
                state.runtime_cleanups.contains_key(pane)
                    || state.runtime(*pane).is_ok_and(|v| {
                        matches!(
                            v.phase,
                            SessionPhase::Starting { .. } | SessionPhase::Running { .. }
                        )
                    })
            })
            || state
                .retirements
                .values()
                .any(|v| v.binding() == patch.binding && !v.cleanup_confirmed())
        {
            return Ok(ReductionPatch::rejected(
                state,
                OperationFailure::BusyRetiring,
            ));
        }
        reduction = super::remove_hierarchy(state, &remove)?.into_reduction();
    } else if state.creation_barrier() {
        return Ok(ReductionPatch::rejected(
            state,
            OperationFailure::BusyRetiring,
        ));
    }
    let mut org = reduction.candidate.organization().clone();
    let mut facts = org
        .retained
        .as_ref()
        .ok_or(ReducerFailure::InvalidFacts)?
        .parts()
        .clone();
    let reservation = patch.settlement.as_ref().map(|v| v.reservation.parts());
    // Compare identity-keyed values; upsert order is not an inventory change.
    let mut inventory_changed = false;
    for repository in &patch.repositories {
        if repository.id != patch.scope.repository
            || repository.directory.server() != patch.scope.server
        {
            return Err(ReducerFailure::InvalidFacts);
        }
        if !old.repositories.iter().any(|v| v.id == repository.id)
            && reservation.and_then(|v| v.repository) != Some(repository.id)
            && !fresh_marks.is_some_and(|marks| {
                state.high_water.repository < repository.id.local()
                    && repository.id.local() <= marks.repository
            })
        {
            return Err(ReducerFailure::InvalidFacts);
        }
        inventory_changed |=
            old.repositories.iter().find(|v| v.id == repository.id) != Some(repository);
        org.repositories.retain(|v| v.id != repository.id);
        org.repositories.push(repository.clone());
    }
    for (worktree, listed) in &patch.worktrees {
        if worktree.repository != patch.scope.repository
            || worktree.id.server() != patch.scope.server
            || worktree.directory.server() != patch.scope.server
        {
            return Err(ReducerFailure::InvalidFacts);
        }
        if !old.worktrees.iter().any(|v| v.id == worktree.id)
            && reservation.and_then(|v| v.worktree) != Some(worktree.id)
            && !fresh_marks.is_some_and(|marks| {
                state.high_water.worktree < worktree.id.local()
                    && worktree.id.local() <= marks.worktree
            })
        {
            return Err(ReducerFailure::InvalidFacts);
        }
        let previous = facts
            .worktree_inventory
            .iter()
            .find(|v| v.worktree() == worktree.id)
            .map(|v| v.version());
        let unchanged = old.worktrees.iter().find(|v| v.id == worktree.id) == Some(worktree)
            && facts
                .worktree_inventory
                .iter()
                .find(|v| v.worktree() == worktree.id)
                .is_some_and(|v| v.listed() == *listed);
        inventory_changed |= !unchanged;
        let version = match previous {
            Some(version) if unchanged => version,
            Some(version) => version
                .checked_successor()
                .ok_or(ReducerFailure::CounterExhausted)?,
            None => FieldVersion::new(1),
        };
        org.worktrees.retain(|v| v.id != worktree.id);
        org.worktrees.push(worktree.clone());
        facts
            .worktree_inventory
            .retain(|v| v.worktree() != worktree.id);
        facts
            .worktree_inventory
            .push(WorktreeInventoryFacts::new(worktree.id, *listed, version));
    }
    for edit in &patch.memberships {
        let Some(tab) = org.tabs.iter_mut().find(|v| v.id == edit.tab) else {
            return Ok(ReductionPatch::rejected(
                state,
                OperationFailure::TargetDeleted,
            ));
        };
        let version = facts
            .tab_membership_versions
            .iter_mut()
            .find(|v| v.id() == edit.tab)
            .ok_or(ReducerFailure::InvalidFacts)?;
        if version.version() != edit.expected_version {
            return Ok(ReductionPatch::rejected(
                state,
                OperationFailure::StaleValidation,
            ));
        }
        if edit.worktree.is_some_and(|id| {
            !org.worktrees
                .iter()
                .any(|w| w.id == id && w.repository == patch.scope.repository)
        }) {
            return Err(ReducerFailure::InvalidFacts);
        }
        if tab.worktree != edit.worktree {
            tab.worktree = edit.worktree;
            *version = TabMembershipVersion::new(
                tab.id,
                version
                    .version()
                    .checked_successor()
                    .ok_or(ReducerFailure::CounterExhausted)?,
            );
        }
    }
    let mut runtime = reduction.candidate.snapshot.runtime().to_vec();
    if let Some(settlement) = &patch.settlement {
        for fallback in &settlement.reservation.parts().fallbacks {
            // Restore a Workspace removed only by this transaction's final-child
            // cascade. A Workspace deleted before settlement is never recreated.
            let Some(original) = old
                .workspaces
                .iter()
                .find(|v| v.id == fallback.plan.workspace)
            else {
                return Ok(ReductionPatch::rejected(
                    state,
                    OperationFailure::TargetDeleted,
                ));
            };
            if !old.worktrees.iter().any(|v| {
                v.id == fallback.plan.worktree
                    && v.repository == patch.scope.repository
                    && !v.missing
            }) {
                return Ok(ReductionPatch::rejected(
                    state,
                    OperationFailure::StaleValidation,
                ));
            }
            if !org.workspaces.iter().any(|v| v.id == original.id) {
                org.workspaces.push(WorkspaceRecord {
                    root_tab: fallback.tab,
                    ..original.clone()
                });
                facts.workspace_names.push(
                    old_facts
                        .workspace_names()
                        .iter()
                        .find(|v| v.workspace() == original.id)
                        .ok_or(ReducerFailure::InvalidFacts)?
                        .clone(),
                );
            }
            org.tabs.push(TabRecord {
                id: fallback.tab,
                workspace: original.id,
                worktree: Some(fallback.plan.worktree),
                root_pane: fallback.pane,
                creation_ordinal: fallback.tab_creation_ordinal,
            });
            org.panes.push(PaneRecord {
                id: fallback.pane,
                tab: fallback.tab,
                creation_ordinal: fallback.pane_creation_ordinal,
                creation: PaneCreation::Initial,
                starting_directory: fallback.plan.directory.clone(),
                confirmed_restoration_directory: Versioned {
                    value: None,
                    version: FieldVersion::new(1),
                },
            });
            facts.creation_defaults.push(
                TabCreationDefault::new(fallback.tab, vec![fallback.pane])
                    .map_err(|_| ReducerFailure::InvalidFacts)?,
            );
            facts
                .tab_membership_versions
                .push(TabMembershipVersion::new(
                    fallback.tab,
                    FieldVersion::new(1),
                ));
            let generation = ControlGeneration::new(1);
            facts.pane_generations.push(PaneGenerationHighWater::new(
                fallback.pane,
                SessionGeneration::new(0),
                generation,
            ));
            let control = if settlement.creator_still_authorized {
                ControlRecord::Held {
                    client: patch.binding.id.stream().client(),
                    generation,
                }
            } else {
                ControlRecord::Unclaimed
            };
            runtime.push(PaneRuntimeRecord {
                pane: fallback.pane,
                session_generation: SessionGeneration::new(0),
                phase: SessionPhase::NotStarted,
                control_generation: generation,
                control,
            });
            if settlement.creator_still_authorized {
                reduction
                    .effects
                    .push(OrganizationEffect::RegisterCreatedPane {
                        pane: fallback.pane,
                        initial_grant: ControlGrant::new(
                            fallback.pane,
                            patch.binding.id.stream().client(),
                            generation,
                        )
                        .map_err(|_| ReducerFailure::InvalidFacts)?,
                    });
            }
        }
        reduction
            .candidate
            .git_allocations
            .remove(&patch.binding.id);
        reduction
            .candidate
            .retirements
            .retain(|_, value| value.binding() != patch.binding);
        reduction.operations.push(OperationWrite::GitAllocation(
            GitAllocationMutation::Consume {
                binding: patch.binding,
            },
        ));
    }
    if inventory_changed {
        let next = match patch.expected_inventory_version {
            Some(v) => v
                .checked_successor()
                .ok_or(ReducerFailure::CounterExhausted)?,
            None => FieldVersion::new(1),
        };
        facts
            .repository_inventory_versions
            .retain(|v| v.id() != patch.scope.repository);
        facts
            .repository_inventory_versions
            .push(RepositoryInventoryVersion::new(
                patch.scope.repository,
                next,
            ));
    }
    install_facts(&mut org, facts)?;
    super::naming::refresh_root_names(state, &mut org)?;
    reduction.candidate.replace_organization(org, runtime)?;
    Ok(reduction)
}

pub(crate) fn plan_git_allocation(
    state: &ReducerState,
    request: &GitAllocationRequest,
    context: &ReductionContext,
) -> Result<ReductionPatch, ReducerFailure> {
    if state.creation_barrier() {
        return Ok(ReductionPatch::rejected(
            state,
            OperationFailure::BusyRetiring,
        ));
    }
    if state.git_allocations.contains_key(&context.operation) {
        return Err(ReducerFailure::InvalidFacts);
    }
    if request.fallbacks.len() > state.limits.workspaces
        || state.git_allocations.len() >= state.limits.retirement_batches
    {
        return Ok(ReductionPatch::rejected(state, OperationFailure::Capacity));
    }
    if request.fallbacks.iter().any(|v| {
        !state
            .organization()
            .workspaces
            .iter()
            .any(|w| w.id == v.workspace)
            || !state
                .organization()
                .worktrees
                .iter()
                .any(|w| w.id == v.worktree && !w.missing)
            || v.directory.server() != state.organization().server
            || v.directory.bytes().len() > state.limits.directory_bytes
    }) {
        return Ok(ReductionPatch::rejected(
            state,
            OperationFailure::WrongMembership,
        ));
    }
    let mut reduction = ReductionPatch::unchanged(state, Outcome::Applied);
    let server = state.organization().server;
    let marks = &mut reduction.candidate.high_water;
    let repository = if request.repository {
        Some(allocate::<RepositoryId>(server, &mut marks.repository)?)
    } else {
        None
    };
    let worktree = if request.worktree {
        Some(allocate::<WorktreeId>(server, &mut marks.worktree)?)
    } else {
        None
    };
    let mut fallbacks = Vec::new();
    for plan in &request.fallbacks {
        fallbacks.push(GitReservedFallback {
            plan: plan.clone(),
            tab: allocate::<TabId>(server, &mut marks.tab)?,
            pane: allocate::<PaneId>(server, &mut marks.pane)?,
            tab_creation_ordinal: ordinal(marks)?,
            pane_creation_ordinal: ordinal(marks)?,
        });
    }
    let reservation = GitAllocationReservation::new(GitAllocationReservationParts {
        binding: context.binding(),
        expected_high_water: state.high_water,
        resulting_high_water: *marks,
        repository,
        worktree,
        fallbacks,
    })
    .map_err(|_| ReducerFailure::InvalidFacts)?;
    reduction.operations.push(OperationWrite::GitAllocation(
        GitAllocationMutation::Reserve(Box::new(reservation.clone())),
    ));
    reduction
        .candidate
        .git_allocations
        .insert(context.operation, reservation.clone());
    reduction.resolution = Resolution::AllocationReserved(Box::new(reservation));
    Ok(reduction)
}
