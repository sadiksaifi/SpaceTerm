//! Bounded complete recovery within the authority's one consistent snapshot.

use super::{
    StoreIdentity, StoreLimits, StoreServices, codec,
    database_adapter::classify,
    limits::{Collection, SizeBudget},
    schema,
};
use crate::startup::BuildIdentity;
use crate::storage::Transaction;
use rusqlite::{Row, types::ValueRef};
use spaceterm_core::authority::*;
use spaceterm_core::identity::*;
use spaceterm_core::organization::*;
use spaceterm_core::organization_retained::*;
use spaceterm_core::remote_access::RemoteAccessPolicy;
use spaceterm_core::storage::{RecoveredState, StoreFailure};

pub(super) struct Metadata {
    pub store_revision: StoreRevision,
    pub organization_revision: OrganizationRevision,
    pub epoch: ServerEpoch,
}

pub(super) fn counter(row: &Row<'_>, index: usize) -> rusqlite::Result<u64> {
    let value = row.get_ref(index)?.as_blob()?;
    codec::decode_u64(value).map_err(|_| rusqlite::Error::InvalidQuery)
}
fn boolean(row: &Row<'_>, index: usize) -> rusqlite::Result<bool> {
    // Read-only SQLite does not load CHECK expressions. Reject raw values before
    // rusqlite's bool conversion can normalize any nonzero integer to true.
    match row.get::<_, i64>(index)? {
        0 => Ok(false),
        1 => Ok(true),
        _ => Err(rusqlite::Error::InvalidQuery),
    }
}
pub(super) fn require_nulls(row: &Row<'_>, indices: &[usize]) -> rusqlite::Result<()> {
    // Variant decoders must not discard retained fields that their schema
    // requires to be NULL. Later domain validation cannot inspect lost values.
    for &index in indices {
        if !matches!(row.get_ref(index)?, ValueRef::Null) {
            return Err(rusqlite::Error::InvalidQuery);
        }
    }
    Ok(())
}
fn fixed<const N: usize>(row: &Row<'_>, index: usize) -> rusqlite::Result<[u8; N]> {
    row.get_ref(index)?
        .as_blob()?
        .try_into()
        .map_err(|_| rusqlite::Error::InvalidQuery)
}
fn checked<T, E>(value: Result<T, E>) -> rusqlite::Result<T> {
    value.map_err(|_| rusqlite::Error::InvalidQuery)
}
fn directory(row: &Row<'_>, index: usize, server: ServerId) -> rusqlite::Result<ServerDirectory> {
    let bytes = row.get_ref(index)?.as_blob()?;
    checked(ServerDirectory::new(server, bytes.to_vec()))
}
fn optional_directory(
    row: &Row<'_>,
    index: usize,
    server: ServerId,
) -> rusqlite::Result<Option<ServerDirectory>> {
    match row.get_ref(index)? {
        ValueRef::Null => Ok(None),
        _ => directory(row, index, server).map(Some),
    }
}
fn optional_id(row: &Row<'_>, index: usize) -> rusqlite::Result<Option<u64>> {
    match row.get_ref(index)? {
        ValueRef::Null => Ok(None),
        _ => counter(row, index).map(Some),
    }
}
fn name(row: &Row<'_>, index: usize) -> rusqlite::Result<NameText> {
    checked(NameText::new(row.get_ref(index)?.as_str()?.to_owned()))
}

pub(super) fn metadata(
    sql: &Transaction<'_>,
    identity: StoreIdentity,
) -> Result<Metadata, StoreFailure> {
    let (matches, encoding, origin, seed, value) = sql.query_row(
        "SELECT application_id, encoding_version, server_id, account_uid, build_identity, server_public_key, store_revision, organization_revision, last_epoch, initialization_origin, legacy_seed_decision FROM store_metadata WHERE singleton = 1",
        [], |row| {
            let build = match identity.account.build_identity { BuildIdentity::Release => 0, BuildIdentity::Preflight => 1, BuildIdentity::Development => 2 };
            let matches = row.get::<_, u32>(0)? == schema::APPLICATION_ID
                && fixed::<16>(row, 2)? == identity.server.bytes()
                && counter(row, 3)? == u64::from(identity.account.account_uid)
                && row.get::<_, i64>(4)? == build
                && fixed::<32>(row, 5)? == identity.server_public_key;
            Ok((matches, row.get::<_, u32>(1)?, row.get::<_, i64>(9)?, row.get::<_, i64>(10)?, Metadata {
                store_revision: StoreRevision::new(counter(row, 6)?),
                organization_revision: OrganizationRevision::new(counter(row, 7)?),
                epoch: ServerEpoch::new(counter(row, 8)?),
            }))
        },
    ).map_err(classify)?;
    if !matches
        || identity.server.bytes() == [0; 16]
        || identity.server_public_key == [0; 32]
        || identity.account.account_uid == 0
        || identity.account.account_uid == u32::MAX
    {
        return Err(StoreFailure::IdentityMismatch);
    }
    schema::check_encoding_version(encoding).map_err(classify)?;
    if !(0..=2).contains(&origin) || !(0..=1).contains(&seed) || (origin != 0 && seed != 0) {
        return Err(StoreFailure::InvalidDurableState);
    }
    Ok(value)
}

pub(super) fn high_water(sql: &Transaction<'_>) -> Result<IdentityHighWater, StoreFailure> {
    sql.query_row("SELECT workspace, tab, pane, worktree, repository, pairing, client, operation_stream, creation_ordinal FROM identity_high_water WHERE singleton = 1", [], |row| {
        Ok(IdentityHighWater { workspace: counter(row, 0)?, tab: counter(row, 1)?, pane: counter(row, 2)?, worktree: counter(row, 3)?, repository: counter(row, 4)?, pairing: counter(row, 5)?, client: counter(row, 6)?, operation_stream: counter(row, 7)?, creation_ordinal: counter(row, 8)? })
    }).map_err(classify)
}

struct Inventory {
    workspaces: u64,
    tabs: u64,
    panes: u64,
    repositories: u64,
    worktrees: u64,
    endpoints: u64,
}

fn count(sql: &Transaction<'_>, table: &str) -> Result<u64, StoreFailure> {
    sql.query_row(&format!("SELECT count(*) FROM {table}"), [], |row| {
        row.get(0)
    })
    .map_err(classify)
}

fn preflight(
    sql: &Transaction<'_>,
    limits: &StoreLimits,
    budget: &mut SizeBudget,
) -> Result<Inventory, StoreFailure> {
    // Table/column names are local schema constants, never caller input. Every
    // count and byte sum is read before a collection or payload is allocated.
    for (table, collection, payload) in [
        (
            "workspaces",
            Collection::Workspaces,
            "length(CAST(display_name AS BLOB)) + coalesce(length(pinned_directory), 0)",
        ),
        ("workspace_field_versions", Collection::Workspaces, "0"),
        (
            "workspace_name_bases",
            Collection::Workspaces,
            "length(CAST(automatic_base AS BLOB)) + length(CAST(assigned_automatic AS BLOB))",
        ),
        ("tabs", Collection::Tabs, "0"),
        ("tab_field_versions", Collection::Tabs, "0"),
        (
            "panes",
            Collection::Panes,
            "length(starting_directory) + coalesce(length(restoration_directory), 0)",
        ),
        ("pane_field_versions", Collection::Panes, "0"),
        ("pane_generation_high_water", Collection::Panes, "0"),
        ("tab_creation_replay", Collection::CreationReplay, "0"),
        (
            "repositories",
            Collection::Repositories,
            "length(directory)",
        ),
        ("repository_field_versions", Collection::Repositories, "0"),
        ("worktrees", Collection::Worktrees, "length(directory)"),
        ("worktree_inventory", Collection::Worktrees, "0"),
        (
            "remote_bind_endpoints",
            Collection::RemoteEndpoints,
            "length(address)",
        ),
        ("pairings", Collection::Pairings, "0"),
        ("paired_clients", Collection::Clients, "0"),
        (
            "enrollment_authorizations",
            Collection::Enrollments,
            "length(issuer)",
        ),
        (
            "pairing_receipts",
            Collection::Receipts,
            "length(canonical_body) + length(signature)",
        ),
        ("revocation_receipts", Collection::Receipts, "0"),
        ("operation_streams", Collection::OperationStreams, "0"),
        (
            "operation_entries",
            Collection::OperationEntries,
            "coalesce(length(external_progress), 0) + coalesce(length(compact_outcome), 0)",
        ),
        (
            "git_repository_locators",
            Collection::Repositories,
            "length(main_root) + length(common_directory)",
        ),
        (
            "git_allocation_reservations",
            Collection::GitIntents,
            "length(reservation)",
        ),
        (
            "git_write_intents",
            Collection::GitIntents,
            "length(action) + length(affected_scope) + coalesce(length(external_result), 0)",
        ),
        (
            "native_retirements",
            Collection::NativeRetirements,
            "length(descriptor)",
        ),
    ] {
        let (rows, bytes): (u64, u64) = sql
            .query_row(
                &format!("SELECT count(*), coalesce(sum({payload}), 0) FROM {table}"),
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .map_err(classify)?;
        limits
            .check_count(collection, rows)
            .map_err(|_| StoreFailure::InvalidDurableState)?;
        // Includes vectors, owner validation indexes and temporary per-row values.
        budget
            .charge_rows(rows, 1024)
            .map_err(|_| StoreFailure::InvalidDurableState)?;
        budget
            .charge(usize::try_from(bytes).map_err(|_| StoreFailure::InvalidDurableState)?)
            .map_err(|_| StoreFailure::InvalidDurableState)?;
    }
    // Total residency is not permission for an oversized individual field. Check
    // lengths through SQLite before any text/path/compact-outcome copy.
    for (table, column, maximum) in [
        ("workspaces", "display_name", MAX_NAME_BYTES),
        ("workspaces", "pinned_directory", MAX_DIRECTORY_BYTES),
        ("workspace_name_bases", "automatic_base", MAX_NAME_BYTES),
        ("workspace_name_bases", "assigned_automatic", MAX_NAME_BYTES),
        ("panes", "starting_directory", MAX_DIRECTORY_BYTES),
        ("panes", "restoration_directory", MAX_DIRECTORY_BYTES),
        ("repositories", "directory", MAX_DIRECTORY_BYTES),
        ("worktrees", "directory", MAX_DIRECTORY_BYTES),
        ("enrollment_authorizations", "issuer", 160),
        ("pairing_receipts", "canonical_body", 1984),
        (
            "operation_entries",
            "compact_outcome",
            spaceterm_core::operation_ledger_contracts::MAX_COMPACT_OUTCOME_BYTES,
        ),
        ("operation_entries", "external_progress", 16 * 1024 * 1024),
        ("git_repository_locators", "main_root", MAX_DIRECTORY_BYTES),
        (
            "git_repository_locators",
            "common_directory",
            MAX_DIRECTORY_BYTES,
        ),
        (
            "git_allocation_reservations",
            "reservation",
            16 * 1024 * 1024,
        ),
        ("git_write_intents", "action", 16 * 1024),
        ("git_write_intents", "affected_scope", 16 * 1024 * 1024),
        ("git_write_intents", "external_result", 128),
        ("native_retirements", "descriptor", 16 * 1024 * 1024),
    ] {
        let invalid: u64 = sql.query_row(
            &format!("SELECT count(*) FROM {table} WHERE {column} IS NOT NULL AND (length(CAST({column} AS BLOB)) = 0 OR length(CAST({column} AS BLOB)) > ?1)"),
            [maximum as u64], |row| row.get(0),
        ).map_err(classify)?;
        if invalid != 0 {
            return Err(StoreFailure::InvalidDurableState);
        }
    }
    for table in ["store_metadata", "identity_high_water", "server_settings"] {
        if count(sql, table)? != 1 {
            return Err(StoreFailure::InvalidDurableState);
        }
    }
    let inventory = Inventory {
        workspaces: count(sql, "workspaces")?,
        tabs: count(sql, "tabs")?,
        panes: count(sql, "panes")?,
        repositories: count(sql, "repositories")?,
        worktrees: count(sql, "worktrees")?,
        endpoints: count(sql, "remote_bind_endpoints")?,
    };
    for (table, expected) in [
        ("workspace_field_versions", inventory.workspaces),
        ("workspace_name_bases", inventory.workspaces),
        ("tab_field_versions", inventory.tabs),
        ("pane_field_versions", inventory.panes),
        ("pane_generation_high_water", inventory.panes),
        ("tab_creation_replay", inventory.panes),
        ("repository_field_versions", inventory.repositories),
        ("worktree_inventory", inventory.worktrees),
    ] {
        if count(sql, table)? != expected {
            return Err(StoreFailure::InvalidDurableState);
        }
    }
    let settings_bytes: u64 = sql.query_row("SELECT length(CAST(worktree_location AS BLOB)) FROM server_settings WHERE singleton = 1", [], |row| row.get(0)).map_err(classify)?;
    if settings_bytes == 0 || settings_bytes > MAX_TEMPLATE_BYTES as u64 {
        return Err(StoreFailure::InvalidDurableState);
    }
    budget
        .charge(settings_bytes as usize)
        .map_err(|_| StoreFailure::InvalidDurableState)?;
    Ok(inventory)
}

fn rows<T>(
    sql: &Transaction<'_>,
    query: &str,
    count: u64,
    decode: impl Fn(&Row<'_>) -> rusqlite::Result<T>,
) -> Result<Vec<T>, StoreFailure> {
    let mut values = Vec::new();
    values
        .try_reserve_exact(usize::try_from(count).map_err(|_| StoreFailure::InvalidDurableState)?)
        .map_err(|_| StoreFailure::Unavailable)?;
    let mut key = [0; 8];
    for _ in 0..count {
        let (next, value) = sql
            .query_row(query, [key.as_slice()], |row| {
                Ok((fixed::<8>(row, 0)?, decode(row)?))
            })
            .map_err(classify)?;
        if next <= key {
            return Err(StoreFailure::InvalidDurableState);
        }
        key = next;
        values.push(value);
    }
    Ok(values)
}

fn replay(
    sql: &Transaction<'_>,
    tab: TabId,
    pane_count: u64,
) -> Result<TabCreationDefault, StoreFailure> {
    let key = tab.local().to_be_bytes();
    let count: u64 = sql
        .query_row(
            "SELECT count(*) FROM tab_creation_replay WHERE tab_id = ?1",
            [key.as_slice()],
            |row| row.get(0),
        )
        .map_err(classify)?;
    if count == 0 || count > pane_count || count > u64::from(u16::MAX) {
        return Err(StoreFailure::InvalidDurableState);
    }
    let mut order = Vec::new();
    order
        .try_reserve_exact(count as usize)
        .map_err(|_| StoreFailure::Unavailable)?;
    for position in 0..count {
        let pane = sql
            .query_row(
                "SELECT pane_id FROM tab_creation_replay WHERE tab_id = ?1 AND position = ?2",
                rusqlite::params![key.as_slice(), position.to_be_bytes().as_slice()],
                |row| checked(PaneId::new(tab.server(), counter(row, 0)?)),
            )
            .map_err(classify)?;
        order.push(pane);
    }
    TabCreationDefault::new(tab, order).map_err(|_| StoreFailure::InvalidDurableState)
}

fn remote(
    sql: &Transaction<'_>,
    server: ServerId,
    count: u64,
) -> Result<Versioned<RemoteAccessPolicy>, StoreFailure> {
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, SocketAddrV6};
    if count > spaceterm_core::remote_access::MAX_REMOTE_BIND_ENDPOINTS as u64 {
        return Err(StoreFailure::InvalidDurableState);
    }
    let mut endpoints = Vec::new();
    endpoints
        .try_reserve_exact(count as usize)
        .map_err(|_| StoreFailure::Unavailable)?;
    for position in 0..count {
        let address = sql.query_row("SELECT address_family, address, port, flowinfo, scope_id FROM remote_bind_endpoints WHERE position = ?1", [position.to_be_bytes().as_slice()], |row| {
            let port = row.get(2)?;
            match row.get::<_, i64>(0)? {
                4 if row.get::<_, u32>(3)? == 0 && row.get::<_, u32>(4)? == 0 => Ok(SocketAddr::new(IpAddr::V4(Ipv4Addr::from(fixed::<4>(row, 1)?)), port)),
                6 => Ok(SocketAddr::V6(SocketAddrV6::new(Ipv6Addr::from(fixed::<16>(row, 1)?), port, row.get(3)?, row.get(4)?))),
                _ => Err(rusqlite::Error::InvalidQuery),
            }
        }).map_err(classify)?;
        endpoints.push(address);
    }
    let version = sql
        .query_row(
            "SELECT remote_access_version FROM server_settings WHERE singleton = 1",
            [],
            |row| counter(row, 0),
        )
        .map_err(classify)?;
    let value = if endpoints.is_empty() {
        RemoteAccessPolicy::disabled(server)
    } else {
        RemoteAccessPolicy::enabled(server, endpoints)
            .map_err(|_| StoreFailure::InvalidDurableState)?
    };
    Ok(Versioned {
        value,
        version: FieldVersion::new(version),
    })
}

pub(super) fn load(
    sql: &Transaction<'_>,
    identity: StoreIdentity,
    limits: &StoreLimits,
    services: &StoreServices,
) -> Result<RecoveredState, StoreFailure> {
    schema::validate_transaction_inventory(sql).map_err(classify)?;
    let metadata = metadata(sql, identity)?;
    let mut budget = limits.recovery_budget();
    let inventory = preflight(sql, limits, &mut budget)?;
    let server = identity.server;
    let settings = sql.query_row("SELECT worktree_location, worktree_location_version FROM server_settings WHERE singleton = 1", [], |row| Ok(ServerSettingsRecord { worktree_location: Versioned { value: checked(WorktreeLocationTemplate::new(row.get_ref(0)?.as_str()?.to_owned()))?, version: FieldVersion::new(counter(row, 1)?) } })).map_err(classify)?;
    let workspaces = rows(
        sql,
        "SELECT w.id, w.creation_ordinal, w.name_mode, w.display_name, w.pinned_directory, w.root_tab, v.name_version, v.pin_version FROM workspaces w JOIN workspace_field_versions v ON v.workspace_id = w.id WHERE w.id > ?1 ORDER BY w.id LIMIT 1",
        inventory.workspaces,
        |row| {
            Ok(WorkspaceRecord {
                id: checked(WorkspaceId::new(server, counter(row, 0)?))?,
                creation_ordinal: CreationOrdinal::new(counter(row, 1)?),
                name: Versioned {
                    value: WorkspaceName {
                        mode: match row.get::<_, i64>(2)? {
                            0 => NameMode::Automatic,
                            1 => NameMode::Custom,
                            _ => return Err(rusqlite::Error::InvalidQuery),
                        },
                        display: name(row, 3)?,
                    },
                    version: FieldVersion::new(counter(row, 6)?),
                },
                pinned_directory: Versioned {
                    value: optional_directory(row, 4, server)?,
                    version: FieldVersion::new(counter(row, 7)?),
                },
                root_tab: checked(TabId::new(server, counter(row, 5)?))?,
            })
        },
    )?;
    let tabs = rows(
        sql,
        "SELECT id, workspace_id, worktree_id, root_pane, creation_ordinal FROM tabs WHERE id > ?1 ORDER BY id LIMIT 1",
        inventory.tabs,
        |row| {
            Ok(TabRecord {
                id: checked(TabId::new(server, counter(row, 0)?))?,
                workspace: checked(WorkspaceId::new(server, counter(row, 1)?))?,
                worktree: optional_id(row, 2)?
                    .map(|id| WorktreeId::new(server, id))
                    .transpose()
                    .map_err(|_| rusqlite::Error::InvalidQuery)?,
                root_pane: checked(PaneId::new(server, counter(row, 3)?))?,
                creation_ordinal: CreationOrdinal::new(counter(row, 4)?),
            })
        },
    )?;
    let panes = rows(
        sql,
        "SELECT p.id, p.tab_id, p.creation_ordinal, p.creation_kind, p.creation_source, p.split_axis, p.split_edge, p.starting_directory, p.restoration_directory, v.restoration_version FROM panes p JOIN pane_field_versions v ON v.pane_id = p.id WHERE p.id > ?1 ORDER BY p.id LIMIT 1",
        inventory.panes,
        |row| {
            let source = optional_id(row, 4)?;
            let axis: Option<i64> = row.get(5)?;
            let edge: Option<i64> = row.get(6)?;
            let creation = match (row.get::<_, i64>(3)?, source, axis, edge) {
                (0, None, None, None) => PaneCreation::Initial,
                (1, Some(id), Some(axis), Some(edge)) => PaneCreation::SplitOf {
                    source: checked(PaneId::new(server, id))?,
                    axis: match axis {
                        0 => SplitAxis::Horizontal,
                        1 => SplitAxis::Vertical,
                        _ => return Err(rusqlite::Error::InvalidQuery),
                    },
                    edge: match edge {
                        0 => SplitEdge::Before,
                        1 => SplitEdge::After,
                        _ => return Err(rusqlite::Error::InvalidQuery),
                    },
                },
                _ => return Err(rusqlite::Error::InvalidQuery),
            };
            Ok(PaneRecord {
                id: checked(PaneId::new(server, counter(row, 0)?))?,
                tab: checked(TabId::new(server, counter(row, 1)?))?,
                creation_ordinal: CreationOrdinal::new(counter(row, 2)?),
                creation,
                starting_directory: directory(row, 7, server)?,
                confirmed_restoration_directory: Versioned {
                    value: optional_directory(row, 8, server)?,
                    version: FieldVersion::new(counter(row, 9)?),
                },
            })
        },
    )?;
    let repositories = rows(
        sql,
        "SELECT id, directory FROM repositories WHERE id > ?1 ORDER BY id LIMIT 1",
        inventory.repositories,
        |row| {
            Ok(RepositoryRecord {
                id: checked(RepositoryId::new(server, counter(row, 0)?))?,
                directory: directory(row, 1, server)?,
            })
        },
    )?;
    let worktrees = rows(
        sql,
        "SELECT id, repository_id, directory, is_main, missing FROM worktrees WHERE id > ?1 ORDER BY id LIMIT 1",
        inventory.worktrees,
        |row| {
            Ok(WorktreeRecord {
                id: checked(WorktreeId::new(server, counter(row, 0)?))?,
                repository: checked(RepositoryId::new(server, counter(row, 1)?))?,
                directory: directory(row, 2, server)?,
                main: boolean(row, 3)?,
                missing: boolean(row, 4)?,
            })
        },
    )?;
    let workspace_names = rows(
        sql,
        "SELECT workspace_id, automatic_base, assigned_automatic FROM workspace_name_bases WHERE workspace_id > ?1 ORDER BY workspace_id LIMIT 1",
        inventory.workspaces,
        |row| {
            Ok(WorkspaceNameBasis::new(
                checked(WorkspaceId::new(server, counter(row, 0)?))?,
                name(row, 1)?,
                name(row, 2)?,
            ))
        },
    )?;
    let pane_generations = rows(
        sql,
        "SELECT pane_id, session_generation, control_generation FROM pane_generation_high_water WHERE pane_id > ?1 ORDER BY pane_id LIMIT 1",
        inventory.panes,
        |row| {
            Ok(PaneGenerationHighWater::new(
                checked(PaneId::new(server, counter(row, 0)?))?,
                SessionGeneration::new(counter(row, 1)?),
                ControlGeneration::new(counter(row, 2)?),
            ))
        },
    )?;
    let tab_membership_versions = rows(
        sql,
        "SELECT tab_id, membership_version FROM tab_field_versions WHERE tab_id > ?1 ORDER BY tab_id LIMIT 1",
        inventory.tabs,
        |row| {
            Ok(TabMembershipVersion::new(
                checked(TabId::new(server, counter(row, 0)?))?,
                FieldVersion::new(counter(row, 1)?),
            ))
        },
    )?;
    let repository_inventory_versions = rows(
        sql,
        "SELECT repository_id, inventory_version FROM repository_field_versions WHERE repository_id > ?1 ORDER BY repository_id LIMIT 1",
        inventory.repositories,
        |row| {
            Ok(RepositoryInventoryVersion::new(
                checked(RepositoryId::new(server, counter(row, 0)?))?,
                FieldVersion::new(counter(row, 1)?),
            ))
        },
    )?;
    let worktree_inventory = rows(
        sql,
        "SELECT worktree_id, listed, inventory_version FROM worktree_inventory WHERE worktree_id > ?1 ORDER BY worktree_id LIMIT 1",
        inventory.worktrees,
        |row| {
            Ok(WorktreeInventoryFacts::new(
                checked(WorktreeId::new(server, counter(row, 0)?))?,
                boolean(row, 1)?,
                FieldVersion::new(counter(row, 2)?),
            ))
        },
    )?;
    let mut creation_defaults = Vec::new();
    creation_defaults
        .try_reserve_exact(tabs.len())
        .map_err(|_| StoreFailure::Unavailable)?;
    for tab in &tabs {
        creation_defaults.push(replay(sql, tab.id, inventory.panes)?);
    }
    let retained = OrganizationRetainedFacts::new(OrganizationRetainedParts {
        server,
        workspace_names,
        creation_defaults,
        pane_generations,
        tab_membership_versions,
        repository_inventory_versions,
        worktree_inventory,
        remote_access: remote(sql, server, inventory.endpoints)?,
    })
    .map_err(|_| StoreFailure::InvalidDurableState)?;
    let mut recovered = RecoveredState {
        organization: OrganizationState {
            server,
            revision: metadata.organization_revision,
            settings,
            workspaces,
            tabs,
            panes,
            repositories,
            worktrees,
            retained: Some(retained),
        },
        store_revision: metadata.store_revision,
        last_epoch: metadata.epoch,
        high_water: high_water(sql)?,
        streams: Vec::new(),
        operations: Vec::new(),
        ledger: None,
        pairing: None,
        git_repositories: Vec::new(),
        git_intents: Vec::new(),
        git_allocations: Vec::new(),
        native_retirements: Vec::new(),
    };
    services
        .operations
        .recover(sql, &mut recovered, limits, &mut budget)?;
    recovered.validate_with(services.validator.as_ref())?;
    Ok(recovered)
}
