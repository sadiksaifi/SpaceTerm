//! Relational organization projection. Core owns hierarchy and mutation policy.

use super::{database_adapter::classify, limits::SizeBudget, transaction::SqlWriter};
use rusqlite::{Params, params};
use spaceterm_core::authority::FieldVersion;
use spaceterm_core::operation_ledger_contracts::RecoveredOperationRecord;
use spaceterm_core::organization::*;
use spaceterm_core::organization_retained::RetainedOrganizationWrite as Retained;
use spaceterm_core::storage::{OperationWrite, RecoveredState, StoreFailure, TransactionParts};
use spaceterm_core::worktrees::intent::{GitAllocationMutation, GitAllocationReservation};
use std::collections::{BTreeMap, BTreeSet};

fn invalid<T>() -> Result<T, StoreFailure> {
    Err(StoreFailure::InvalidTransaction)
}
fn version<T: PartialEq>(old: &Versioned<T>, new: &Versioned<T>) -> Result<(), StoreFailure> {
    if new.version < old.version
        || (old.value != new.value && old.version.checked_successor() != Some(new.version))
    {
        return invalid();
    }
    Ok(())
}
fn retained_version(old: FieldVersion, new: FieldVersion) -> Result<(), StoreFailure> {
    if new < old {
        return invalid();
    }
    Ok(())
}

fn target(write: &OrganizationWrite) -> (u8, u64) {
    match write {
        OrganizationWrite::PutWorkspace(v) => (0, v.id.local()),
        OrganizationWrite::DeleteWorkspace(v) => (0, v.local()),
        OrganizationWrite::PutTab(v) => (1, v.id.local()),
        OrganizationWrite::DeleteTab(v) => (1, v.local()),
        OrganizationWrite::PutPane(v) => (2, v.id.local()),
        OrganizationWrite::DeletePane(v) => (2, v.local()),
        OrganizationWrite::PutRepository(v) => (3, v.id.local()),
        OrganizationWrite::DeleteRepository(v) => (3, v.local()),
        OrganizationWrite::PutWorktree(v) => (4, v.id.local()),
        OrganizationWrite::DeleteWorktree(v) => (4, v.local()),
        OrganizationWrite::SetServerSettings(_) => (5, 0),
        OrganizationWrite::Retained(v) => match v {
            Retained::PutWorkspaceNameBasis(v) => (6, v.workspace().local()),
            Retained::DeleteWorkspaceNameBasis(v) => (6, v.local()),
            Retained::PutCreationDefault(v) => (7, v.tab().local()),
            Retained::DeleteCreationDefault(v) => (7, v.local()),
            Retained::SetPaneGenerationHighWater(v) => (8, v.pane().local()),
            Retained::DeletePaneGenerationHighWater(v) => (8, v.local()),
            Retained::SetTabMembershipVersion(v) => (9, v.id().local()),
            Retained::DeleteTabMembershipVersion(v) => (9, v.local()),
            Retained::SetRepositoryInventoryVersion(v) => (10, v.id().local()),
            Retained::DeleteRepositoryInventoryVersion(v) => (10, v.local()),
            Retained::SetWorktreeInventoryFacts(v) => (11, v.worktree().local()),
            Retained::DeleteWorktreeInventoryFacts(v) => (11, v.local()),
            Retained::SetRemoteAccess(_) => (12, 0),
        },
    }
}

pub(super) fn preflight(
    parts: &TransactionParts,
    budget: &mut SizeBudget,
) -> Result<(), StoreFailure> {
    let mut targets = BTreeSet::new();
    for write in &parts.organization_writes {
        write
            .validate_scope(parts.server)
            .map_err(|_| StoreFailure::InvalidTransaction)?;
        if !targets.insert(target(write)) {
            return invalid();
        }
        // Includes cloned typed values when auxiliary slots are merged.
        budget
            .charge(1024)
            .map_err(|_| StoreFailure::InvalidTransaction)?;
        let bytes = match write {
            OrganizationWrite::PutWorkspace(v) => {
                v.name.value.display.as_str().len()
                    + v.pinned_directory
                        .value
                        .as_ref()
                        .map_or(0, |v| v.bytes().len())
            }
            OrganizationWrite::PutPane(v) => {
                v.starting_directory.bytes().len()
                    + v.confirmed_restoration_directory
                        .value
                        .as_ref()
                        .map_or(0, |v| v.bytes().len())
            }
            OrganizationWrite::PutRepository(v) => v.directory.bytes().len(),
            OrganizationWrite::PutWorktree(v) => v.directory.bytes().len(),
            OrganizationWrite::SetServerSettings(v) => v.worktree_location.value.as_str().len(),
            OrganizationWrite::Retained(Retained::PutWorkspaceNameBasis(v)) => {
                v.automatic_base().as_str().len() + v.assigned_automatic().as_str().len()
            }
            OrganizationWrite::Retained(Retained::PutCreationDefault(v)) => v
                .replay_order()
                .len()
                .checked_mul(24)
                .ok_or(StoreFailure::InvalidTransaction)?,
            OrganizationWrite::Retained(Retained::SetRemoteAccess(v)) => {
                v.value.endpoints().len() * 32
            }
            _ => 0,
        };
        budget
            .charge(bytes)
            .map_err(|_| StoreFailure::InvalidTransaction)?;
    }
    Ok(())
}

fn consuming<'a>(
    parts: &TransactionParts,
    old: &'a RecoveredState,
) -> impl Iterator<Item = &'a GitAllocationReservation> {
    old.git_allocations.iter().filter(move |reservation| {
        let binding = reservation.binding();
        parts.operation_writes.iter().any(|write| matches!(write, OperationWrite::GitAllocation(GitAllocationMutation::Consume { binding: value }) if *value == binding))
            && old.ledger.as_ref().is_some_and(|ledger| ledger.operations.iter().any(|entry| matches!(entry, RecoveredOperationRecord::Pending(v) if v.binding == binding)))
            && parts.operation_writes.iter().any(|write| matches!(write, OperationWrite::Ledger(v) if matches!(v.as_ref(), spaceterm_core::operation_ledger_contracts::LedgerWrite::CompleteOperation { record, .. } if record.binding == binding)))
    })
}

fn fresh(local: u64, old: u64, new: u64, reservation: bool) -> Result<(), StoreFailure> {
    if local == 0 || local > new || (local <= old && !reservation) {
        return invalid();
    }
    Ok(())
}
fn ordinal(value: u64, old: u64, new: u64, reservation: bool) -> Result<(), StoreFailure> {
    fresh(value, old, new, reservation)
}

pub(super) fn compare(parts: &TransactionParts, old: &RecoveredState) -> Result<(), StoreFailure> {
    let before = &old.organization;
    let retained = before
        .retained
        .as_ref()
        .ok_or(StoreFailure::InvalidDurableState)?;
    let workspaces: BTreeMap<_, _> = before.workspaces.iter().map(|v| (v.id, v)).collect();
    let tabs: BTreeMap<_, _> = before.tabs.iter().map(|v| (v.id, v)).collect();
    let panes: BTreeMap<_, _> = before.panes.iter().map(|v| (v.id, v)).collect();
    let repositories: BTreeSet<_> = before.repositories.iter().map(|v| v.id).collect();
    let worktrees: BTreeMap<_, _> = before.worktrees.iter().map(|v| (v.id, v)).collect();
    let generations: BTreeMap<_, _> = retained
        .pane_generations()
        .iter()
        .map(|v| (v.pane(), v))
        .collect();
    let tab_versions: BTreeMap<_, _> = retained
        .tab_membership_versions()
        .iter()
        .map(|v| (v.id(), v))
        .collect();
    let repository_versions: BTreeMap<_, _> = retained
        .repository_inventory_versions()
        .iter()
        .map(|v| (v.id(), v))
        .collect();
    let inventory: BTreeMap<_, _> = retained
        .worktree_inventory()
        .iter()
        .map(|v| (v.worktree(), v))
        .collect();
    let marks = parts.expected_high_water;
    let next = parts.resulting_high_water;
    for write in &parts.organization_writes {
        match write {
            OrganizationWrite::PutWorkspace(v) => {
                if let Some(previous) = workspaces.get(&v.id) {
                    if previous.creation_ordinal != v.creation_ordinal {
                        return invalid();
                    }
                    version(&previous.name, &v.name)?;
                    version(&previous.pinned_directory, &v.pinned_directory)?;
                } else {
                    fresh(v.id.local(), marks.workspace, next.workspace, false)?;
                    ordinal(
                        v.creation_ordinal.get(),
                        marks.creation_ordinal,
                        next.creation_ordinal,
                        false,
                    )?;
                }
            }
            OrganizationWrite::PutTab(v) => {
                if let Some(previous) = tabs.get(&v.id) {
                    if previous.workspace != v.workspace
                        || previous.creation_ordinal != v.creation_ordinal
                    {
                        return invalid();
                    }
                } else {
                    let reserved = consuming(parts, old).any(|r| {
                        r.parts().fallbacks.iter().any(|f| {
                            f.tab == v.id
                                && f.pane == v.root_pane
                                && f.plan.workspace == v.workspace
                                && v.worktree == Some(f.plan.worktree)
                                && f.tab_creation_ordinal == v.creation_ordinal
                        })
                    });
                    fresh(v.id.local(), marks.tab, next.tab, reserved)?;
                    ordinal(
                        v.creation_ordinal.get(),
                        marks.creation_ordinal,
                        next.creation_ordinal,
                        reserved,
                    )?;
                }
            }
            OrganizationWrite::PutPane(v) => {
                if let Some(previous) = panes.get(&v.id) {
                    if previous.tab != v.tab || previous.creation_ordinal != v.creation_ordinal {
                        return invalid();
                    }
                    // Pane repair and shell confirmation share one directory field version.
                    version(
                        &Versioned {
                            value: (
                                &previous.starting_directory,
                                &previous.confirmed_restoration_directory.value,
                            ),
                            version: previous.confirmed_restoration_directory.version,
                        },
                        &Versioned {
                            value: (
                                &v.starting_directory,
                                &v.confirmed_restoration_directory.value,
                            ),
                            version: v.confirmed_restoration_directory.version,
                        },
                    )?;
                } else {
                    let reserved = consuming(parts, old).any(|r| {
                        r.parts().fallbacks.iter().any(|f| {
                            f.pane == v.id
                                && f.tab == v.tab
                                && f.pane_creation_ordinal == v.creation_ordinal
                                && f.plan.directory == v.starting_directory
                                && v.creation == PaneCreation::Initial
                        })
                    });
                    fresh(v.id.local(), marks.pane, next.pane, reserved)?;
                    ordinal(
                        v.creation_ordinal.get(),
                        marks.creation_ordinal,
                        next.creation_ordinal,
                        reserved,
                    )?;
                }
            }
            OrganizationWrite::PutRepository(v) => {
                if !repositories.contains(&v.id) {
                    let reserved =
                        consuming(parts, old).any(|r| r.parts().repository == Some(v.id));
                    fresh(v.id.local(), marks.repository, next.repository, reserved)?;
                }
            }
            OrganizationWrite::PutWorktree(v) => {
                if let Some(previous) = worktrees.get(&v.id) {
                    if previous.repository != v.repository {
                        return invalid();
                    }
                } else {
                    let reserved = consuming(parts, old).any(|r| r.parts().worktree == Some(v.id));
                    fresh(v.id.local(), marks.worktree, next.worktree, reserved)?;
                }
            }
            OrganizationWrite::SetServerSettings(v) => {
                version(&before.settings.worktree_location, &v.worktree_location)?
            }
            OrganizationWrite::Retained(Retained::SetPaneGenerationHighWater(v)) => {
                if let Some(previous) = generations.get(&v.pane())
                    && !v.dominates(**previous)
                {
                    return invalid();
                }
            }
            OrganizationWrite::Retained(Retained::SetTabMembershipVersion(v)) => {
                if let Some(previous) = tab_versions.get(&v.id()) {
                    retained_version(previous.version(), v.version())?;
                }
            }
            OrganizationWrite::Retained(Retained::SetRepositoryInventoryVersion(v)) => {
                if let Some(previous) = repository_versions.get(&v.id()) {
                    retained_version(previous.version(), v.version())?;
                }
            }
            OrganizationWrite::Retained(Retained::SetWorktreeInventoryFacts(v)) => {
                if let Some(previous) = inventory.get(&v.worktree()) {
                    retained_version(previous.version(), v.version())?;
                    if previous.listed() != v.listed()
                        && previous.version().checked_successor() != Some(v.version())
                    {
                        return invalid();
                    }
                }
            }
            OrganizationWrite::Retained(Retained::SetRemoteAccess(v)) => {
                version(retained.remote_access(), v)?
            }
            _ => {}
        }
    }
    Ok(())
}

fn put<P: Params>(
    writer: &mut SqlWriter<'_, '_>,
    table: &str,
    key_column: &str,
    id: u64,
    insert: &str,
    update: &str,
    parameters: P,
) -> Result<(), StoreFailure> {
    let exists: u64 = writer
        .sql
        .query_row(
            &format!("SELECT count(*) FROM {table} WHERE {key_column} = ?1"),
            [id.to_be_bytes().as_slice()],
            |row| row.get(0),
        )
        .map_err(classify)?;
    if exists > 1 {
        return Err(StoreFailure::InvalidDurableState);
    }
    writer.exact(if exists == 0 { insert } else { update }, parameters, 1)
}
fn delete(
    writer: &mut SqlWriter<'_, '_>,
    table: &str,
    column: &str,
    id: u64,
) -> Result<(), StoreFailure> {
    writer.exact(
        &format!("DELETE FROM {table} WHERE {column} = ?1"),
        [id.to_be_bytes().as_slice()],
        1,
    )
}

pub(super) fn row_bound(
    sql: &crate::storage::Transaction<'_>,
    parts: &TransactionParts,
) -> Result<usize, StoreFailure> {
    let mut total = 2_usize; // Metadata and all high-water marks.
    for write in &parts.organization_writes {
        let rows = match write {
            OrganizationWrite::PutWorkspace(_)
            | OrganizationWrite::DeleteWorkspace(_)
            | OrganizationWrite::PutPane(_)
            | OrganizationWrite::DeletePane(_) => 2,
            OrganizationWrite::Retained(Retained::PutCreationDefault(v)) => {
                let old: usize = sql
                    .query_row(
                        "SELECT count(*) FROM tab_creation_replay WHERE tab_id = ?1",
                        [v.tab().local().to_be_bytes().as_slice()],
                        |row| row.get(0),
                    )
                    .map_err(classify)?;
                old.checked_add(v.replay_order().len())
                    .ok_or(StoreFailure::InvalidTransaction)?
            }
            OrganizationWrite::Retained(Retained::DeleteCreationDefault(v)) => sql
                .query_row(
                    "SELECT count(*) FROM tab_creation_replay WHERE tab_id = ?1",
                    [v.local().to_be_bytes().as_slice()],
                    |row| row.get(0),
                )
                .map_err(classify)?,
            OrganizationWrite::Retained(Retained::SetRemoteAccess(v)) => {
                let old: usize = sql
                    .query_row("SELECT count(*) FROM remote_bind_endpoints", [], |row| {
                        row.get(0)
                    })
                    .map_err(classify)?;
                old + v.value.endpoints().len() + 1
            }
            _ => 1,
        };
        total = total
            .checked_add(rows)
            .ok_or(StoreFailure::InvalidTransaction)?;
    }
    Ok(total)
}

pub(super) fn apply(
    writer: &mut SqlWriter<'_, '_>,
    parts: &TransactionParts,
) -> Result<(), StoreFailure> {
    for write in &parts.organization_writes {
        match write {
            OrganizationWrite::PutWorkspace(v) => {
                let id = v.id.local().to_be_bytes();
                put(writer, "workspaces", "id", v.id.local(), "INSERT INTO workspaces VALUES (?1, ?2, ?3, ?4, ?5, ?6)", "UPDATE workspaces SET creation_ordinal = ?2, name_mode = ?3, display_name = ?4, pinned_directory = ?5, root_tab = ?6 WHERE id = ?1", params![id.as_slice(), v.creation_ordinal.get().to_be_bytes().as_slice(), match v.name.value.mode { NameMode::Automatic => 0, NameMode::Custom => 1 }, v.name.value.display.as_str(), v.pinned_directory.value.as_ref().map(ServerDirectory::bytes), v.root_tab.local().to_be_bytes().as_slice()])?;
                put(writer, "workspace_field_versions", "workspace_id", v.id.local(), "INSERT INTO workspace_field_versions VALUES (?1, ?2, ?3)", "UPDATE workspace_field_versions SET name_version = ?2, pin_version = ?3 WHERE workspace_id = ?1", params![id.as_slice(), v.name.version.get().to_be_bytes().as_slice(), v.pinned_directory.version.get().to_be_bytes().as_slice()])?;
            }
            OrganizationWrite::DeleteWorkspace(v) => {
                delete(writer, "workspace_field_versions", "workspace_id", v.local())?;
                delete(writer, "workspaces", "id", v.local())?;
            }
            OrganizationWrite::PutTab(v) => put(writer, "tabs", "id", v.id.local(), "INSERT INTO tabs VALUES (?1, ?2, ?3, ?4, ?5)", "UPDATE tabs SET workspace_id = ?2, worktree_id = ?3, root_pane = ?4, creation_ordinal = ?5 WHERE id = ?1", params![v.id.local().to_be_bytes().as_slice(), v.workspace.local().to_be_bytes().as_slice(), v.worktree.map(|id| id.local().to_be_bytes()).as_ref().map(|id| id.as_slice()), v.root_pane.local().to_be_bytes().as_slice(), v.creation_ordinal.get().to_be_bytes().as_slice()])?,
            OrganizationWrite::DeleteTab(v) => delete(writer, "tabs", "id", v.local())?,
            OrganizationWrite::PutPane(v) => {
                let (kind, source, axis, edge) = match v.creation {
                    PaneCreation::Initial => (0, None, None, None),
                    PaneCreation::SplitOf { source, axis, edge } => (1, Some(source.local().to_be_bytes()), Some(match axis { SplitAxis::Horizontal => 0, SplitAxis::Vertical => 1 }), Some(match edge { SplitEdge::Before => 0, SplitEdge::After => 1 })),
                };
                put(writer, "panes", "id", v.id.local(), "INSERT INTO panes VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)", "UPDATE panes SET tab_id = ?2, creation_ordinal = ?3, creation_kind = ?4, creation_source = ?5, split_axis = ?6, split_edge = ?7, starting_directory = ?8, restoration_directory = ?9 WHERE id = ?1", params![v.id.local().to_be_bytes().as_slice(), v.tab.local().to_be_bytes().as_slice(), v.creation_ordinal.get().to_be_bytes().as_slice(), kind, source.as_ref().map(|id| id.as_slice()), axis, edge, v.starting_directory.bytes(), v.confirmed_restoration_directory.value.as_ref().map(ServerDirectory::bytes)])?;
                put(writer, "pane_field_versions", "pane_id", v.id.local(), "INSERT INTO pane_field_versions VALUES (?1, ?2)", "UPDATE pane_field_versions SET restoration_version = ?2 WHERE pane_id = ?1", params![v.id.local().to_be_bytes().as_slice(), v.confirmed_restoration_directory.version.get().to_be_bytes().as_slice()])?;
            }
            OrganizationWrite::DeletePane(v) => {
                delete(writer, "pane_field_versions", "pane_id", v.local())?;
                delete(writer, "panes", "id", v.local())?;
            }
            OrganizationWrite::PutRepository(v) => put(writer, "repositories", "id", v.id.local(), "INSERT INTO repositories VALUES (?1, ?2)", "UPDATE repositories SET directory = ?2 WHERE id = ?1", params![v.id.local().to_be_bytes().as_slice(), v.directory.bytes()])?,
            OrganizationWrite::DeleteRepository(v) => delete(writer, "repositories", "id", v.local())?,
            OrganizationWrite::PutWorktree(v) => put(writer, "worktrees", "id", v.id.local(), "INSERT INTO worktrees VALUES (?1, ?2, ?3, ?4, ?5)", "UPDATE worktrees SET repository_id = ?2, directory = ?3, is_main = ?4, missing = ?5 WHERE id = ?1", params![v.id.local().to_be_bytes().as_slice(), v.repository.local().to_be_bytes().as_slice(), v.directory.bytes(), v.main, v.missing])?,
            OrganizationWrite::DeleteWorktree(v) => delete(writer, "worktrees", "id", v.local())?,
            OrganizationWrite::SetServerSettings(v) => writer.exact("UPDATE server_settings SET worktree_location = ?1, worktree_location_version = ?2 WHERE singleton = 1", params![v.worktree_location.value.as_str(), v.worktree_location.version.get().to_be_bytes().as_slice()], 1)?,
            OrganizationWrite::Retained(v) => apply_retained(writer, v)?,
        }
    }
    Ok(())
}

fn apply_retained(writer: &mut SqlWriter<'_, '_>, write: &Retained) -> Result<(), StoreFailure> {
    match write {
        Retained::PutWorkspaceNameBasis(v) => put(
            writer,
            "workspace_name_bases",
            "workspace_id",
            v.workspace().local(),
            "INSERT INTO workspace_name_bases VALUES (?1, ?2, ?3)",
            "UPDATE workspace_name_bases SET automatic_base = ?2, assigned_automatic = ?3 WHERE workspace_id = ?1",
            params![
                v.workspace().local().to_be_bytes().as_slice(),
                v.automatic_base().as_str(),
                v.assigned_automatic().as_str()
            ],
        ),
        Retained::DeleteWorkspaceNameBasis(v) => {
            delete(writer, "workspace_name_bases", "workspace_id", v.local())
        }
        Retained::PutCreationDefault(v) => {
            writer.execute(
                "DELETE FROM tab_creation_replay WHERE tab_id = ?1",
                [v.tab().local().to_be_bytes().as_slice()],
            )?;
            for (position, pane) in v.replay_order().iter().enumerate() {
                writer.exact(
                    "INSERT INTO tab_creation_replay VALUES (?1, ?2, ?3)",
                    params![
                        v.tab().local().to_be_bytes().as_slice(),
                        (position as u64).to_be_bytes().as_slice(),
                        pane.local().to_be_bytes().as_slice()
                    ],
                    1,
                )?;
            }
            Ok(())
        }
        Retained::DeleteCreationDefault(v) => {
            if writer.execute(
                "DELETE FROM tab_creation_replay WHERE tab_id = ?1",
                [v.local().to_be_bytes().as_slice()],
            )? == 0
            {
                return invalid();
            }
            Ok(())
        }
        Retained::SetPaneGenerationHighWater(v) => put(
            writer,
            "pane_generation_high_water",
            "pane_id",
            v.pane().local(),
            "INSERT INTO pane_generation_high_water VALUES (?1, ?2, ?3)",
            "UPDATE pane_generation_high_water SET session_generation = ?2, control_generation = ?3 WHERE pane_id = ?1",
            params![
                v.pane().local().to_be_bytes().as_slice(),
                v.session().get().to_be_bytes().as_slice(),
                v.control().get().to_be_bytes().as_slice()
            ],
        ),
        Retained::DeletePaneGenerationHighWater(v) => {
            delete(writer, "pane_generation_high_water", "pane_id", v.local())
        }
        Retained::SetTabMembershipVersion(v) => put(
            writer,
            "tab_field_versions",
            "tab_id",
            v.id().local(),
            "INSERT INTO tab_field_versions VALUES (?1, ?2)",
            "UPDATE tab_field_versions SET membership_version = ?2 WHERE tab_id = ?1",
            params![
                v.id().local().to_be_bytes().as_slice(),
                v.version().get().to_be_bytes().as_slice()
            ],
        ),
        Retained::DeleteTabMembershipVersion(v) => {
            delete(writer, "tab_field_versions", "tab_id", v.local())
        }
        Retained::SetRepositoryInventoryVersion(v) => put(
            writer,
            "repository_field_versions",
            "repository_id",
            v.id().local(),
            "INSERT INTO repository_field_versions VALUES (?1, ?2)",
            "UPDATE repository_field_versions SET inventory_version = ?2 WHERE repository_id = ?1",
            params![
                v.id().local().to_be_bytes().as_slice(),
                v.version().get().to_be_bytes().as_slice()
            ],
        ),
        Retained::DeleteRepositoryInventoryVersion(v) => delete(
            writer,
            "repository_field_versions",
            "repository_id",
            v.local(),
        ),
        Retained::SetWorktreeInventoryFacts(v) => put(
            writer,
            "worktree_inventory",
            "worktree_id",
            v.worktree().local(),
            "INSERT INTO worktree_inventory VALUES (?1, ?2, ?3)",
            "UPDATE worktree_inventory SET listed = ?2, inventory_version = ?3 WHERE worktree_id = ?1",
            params![
                v.worktree().local().to_be_bytes().as_slice(),
                v.listed(),
                v.version().get().to_be_bytes().as_slice()
            ],
        ),
        Retained::DeleteWorktreeInventoryFacts(v) => {
            delete(writer, "worktree_inventory", "worktree_id", v.local())
        }
        Retained::SetRemoteAccess(v) => {
            writer.execute("DELETE FROM remote_bind_endpoints", [])?;
            for (position, endpoint) in v.value.endpoints().iter().enumerate() {
                let (family, address, flowinfo, scope) = match endpoint {
                    std::net::SocketAddr::V4(v) => (4, v.ip().octets().to_vec(), 0, 0),
                    std::net::SocketAddr::V6(v) => {
                        (6, v.ip().octets().to_vec(), v.flowinfo(), v.scope_id())
                    }
                };
                writer.exact(
                    "INSERT INTO remote_bind_endpoints VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                    params![
                        (position as u64).to_be_bytes().as_slice(),
                        family,
                        address,
                        endpoint.port(),
                        flowinfo,
                        scope
                    ],
                    1,
                )?;
            }
            writer.exact(
                "UPDATE server_settings SET remote_access_version = ?1 WHERE singleton = 1",
                [v.version.get().to_be_bytes().as_slice()],
                1,
            )
        }
    }
}
