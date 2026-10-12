//! Relational projection tests plus real ledger/reducer composition regressions.
//! Organization-only tests reject every auxiliary row through their narrow adapter.

use spaceterm_core::authority::*;
use spaceterm_core::identity::*;
use spaceterm_core::organization::*;
use spaceterm_core::organization_retained::*;
use spaceterm_core::storage::*;
use spaceterm_host::organization_store::{
    OperationProjection, SqliteOrganizationStore, StoreIdentity, StoreLimits, StoreServices,
    limits::SizeBudget, migrations,
};
use spaceterm_host::startup::{AccountBuildKey, BuildIdentity};
use spaceterm_host::storage::{Database, Transaction};
use std::os::unix::fs::PermissionsExt;

pub struct OrganizationOnly;
impl DurabilityValidator for OrganizationOnly {
    fn validate_transaction_projection(
        &self,
        parts: &TransactionParts,
    ) -> Result<(), StoreFailure> {
        if parts.operation_writes.is_empty() {
            Ok(())
        } else {
            Err(StoreFailure::RecoveryRequired)
        }
    }
    fn validate_recovery(&self, recovered: &RecoveredState) -> Result<(), StoreFailure> {
        if recovered.ledger.is_some()
            || recovered.pairing.is_some()
            || !recovered.streams.is_empty()
            || !recovered.operations.is_empty()
            || !recovered.git_repositories.is_empty()
            || !recovered.git_intents.is_empty()
            || !recovered.git_allocations.is_empty()
            || !recovered.native_retirements.is_empty()
        {
            return Err(StoreFailure::RecoveryRequired);
        }
        recovered
            .organization
            .retained
            .as_ref()
            .ok_or(StoreFailure::InvalidDurableState)?
            .validate_against(&recovered.organization)
            .map_err(|_| StoreFailure::InvalidDurableState)
    }
}
impl OperationProjection for OrganizationOnly {
    fn preflight(
        &self,
        writes: &[OperationWrite],
        _budget: &mut SizeBudget,
    ) -> Result<usize, StoreFailure> {
        if writes.is_empty() {
            Ok(0)
        } else {
            Err(StoreFailure::RecoveryRequired)
        }
    }
    fn apply(
        &self,
        _sql: &Transaction<'_>,
        parts: &TransactionParts,
        _previous: &RecoveredState,
    ) -> Result<usize, StoreFailure> {
        if parts.operation_writes.is_empty() {
            Ok(0)
        } else {
            Err(StoreFailure::RecoveryRequired)
        }
    }
    fn recover(
        &self,
        sql: &Transaction<'_>,
        _recovered: &mut RecoveredState,
        _limits: &StoreLimits,
        _budget: &mut SizeBudget,
    ) -> Result<(), StoreFailure> {
        for table in [
            "pairings",
            "paired_clients",
            "enrollment_authorizations",
            "pairing_receipts",
            "revocation_receipts",
            "operation_streams",
            "operation_entries",
            "git_repository_locators",
            "git_allocation_reservations",
            "git_write_intents",
            "native_retirements",
        ] {
            let count: i64 = sql
                .query_row(&format!("SELECT count(*) FROM {table}"), [], |row| {
                    row.get(0)
                })
                .map_err(|_| StoreFailure::InvalidDurableState)?;
            if count != 0 {
                return Err(StoreFailure::RecoveryRequired);
            }
        }
        Ok(())
    }
}
pub fn services() -> StoreServices {
    StoreServices::new(Box::new(OrganizationOnly), Box::new(OrganizationOnly))
}
pub fn identity() -> StoreIdentity {
    StoreIdentity {
        server: ServerId::new([11; 16]),
        account: AccountBuildKey {
            account_uid: 501,
            build_identity: BuildIdentity::Development,
        },
        server_public_key: [29; 32],
    }
}
pub fn fixture() -> (tempfile::TempDir, Database) {
    #[cfg(target_os = "macos")]
    let root = std::path::PathBuf::from("/private/tmp");
    #[cfg(target_os = "linux")]
    let root = std::path::PathBuf::from(std::env::var_os("HOME").unwrap());
    let directory = tempfile::tempdir_in(root).unwrap();
    std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let mut database = Database::open(directory.path(), migrations()).unwrap();
    let stamp = 0_u64.to_be_bytes();
    let uid = u64::from(identity().account.account_uid).to_be_bytes();
    let seeded = database.transaction(|sql| {
        sql.execute(
            "INSERT INTO store_metadata VALUES (1, 1398034258, 1, ?1, ?2, 2, ?3, ?4, ?4, ?4, 1, 0)",
            rusqlite::params![
                identity().server.bytes().as_slice(),
                uid.as_slice(),
                identity().server_public_key.as_slice(),
                stamp.as_slice()
            ],
        )?;
        sql.execute(
            "INSERT INTO identity_high_water VALUES (1, ?1, ?1, ?1, ?1, ?1, ?1, ?1, ?1, ?1)",
            [stamp.as_slice()],
        )?;
        sql.execute(
            "INSERT INTO server_settings VALUES (1, '~/worktrees/{branch}', ?1, ?1)",
            [stamp.as_slice()],
        )?;
        Ok(())
    });
    assert_eq!(
        seeded,
        spaceterm_host::storage::CommitDisposition::Durable(())
    );
    (directory, database)
}
pub fn store(database: Database, limits: StoreLimits) -> SqliteOrganizationStore {
    SqliteOrganizationStore::from_test_database(database, identity(), limits, services()).unwrap()
}
pub fn create_parts(previous: &RecoveredState, local: u64) -> TransactionParts {
    let server = identity().server;
    let workspace = WorkspaceId::new(server, local).unwrap();
    let tab = TabId::new(server, local).unwrap();
    let pane = PaneId::new(server, local).unwrap();
    let ordinal = previous.high_water.creation_ordinal;
    let mut high = previous.high_water;
    high.workspace = local;
    high.tab = local;
    high.pane = local;
    high.creation_ordinal = ordinal + 3;
    TransactionParts {
        server,
        expected_store_revision: previous.store_revision,
        resulting_store_revision: previous.store_revision.checked_successor().unwrap(),
        expected_organization_revision: previous.organization.revision,
        resulting_organization_revision: previous
            .organization
            .revision
            .checked_successor()
            .unwrap(),
        expected_epoch: previous.last_epoch,
        resulting_epoch: previous.last_epoch,
        expected_high_water: previous.high_water,
        resulting_high_water: high,
        organization_writes: vec![
            OrganizationWrite::PutWorkspace(WorkspaceRecord {
                id: workspace,
                name: Versioned {
                    value: WorkspaceName {
                        mode: NameMode::Custom,
                        display: NameText::new("retained name".into()).unwrap(),
                    },
                    version: FieldVersion::new(3),
                },
                pinned_directory: Versioned {
                    value: Some(
                        ServerDirectory::new(server, b"/nonexistent/pin".to_vec()).unwrap(),
                    ),
                    version: FieldVersion::new(8),
                },
                root_tab: tab,
                creation_ordinal: CreationOrdinal::new(ordinal + 1),
            }),
            OrganizationWrite::PutTab(TabRecord {
                id: tab,
                workspace,
                worktree: None,
                root_pane: pane,
                creation_ordinal: CreationOrdinal::new(ordinal + 2),
            }),
            OrganizationWrite::PutPane(PaneRecord {
                id: pane,
                tab,
                creation_ordinal: CreationOrdinal::new(ordinal + 3),
                creation: PaneCreation::Initial,
                starting_directory: ServerDirectory::new(server, b"/nonexistent/start".to_vec())
                    .unwrap(),
                confirmed_restoration_directory: Versioned {
                    value: Some(
                        ServerDirectory::new(server, b"/nonexistent/confirmed".to_vec()).unwrap(),
                    ),
                    version: FieldVersion::new(9),
                },
            }),
            OrganizationWrite::Retained(RetainedOrganizationWrite::PutWorkspaceNameBasis(
                WorkspaceNameBasis::new(
                    workspace,
                    NameText::new("automatic base".into()).unwrap(),
                    NameText::new("automatic assignment".into()).unwrap(),
                ),
            )),
            OrganizationWrite::Retained(RetainedOrganizationWrite::PutCreationDefault(
                TabCreationDefault::new(tab, vec![pane]).unwrap(),
            )),
            OrganizationWrite::Retained(RetainedOrganizationWrite::SetTabMembershipVersion(
                TabMembershipVersion::new(tab, FieldVersion::new(4)),
            )),
            OrganizationWrite::Retained(RetainedOrganizationWrite::SetPaneGenerationHighWater(
                PaneGenerationHighWater::new(
                    pane,
                    SessionGeneration::new(17),
                    ControlGeneration::new(23),
                ),
            )),
        ],
        operation_writes: Vec::new(),
    }
}
pub fn proposal(parts: TransactionParts) -> DurableTransaction {
    DurableTransaction::new_with_validation(parts, &OrganizationOnly).unwrap()
}
pub fn unchanged(previous: &RecoveredState) -> TransactionParts {
    TransactionParts {
        server: previous.organization.server,
        expected_store_revision: previous.store_revision,
        resulting_store_revision: previous.store_revision.checked_successor().unwrap(),
        expected_organization_revision: previous.organization.revision,
        resulting_organization_revision: previous.organization.revision,
        expected_epoch: previous.last_epoch,
        resulting_epoch: previous.last_epoch,
        expected_high_water: previous.high_water,
        resulting_high_water: previous.high_water,
        organization_writes: Vec::new(),
        operation_writes: Vec::new(),
    }
}

#[test]
fn round_trip_preserves_names_pins_versions_and_creation_order() {
    let (directory, database) = fixture();
    let mut store = store(database, StoreLimits::production());
    let before = store.recover().unwrap();
    let transaction = proposal(create_parts(&before, 1));
    assert_eq!(
        store.commit(&transaction),
        CommitDisposition::Durable(CommitReceipt {
            store_revision: StoreRevision::new(1),
            organization_revision: OrganizationRevision::new(1)
        })
    );
    let expected = store.recover().unwrap();
    drop(store);
    let mut reopened = self::store(
        Database::open_existing(directory.path(), migrations()).unwrap(),
        StoreLimits::production(),
    );
    let recovered = reopened.recover().unwrap();
    assert_eq!(recovered, expected);
    assert_eq!(
        recovered.organization.workspaces[0]
            .name
            .value
            .display
            .as_str(),
        "retained name"
    );
    assert_eq!(
        recovered.organization.panes[0]
            .restoration_directory()
            .bytes(),
        b"/nonexistent/confirmed"
    );
    assert_eq!(
        recovered.organization.retained.unwrap().pane_generations()[0]
            .session()
            .get(),
        17
    );
}

#[test]
fn pane_directory_repair_clears_confirmation_and_survives_reopen() {
    let (directory, database) = fixture();
    let mut store = store(database, StoreLimits::production());
    let empty = store.recover().unwrap();
    assert!(matches!(
        store.commit(&proposal(create_parts(&empty, 1))),
        CommitDisposition::Durable(_)
    ));
    let before = store.recover().unwrap();
    assert!(
        before.organization.panes[0]
            .confirmed_restoration_directory
            .value
            .is_some()
    );
    let mut repaired = before.organization.panes[0].clone();
    repaired.starting_directory =
        ServerDirectory::new(identity().server, b"/replacement".to_vec()).unwrap();
    repaired.confirmed_restoration_directory = Versioned {
        value: None,
        version: FieldVersion::new(10),
    };
    let mut parts = unchanged(&before);
    parts.resulting_organization_revision = OrganizationRevision::new(2);
    parts
        .organization_writes
        .push(OrganizationWrite::PutPane(repaired.clone()));

    assert_eq!(
        store.commit(&proposal(parts)),
        CommitDisposition::Durable(CommitReceipt {
            store_revision: StoreRevision::new(2),
            organization_revision: OrganizationRevision::new(2),
        })
    );
    drop(store);
    let mut reopened = self::store(
        Database::open_existing(directory.path(), migrations()).unwrap(),
        StoreLimits::production(),
    );
    let recovered = reopened.recover().unwrap();
    let mut expected = before;
    expected.store_revision = StoreRevision::new(2);
    expected.organization.revision = OrganizationRevision::new(2);
    expected.organization.panes[0] = repaired;
    assert_eq!(recovered, expected);
    assert_eq!(
        recovered.organization.panes[0]
            .restoration_directory()
            .bytes(),
        b"/replacement"
    );
}

#[test]
fn pane_directory_repair_without_confirmation_survives_reopen() {
    let (directory, database) = fixture();
    let mut store = store(database, StoreLimits::production());
    let empty = store.recover().unwrap();
    let mut creation = create_parts(&empty, 1);
    for write in &mut creation.organization_writes {
        if let OrganizationWrite::PutPane(pane) = write {
            pane.confirmed_restoration_directory.value = None;
        }
    }
    assert!(matches!(
        store.commit(&proposal(creation)),
        CommitDisposition::Durable(_)
    ));
    let before = store.recover().unwrap();
    assert!(
        before.organization.panes[0]
            .confirmed_restoration_directory
            .value
            .is_none()
    );
    let mut repaired = before.organization.panes[0].clone();
    repaired.starting_directory =
        ServerDirectory::new(identity().server, b"/replacement".to_vec()).unwrap();
    repaired.confirmed_restoration_directory.version = FieldVersion::new(10);
    let mut parts = unchanged(&before);
    parts.resulting_organization_revision = OrganizationRevision::new(2);
    parts
        .organization_writes
        .push(OrganizationWrite::PutPane(repaired.clone()));

    assert_eq!(
        store.commit(&proposal(parts)),
        CommitDisposition::Durable(CommitReceipt {
            store_revision: StoreRevision::new(2),
            organization_revision: OrganizationRevision::new(2),
        })
    );
    drop(store);
    let mut reopened = self::store(
        Database::open_existing(directory.path(), migrations()).unwrap(),
        StoreLimits::production(),
    );
    let recovered = reopened.recover().unwrap();
    let mut expected = before;
    expected.store_revision = StoreRevision::new(2);
    expected.organization.revision = OrganizationRevision::new(2);
    expected.organization.panes[0] = repaired;
    assert_eq!(recovered, expected);
    assert_eq!(
        recovered.organization.panes[0]
            .restoration_directory()
            .bytes(),
        b"/replacement"
    );
}

#[test]
fn changing_starting_directory_without_restoration_version_is_rejected() {
    let (_directory, database) = fixture();
    let mut store = store(database, StoreLimits::production());
    let empty = store.recover().unwrap();
    assert!(matches!(
        store.commit(&proposal(create_parts(&empty, 1))),
        CommitDisposition::Durable(_)
    ));
    let before = store.recover().unwrap();
    let mut parts = unchanged(&before);
    parts.resulting_organization_revision =
        before.organization.revision.checked_successor().unwrap();
    let mut pane = before.organization.panes[0].clone();
    pane.starting_directory =
        ServerDirectory::new(identity().server, b"/replacement".to_vec()).unwrap();
    parts
        .organization_writes
        .push(OrganizationWrite::PutPane(pane));
    assert_eq!(
        store.commit(&proposal(parts)),
        CommitDisposition::NotApplied(StoreFailure::InvalidTransaction)
    );
    assert_eq!(store.recover().unwrap(), before);
}

#[test]
fn pane_directory_repair_without_confirmation_requires_exact_version_successor() {
    let (_directory, database) = fixture();
    let mut store = store(database, StoreLimits::production());
    let empty = store.recover().unwrap();
    let mut creation = create_parts(&empty, 1);
    for write in &mut creation.organization_writes {
        if let OrganizationWrite::PutPane(pane) = write {
            pane.confirmed_restoration_directory.value = None;
        }
    }
    assert!(matches!(
        store.commit(&proposal(creation)),
        CommitDisposition::Durable(_)
    ));
    let before = store.recover().unwrap();
    for version in [FieldVersion::new(9), FieldVersion::new(11)] {
        let mut repaired = before.organization.panes[0].clone();
        repaired.starting_directory =
            ServerDirectory::new(identity().server, b"/replacement".to_vec()).unwrap();
        repaired.confirmed_restoration_directory.version = version;
        let mut parts = unchanged(&before);
        parts.resulting_organization_revision = OrganizationRevision::new(2);
        parts
            .organization_writes
            .push(OrganizationWrite::PutPane(repaired));

        assert_eq!(
            store.commit(&proposal(parts)),
            CommitDisposition::NotApplied(StoreFailure::InvalidTransaction)
        );
        assert_eq!(store.recover().unwrap(), before);
    }
}

pub fn delete_parts(previous: &RecoveredState) -> TransactionParts {
    let workspace = previous.organization.workspaces[0].id;
    let tab = previous.organization.tabs[0].id;
    let pane = previous.organization.panes[0].id;
    let mut parts = unchanged(previous);
    parts.resulting_organization_revision =
        previous.organization.revision.checked_successor().unwrap();
    parts.organization_writes = vec![
        OrganizationWrite::DeleteWorkspace(workspace),
        OrganizationWrite::Retained(RetainedOrganizationWrite::DeleteWorkspaceNameBasis(
            workspace,
        )),
        OrganizationWrite::DeleteTab(tab),
        OrganizationWrite::Retained(RetainedOrganizationWrite::DeleteTabMembershipVersion(tab)),
        OrganizationWrite::Retained(RetainedOrganizationWrite::DeleteCreationDefault(tab)),
        OrganizationWrite::DeletePane(pane),
        OrganizationWrite::Retained(RetainedOrganizationWrite::DeletePaneGenerationHighWater(
            pane,
        )),
    ];
    parts
}

#[test]
fn organization_delete_keeps_allocation_marks_without_resurrection() {
    let (directory, database) = fixture();
    let mut store = store(database, StoreLimits::production());
    let empty = store.recover().unwrap();
    assert!(matches!(
        store.commit(&proposal(create_parts(&empty, 1))),
        CommitDisposition::Durable(_)
    ));
    let created = store.recover().unwrap();
    assert!(matches!(
        store.commit(&proposal(delete_parts(&created))),
        CommitDisposition::Durable(_)
    ));
    drop(store);
    let mut reopened = self::store(
        Database::open_existing(directory.path(), migrations()).unwrap(),
        StoreLimits::production(),
    );
    let deleted = reopened.recover().unwrap();
    assert!(deleted.organization.workspaces.is_empty());
    assert_eq!(deleted.high_water, created.high_water);
    assert_eq!(
        reopened.commit(&proposal(create_parts(&deleted, 1))),
        CommitDisposition::NotApplied(StoreFailure::InvalidTransaction)
    );
    assert!(matches!(
        reopened.commit(&proposal(create_parts(&deleted, 2))),
        CommitDisposition::Durable(_)
    ));
    assert_eq!(
        reopened.recover().unwrap().organization.workspaces[0]
            .id
            .local(),
        2
    );
}

#[test]
fn stale_revision_is_not_classified_as_corruption() {
    let (_directory, database) = fixture();
    let mut store = store(database, StoreLimits::production());
    let before = store.recover().unwrap();
    let transaction = proposal(create_parts(&before, 1));
    assert!(matches!(
        store.commit(&transaction),
        CommitDisposition::Durable(_)
    ));
    assert_eq!(
        store.commit(&transaction),
        CommitDisposition::NotApplied(StoreFailure::RevisionConflict)
    );
    let after = store.recover().unwrap();
    assert!(matches!(
        store.commit(&proposal(unchanged(&after))),
        CommitDisposition::Durable(_)
    ));
}

#[test]
fn two_connections_with_same_expected_revision_commit_at_most_once() {
    let (directory, database) = fixture();
    let mut first = store(database, StoreLimits::production());
    let mut second = store(
        Database::open_existing(directory.path(), migrations()).unwrap(),
        StoreLimits::production(),
    );
    let transaction = proposal(create_parts(&first.recover().unwrap(), 1));
    second.recover().unwrap();
    assert!(matches!(
        first.commit(&transaction),
        CommitDisposition::Durable(_)
    ));
    assert_eq!(
        second.commit(&transaction),
        CommitDisposition::NotApplied(StoreFailure::RevisionConflict)
    );
    assert_eq!(first.recover().unwrap(), second.recover().unwrap());
}

#[test]
fn awaiting_recovery_rejects_commit_and_auxiliary_read() {
    let (_directory, database) = fixture();
    let mut store = store(database, StoreLimits::production());
    assert_eq!(
        store.read_auxiliary(StoreRevision::new(0)).unwrap_err(),
        StoreFailure::RecoveryRequired
    );
    let before = store.recover().unwrap();
    drop(store);
    let (_another, database) = fixture();
    let mut unopened = self::store(database, StoreLimits::production());
    assert_eq!(
        unopened.commit(&proposal(create_parts(&before, 1))),
        CommitDisposition::NotApplied(StoreFailure::RecoveryRequired)
    );
}

#[test]
fn auxiliary_snapshot_requires_exact_revision() {
    let (_directory, database) = fixture();
    let mut store = store(database, StoreLimits::production());
    let before = store.recover().unwrap();
    assert!(matches!(
        store.commit(&proposal(unchanged(&before))),
        CommitDisposition::Durable(_)
    ));
    assert_eq!(
        store.read_auxiliary(before.store_revision).unwrap_err(),
        StoreFailure::RevisionConflict
    );
    let bundle = store.read_auxiliary(StoreRevision::new(1)).unwrap();
    assert_eq!(bundle.core, store.recover().unwrap());
}

#[test]
fn store_only_transaction_does_not_change_organization_revision() {
    let (_directory, database) = fixture();
    let mut store = store(database, StoreLimits::production());
    let before = store.recover().unwrap();
    let mut parts = unchanged(&before);
    parts.resulting_epoch = before.last_epoch.checked_successor().unwrap();
    assert_eq!(
        store.commit(&proposal(parts)),
        CommitDisposition::Durable(CommitReceipt {
            store_revision: StoreRevision::new(1),
            organization_revision: before.organization.revision
        })
    );
    let after = store.recover().unwrap();
    assert_eq!(after.organization, before.organization);
    assert_eq!(after.last_epoch.get(), 1);
}

#[test]
fn invalid_final_membership_rolls_back_every_projection() {
    let (_directory, database) = fixture();
    let mut store = store(database, StoreLimits::production());
    let before = store.recover().unwrap();
    let mut parts = create_parts(&before, 1);
    if let OrganizationWrite::PutTab(tab) = &mut parts.organization_writes[1] {
        tab.root_pane = PaneId::new(identity().server, 99).unwrap();
    }
    parts.resulting_high_water.pane = 99;
    assert_eq!(
        store.commit(&proposal(parts)),
        CommitDisposition::NotApplied(StoreFailure::InvalidTransaction)
    );
    assert_eq!(store.recover().unwrap(), before);
    assert!(matches!(
        store.commit(&proposal(create_parts(&before, 1))),
        CommitDisposition::Durable(_)
    ));
}

#[test]
fn missing_companion_projection_is_rejected_without_partial_rows() {
    let (_directory, database) = fixture();
    let mut store = store(database, StoreLimits::production());
    let before = store.recover().unwrap();
    let mut parts = create_parts(&before, 1);
    parts.organization_writes.pop();
    assert_eq!(
        store.commit(&proposal(parts)),
        CommitDisposition::NotApplied(StoreFailure::InvalidTransaction)
    );
    assert_eq!(store.recover().unwrap(), before);
}

#[test]
fn duplicate_logical_writes_are_rejected_before_sql() {
    let (_directory, database) = fixture();
    let mut store = store(database, StoreLimits::production());
    let before = store.recover().unwrap();
    let mut parts = create_parts(&before, 1);
    parts
        .organization_writes
        .push(parts.organization_writes[0].clone());
    assert_eq!(
        store.commit(&proposal(parts)),
        CommitDisposition::NotApplied(StoreFailure::InvalidTransaction)
    );
    assert_eq!(store.recover().unwrap(), before);
}

#[test]
fn field_and_generation_versions_cannot_move_backwards() {
    let (_directory, database) = fixture();
    let mut store = store(database, StoreLimits::production());
    let empty = store.recover().unwrap();
    assert!(matches!(
        store.commit(&proposal(create_parts(&empty, 1))),
        CommitDisposition::Durable(_)
    ));
    let before = store.recover().unwrap();
    for write in [
        {
            let mut workspace = before.organization.workspaces[0].clone();
            workspace.name.version = FieldVersion::new(2);
            OrganizationWrite::PutWorkspace(workspace)
        },
        OrganizationWrite::Retained(RetainedOrganizationWrite::SetPaneGenerationHighWater(
            PaneGenerationHighWater::new(
                before.organization.panes[0].id,
                SessionGeneration::new(16),
                ControlGeneration::new(23),
            ),
        )),
    ] {
        let mut parts = unchanged(&before);
        parts.resulting_organization_revision =
            before.organization.revision.checked_successor().unwrap();
        parts.organization_writes.push(write);
        assert_eq!(
            store.commit(&proposal(parts)),
            CommitDisposition::NotApplied(StoreFailure::InvalidTransaction)
        );
    }
    assert_eq!(store.recover().unwrap(), before);
}

#[test]
fn rename_requires_its_field_successor_and_preserves_other_versions() {
    let (_directory, database) = fixture();
    let mut store = store(database, StoreLimits::production());
    let empty = store.recover().unwrap();
    assert!(matches!(
        store.commit(&proposal(create_parts(&empty, 1))),
        CommitDisposition::Durable(_)
    ));
    let before = store.recover().unwrap();
    let mut workspace = before.organization.workspaces[0].clone();
    workspace.name.value.display = NameText::new("changed".into()).unwrap();
    let mut parts = unchanged(&before);
    parts.resulting_organization_revision =
        before.organization.revision.checked_successor().unwrap();
    parts
        .organization_writes
        .push(OrganizationWrite::PutWorkspace(workspace.clone()));
    assert_eq!(
        store.commit(&proposal(parts.clone())),
        CommitDisposition::NotApplied(StoreFailure::InvalidTransaction)
    );
    workspace.name.version = workspace.name.version.checked_successor().unwrap();
    parts.organization_writes[0] = OrganizationWrite::PutWorkspace(workspace.clone());
    assert!(matches!(
        store.commit(&proposal(parts)),
        CommitDisposition::Durable(_)
    ));
    let after = store.recover().unwrap();
    assert_eq!(after.organization.workspaces[0], workspace);
    assert_eq!(after.organization.tabs, before.organization.tabs);
    assert_eq!(after.organization.panes, before.organization.panes);
}

#[test]
fn full_width_identity_revision_and_generation_values_remain_exact() {
    let (_directory, mut database) = fixture();
    let high = (i64::MAX as u64 + 19).to_be_bytes();
    assert_eq!(database.transaction(|sql| {
        sql.execute("UPDATE identity_high_water SET workspace = ?1, tab = ?1, pane = ?1, creation_ordinal = ?1", [high.as_slice()])?;
        sql.execute("UPDATE store_metadata SET store_revision = ?1, organization_revision = ?1, last_epoch = ?1", [high.as_slice()])?;
        Ok(())
    }), spaceterm_host::storage::CommitDisposition::Durable(()));
    let mut store = store(database, StoreLimits::production());
    let before = store.recover().unwrap();
    let mut parts = create_parts(&before, u64::from_be_bytes(high) + 1);
    if let OrganizationWrite::Retained(RetainedOrganizationWrite::SetPaneGenerationHighWater(
        value,
    )) = &mut parts.organization_writes[6]
    {
        *value = PaneGenerationHighWater::new(
            value.pane(),
            SessionGeneration::new(u64::MAX),
            ControlGeneration::new(u64::MAX),
        );
    }
    assert!(matches!(
        store.commit(&proposal(parts)),
        CommitDisposition::Durable(_)
    ));
    let after = store.recover().unwrap();
    assert_eq!(after.store_revision.get(), u64::from_be_bytes(high) + 1);
    assert_eq!(
        after.organization.workspaces[0].id.local(),
        u64::from_be_bytes(high) + 1
    );
    assert_eq!(
        after.organization.retained.unwrap().pane_generations()[0]
            .control()
            .get(),
        u64::MAX
    );
}

#[test]
fn missing_unlisted_worktree_membership_survives_recovery() {
    let (directory, database) = fixture();
    let mut store = store(database, StoreLimits::production());
    let before = store.recover().unwrap();
    let mut parts = create_parts(&before, 1);
    let repository = RepositoryId::new(identity().server, 1).unwrap();
    let worktree = WorktreeId::new(identity().server, 1).unwrap();
    parts.resulting_high_water.repository = 1;
    parts.resulting_high_water.worktree = 1;
    if let OrganizationWrite::PutTab(tab) = &mut parts.organization_writes[1] {
        tab.worktree = Some(worktree);
    }
    parts.organization_writes.extend([
        OrganizationWrite::PutRepository(RepositoryRecord {
            id: repository,
            directory: ServerDirectory::new(identity().server, b"/missing/repository".to_vec())
                .unwrap(),
        }),
        OrganizationWrite::PutWorktree(WorktreeRecord {
            id: worktree,
            repository,
            directory: ServerDirectory::new(identity().server, b"/missing/worktree".to_vec())
                .unwrap(),
            main: false,
            missing: true,
        }),
        OrganizationWrite::Retained(RetainedOrganizationWrite::SetRepositoryInventoryVersion(
            RepositoryInventoryVersion::new(repository, FieldVersion::new(1)),
        )),
        OrganizationWrite::Retained(RetainedOrganizationWrite::SetWorktreeInventoryFacts(
            WorktreeInventoryFacts::new(worktree, false, FieldVersion::new(1)),
        )),
    ]);
    assert!(matches!(
        store.commit(&proposal(parts)),
        CommitDisposition::Durable(_)
    ));
    drop(store);
    let mut reopened = self::store(
        Database::open_existing(directory.path(), migrations()).unwrap(),
        StoreLimits::production(),
    );
    let recovered = reopened.recover().unwrap();
    assert_eq!(recovered.organization.tabs[0].worktree, Some(worktree));
    assert!(recovered.organization.worktrees[0].missing);
    assert!(
        !recovered
            .organization
            .retained
            .unwrap()
            .worktree_inventory()[0]
            .listed()
    );
}

#[test]
fn production_opening_does_not_borrow_test_fixture_authority() {
    let (directory, database) = fixture();
    assert_eq!(
        SqliteOrganizationStore::open_existing(
            database,
            identity(),
            StoreLimits::production(),
            services()
        )
        .unwrap_err(),
        StoreFailure::RecoveryRequired
    );
    let database = Database::open_existing(directory.path(), migrations()).unwrap();
    assert_eq!(
        SqliteOrganizationStore::open_existing_with_services(
            database,
            identity(),
            StoreLimits::production(),
            services()
        )
        .unwrap_err(),
        StoreFailure::RecoveryRequired
    );
    let database = Database::open_existing(directory.path(), migrations()).unwrap();
    let mut store = store(database, StoreLimits::production());
    assert_eq!(store.recover().unwrap().store_revision.get(), 0);
}

#[test]
fn native_failure_and_debug_output_retain_no_sensitive_payload() {
    let (_directory, database) = fixture();
    let mut store = store(database, StoreLimits::production());
    let before = store.recover().unwrap();
    let bundle = store.read_auxiliary(before.store_revision).unwrap();
    for debug in [
        format!("{store:?}"),
        format!("{bundle:?}"),
        format!("{:?}", identity()),
        format!("{:?}", services()),
        format!("{:?}", StoreFailure::InvalidTransaction),
    ] {
        assert!(!debug.contains("worktrees/{branch}"));
        assert!(!debug.contains("501"));
        assert!(!debug.contains("organization.sqlite3"));
    }
}

use spaceterm_core::worktrees::intent::{
    GitRepositoryMutation, RepositoryLocator, RepositoryLocatorParts,
};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

#[derive(Clone, Copy)]
enum CallbackResult {
    Success,
    RejectAfterWrite,
    PanicAfterWrite,
}
struct RepositoryFixture {
    result: CallbackResult,
    applies: Arc<AtomicUsize>,
}
impl DurabilityValidator for RepositoryFixture {
    fn validate_transaction_projection(
        &self,
        parts: &TransactionParts,
    ) -> Result<(), StoreFailure> {
        for write in &parts.operation_writes {
            match write {
                OperationWrite::GitRepository(GitRepositoryMutation::Put(value))
                    if value.scope().server == parts.server
                        && value.scope().repository.local()
                            <= parts.resulting_high_water.repository => {}
                _ => return Err(StoreFailure::InvalidTransaction),
            }
        }
        Ok(())
    }
    fn validate_recovery(&self, recovered: &RecoveredState) -> Result<(), StoreFailure> {
        if recovered.git_repositories.len() > 1
            || recovered.git_repositories.iter().any(|v| {
                v.scope().server != recovered.organization.server
                    || v.scope().repository.local() > recovered.high_water.repository
            })
        {
            return Err(StoreFailure::InvalidDurableState);
        }
        recovered
            .organization
            .retained
            .as_ref()
            .ok_or(StoreFailure::InvalidDurableState)?
            .validate_against(&recovered.organization)
            .map_err(|_| StoreFailure::InvalidDurableState)
    }
}
impl OperationProjection for RepositoryFixture {
    fn preflight(
        &self,
        writes: &[OperationWrite],
        budget: &mut SizeBudget,
    ) -> Result<usize, StoreFailure> {
        if writes.len() > 1 {
            return Err(StoreFailure::InvalidTransaction);
        }
        for write in writes {
            if let OperationWrite::GitRepository(GitRepositoryMutation::Put(value)) = write {
                budget
                    .charge(
                        value.parts().main_root.bytes().len()
                            + value.parts().common_directory.bytes().len()
                            + 256,
                    )
                    .map_err(|_| StoreFailure::InvalidTransaction)?;
            } else {
                return Err(StoreFailure::InvalidTransaction);
            }
        }
        Ok(writes.len())
    }
    fn apply(
        &self,
        sql: &Transaction<'_>,
        parts: &TransactionParts,
        _previous: &RecoveredState,
    ) -> Result<usize, StoreFailure> {
        self.applies.fetch_add(1, Ordering::SeqCst);
        for write in &parts.operation_writes {
            let OperationWrite::GitRepository(GitRepositoryMutation::Put(value)) = write else {
                return Err(StoreFailure::InvalidTransaction);
            };
            sql.execute(
                "INSERT INTO git_repository_locators VALUES (?1, ?2, ?3)",
                rusqlite::params![
                    value.scope().repository.local().to_be_bytes().as_slice(),
                    value.parts().main_root.bytes(),
                    value.parts().common_directory.bytes()
                ],
            )
            .map_err(|_| StoreFailure::InvalidTransaction)?;
        }
        match self.result {
            CallbackResult::Success => Ok(parts.operation_writes.len()),
            CallbackResult::RejectAfterWrite => Err(StoreFailure::InvalidTransaction),
            CallbackResult::PanicAfterWrite => panic!("fixture callback interrupted"),
        }
    }
    fn recover(
        &self,
        sql: &Transaction<'_>,
        recovered: &mut RecoveredState,
        _limits: &StoreLimits,
        _budget: &mut SizeBudget,
    ) -> Result<(), StoreFailure> {
        for table in [
            "pairings",
            "paired_clients",
            "enrollment_authorizations",
            "pairing_receipts",
            "revocation_receipts",
            "operation_streams",
            "operation_entries",
            "git_allocation_reservations",
            "git_write_intents",
            "native_retirements",
        ] {
            let count: u64 = sql
                .query_row(&format!("SELECT count(*) FROM {table}"), [], |row| {
                    row.get(0)
                })
                .map_err(|_| StoreFailure::InvalidDurableState)?;
            if count != 0 {
                return Err(StoreFailure::RecoveryRequired);
            }
        }
        let count: u64 = sql
            .query_row("SELECT count(*) FROM git_repository_locators", [], |row| {
                row.get(0)
            })
            .map_err(|_| StoreFailure::InvalidDurableState)?;
        if count > 1 {
            return Err(StoreFailure::InvalidDurableState);
        }
        if count == 1 {
            let (id, root, common): (Vec<u8>, Vec<u8>, Vec<u8>) = sql.query_row("SELECT repository_id, main_root, common_directory FROM git_repository_locators", [], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?))).map_err(|_| StoreFailure::InvalidDurableState)?;
            let server = recovered.organization.server;
            recovered.git_repositories.push(
                RepositoryLocator::new(RepositoryLocatorParts {
                    scope: spaceterm_core::operations::RepositoryScope {
                        server,
                        repository: RepositoryId::new(
                            server,
                            u64::from_be_bytes(
                                id.try_into()
                                    .map_err(|_| StoreFailure::InvalidDurableState)?,
                            ),
                        )
                        .map_err(|_| StoreFailure::InvalidDurableState)?,
                    },
                    main_root: ServerDirectory::new(server, root)
                        .map_err(|_| StoreFailure::InvalidDurableState)?,
                    common_directory: ServerDirectory::new(server, common)
                        .map_err(|_| StoreFailure::InvalidDurableState)?,
                })
                .map_err(|_| StoreFailure::InvalidDurableState)?,
            );
        }
        Ok(())
    }
}
fn repository_services(result: CallbackResult, applies: Arc<AtomicUsize>) -> StoreServices {
    StoreServices::new(
        Box::new(RepositoryFixture {
            result,
            applies: applies.clone(),
        }),
        Box::new(RepositoryFixture { result, applies }),
    )
}
fn locator_write() -> OperationWrite {
    let server = identity().server;
    OperationWrite::GitRepository(GitRepositoryMutation::Put(
        RepositoryLocator::new(RepositoryLocatorParts {
            scope: spaceterm_core::operations::RepositoryScope {
                server,
                repository: RepositoryId::new(server, 1).unwrap(),
            },
            main_root: ServerDirectory::new(server, b"/descriptive/root".to_vec()).unwrap(),
            common_directory: ServerDirectory::new(server, b"/descriptive/common".to_vec())
                .unwrap(),
        })
        .unwrap(),
    ))
}

#[test]
fn auxiliary_callback_and_organization_share_the_physical_commit() {
    for result in [CallbackResult::Success, CallbackResult::RejectAfterWrite] {
        let (directory, database) = fixture();
        let applies = Arc::new(AtomicUsize::new(0));
        let mut store = SqliteOrganizationStore::from_test_database(
            database,
            identity(),
            StoreLimits::production(),
            repository_services(result, applies.clone()),
        )
        .unwrap();
        let before = store.recover().unwrap();
        let mut parts = create_parts(&before, 1);
        parts.resulting_high_water.repository = 1;
        let transaction = DurableTransaction::new_with_validation(
            parts,
            &RepositoryFixture {
                result,
                applies: applies.clone(),
            },
        )
        .unwrap();
        let disposition = store.commit_with_auxiliary(&transaction, &[locator_write()]);
        match result {
            CallbackResult::Success => {
                assert!(matches!(disposition, CommitDisposition::Durable(_)))
            }
            CallbackResult::RejectAfterWrite => assert_eq!(
                disposition,
                CommitDisposition::NotApplied(StoreFailure::InvalidTransaction)
            ),
            CallbackResult::PanicAfterWrite => unreachable!(),
        }
        assert_eq!(applies.load(Ordering::SeqCst), 1);
        drop(store);
        let database = Database::open_existing(directory.path(), migrations()).unwrap();
        let mut reopened = SqliteOrganizationStore::from_test_database(
            database,
            identity(),
            StoreLimits::production(),
            repository_services(CallbackResult::Success, applies),
        )
        .unwrap();
        let recovered = reopened.recover().unwrap();
        match result {
            CallbackResult::Success => {
                assert_eq!(recovered.organization.workspaces.len(), 1);
                assert_eq!(recovered.git_repositories.len(), 1);
                assert_eq!(recovered.store_revision.get(), 1);
            }
            CallbackResult::RejectAfterWrite => assert_eq!(recovered, before),
            CallbackResult::PanicAfterWrite => unreachable!(),
        }
    }
}

#[test]
fn unwound_authority_callback_fences_same_handle_until_fresh_open() {
    let (directory, database) = fixture();
    let applies = Arc::new(AtomicUsize::new(0));
    let mut store = SqliteOrganizationStore::from_test_database(
        database,
        identity(),
        StoreLimits::production(),
        repository_services(CallbackResult::PanicAfterWrite, applies.clone()),
    )
    .unwrap();
    let before = store.recover().unwrap();
    let mut parts = create_parts(&before, 1);
    parts.resulting_high_water.repository = 1;
    let transaction = DurableTransaction::new_with_validation(
        parts,
        &RepositoryFixture {
            result: CallbackResult::Success,
            applies: applies.clone(),
        },
    )
    .unwrap();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        store.commit_with_auxiliary(&transaction, &[locator_write()])
    }));
    assert!(result.is_err());
    assert_eq!(store.recover().unwrap_err(), StoreFailure::RecoveryRequired);
    assert_eq!(
        store.commit(&transaction),
        CommitDisposition::NotApplied(StoreFailure::RecoveryRequired)
    );
    assert_eq!(applies.load(Ordering::SeqCst), 1);
    drop(store);
    let mut reopened = self::store(
        Database::open_existing(directory.path(), migrations()).unwrap(),
        StoreLimits::production(),
    );
    assert_eq!(reopened.recover().unwrap(), before);
}

#[test]
fn runtime_only_publication_revision_commits_without_retaining_live_authority() {
    let directory = organization_store_support::private_directory();
    let mut fixture = organization_store_support::ReducedFixture::new(directory.path());
    let (_, _, pane) = fixture.create();
    let before = fixture.fixture.store.recover().unwrap();
    let session = fixture.start(pane);
    let after = fixture.fixture.store.recover().unwrap();
    assert_eq!(
        after.organization.revision.get(),
        before.organization.revision.get() + 2
    );
    let generation = after.organization.retained.unwrap().pane_generations()[0];
    assert_eq!(generation.session(), session.generation);
    drop(fixture);
    let mut store = organization_store_support::reopen(directory.path());
    let state = store.recover().unwrap();
    let ledger = spaceterm_core::operation_ledger::model::OperationLedger::recover(
        organization_store_support::identity().server,
        state.ledger.unwrap(),
    )
    .unwrap();
    let recovered =
        organization_store_support::ReducedFixture::from_recovered(store, ledger, directory.path());
    let runtime = recovered.reducer.snapshot();
    assert_eq!(
        runtime.runtime()[0].control,
        spaceterm_core::organization::ControlRecord::Unclaimed
    );
    assert!(matches!(
        runtime.runtime()[0].phase,
        spaceterm_core::organization::SessionPhase::Ended { .. }
    ));
}

#[test]
fn remote_policy_keeps_exact_ipv4_ipv6_scope_and_field_version() {
    use spaceterm_core::remote_access::RemoteAccessPolicy;
    use std::net::{Ipv6Addr, SocketAddr, SocketAddrV6};
    let (directory, database) = fixture();
    let mut store = store(database, StoreLimits::production());
    let before = store.recover().unwrap();
    assert!(
        !before
            .organization
            .retained
            .as_ref()
            .unwrap()
            .remote_access()
            .value
            .is_enabled()
    );
    let endpoints = vec![
        "0.0.0.0:0".parse().unwrap(),
        SocketAddr::V6(SocketAddrV6::new(Ipv6Addr::LOCALHOST, 5200, 19, 23)),
    ];
    let policy = Versioned {
        value: RemoteAccessPolicy::enabled(identity().server, endpoints).unwrap(),
        version: FieldVersion::new(1),
    };
    let mut parts = unchanged(&before);
    parts.resulting_organization_revision =
        before.organization.revision.checked_successor().unwrap();
    parts.organization_writes.push(OrganizationWrite::Retained(
        RetainedOrganizationWrite::SetRemoteAccess(policy.clone()),
    ));
    assert!(matches!(
        store.commit(&proposal(parts)),
        CommitDisposition::Durable(_)
    ));
    drop(store);
    let mut reopened = self::store(
        Database::open_existing(directory.path(), migrations()).unwrap(),
        StoreLimits::production(),
    );
    assert_eq!(
        reopened
            .recover()
            .unwrap()
            .organization
            .retained
            .unwrap()
            .remote_access(),
        &policy
    );
}

#[expect(
    dead_code,
    reason = "The shared fixture also serves authority fault and crash suites."
)]
mod organization_store_support;

#[test]
fn session_lifecycle_admission_recovers_without_starting_effects() {
    use spaceterm_core::operation_ledger_contracts::{
        ExternalEffectKind, PendingKind, RecoveredOperationRecord,
    };
    let directory = organization_store_support::private_directory();
    let mut fixture = organization_store_support::Fixture::new(directory.path());
    let binding = fixture.binding;
    let operation = fixture.admit_command(
        binding,
        &spaceterm_core::operations::Command::EndAllSessions {
            server: organization_store_support::identity().server,
            assessment: spaceterm_core::operations::AssessmentRef {
                operation: fixture.operation(1),
                digest: spaceterm_core::operations::RequestDigest::from_bytes([8; 32]),
            },
        },
        PendingKind::External {
            effect: ExternalEffectKind::SessionLifecycle,
        },
    );
    drop(fixture);
    let recovered = organization_store_support::reopen(directory.path())
        .recover()
        .unwrap();
    assert!(
        matches!(&recovered.ledger.unwrap().operations[..], [RecoveredOperationRecord::Pending(v)] if v.binding == operation && v.kind == PendingKind::External { effect: ExternalEffectKind::SessionLifecycle })
    );
}

#[test]
fn storage_create_close_recover() {
    use organization_store_support::*;
    use spaceterm_core::operation_ledger::canonical::canonical_request_digest;
    use spaceterm_core::operation_ledger::model::{OperationLedger, TransitionResolution};
    use spaceterm_core::operation_ledger_contracts::*;
    use spaceterm_core::operations::*;
    use spaceterm_core::reducer::Resolution;
    let directory = private_directory();
    let mut fixture = ReducedFixture::new(directory.path());
    let binding = fixture.fixture.binding;
    let command = Command::CreateWorkspace {
        server: identity().server,
        source: WorkspaceCreationSource::ServerHome {
            server: identity().server,
        },
        name: NameDraft::Automatic,
    };
    let (workspace, _, pane) = fixture.create();
    let session = fixture.start(pane);
    let original = fixture.fixture.operation(1);
    let digest = canonical_request_digest(&binding, original, &command).unwrap();
    let original_result = fixture
        .fixture
        .ledger
        .lookup(&binding, original, Some(digest))
        .unwrap();
    let assessment = fixture.execute(
        Command::AssessClose {
            target: CloseTarget::Pane(pane),
        },
        PendingKind::AtomicMutation,
    );
    let Resolution::Completed(Outcome::Assessment(assessment)) = assessment.resolution else {
        panic!("assessment required");
    };
    let accepted = fixture.execute(
        Command::CloseConfirmed {
            target: CloseTarget::Pane(pane),
            assessment: assessment.reference(),
        },
        PendingKind::External {
            effect: ExternalEffectKind::SessionLifecycle,
        },
    );
    let Resolution::AwaitingRetirement(plan) = accepted.resolution else {
        panic!("pending retirement required");
    };
    fixture.event(
        spaceterm_core::reducer::OrganizationEvent::RetirementCleanup {
            batch: plan.descriptor.batch(),
            session,
            result: spaceterm_core::retirement::CleanupResult::Confirmed(
                spaceterm_core::retirement::ConfirmedCleanupClass::ReapedAndReleased,
            ),
        },
    );
    let complete = fixture
        .reducer
        .prepare_retirement_completion(plan.descriptor.batch())
        .unwrap();
    fixture.commit_reduction(complete, None);
    drop(fixture);
    let mut store = reopen(directory.path());
    let state = store.recover().unwrap();
    assert!(state.organization.workspaces.is_empty());
    let ledger = OperationLedger::recover(identity().server, state.ledger.unwrap()).unwrap();
    assert_eq!(
        ledger.lookup(&binding, original, Some(digest)).unwrap(),
        original_result
    );
    let mut fixture = ReducedFixture::from_recovered(store, ledger, directory.path());
    let (new_workspace, _, _) = fixture.create();
    assert!(new_workspace.local() > workspace.local());
    let context = fixture.fixture.context();
    let acknowledgement = fixture
        .fixture
        .ledger
        .prepare_acknowledgement(&binding, 3, context)
        .unwrap()
        .unwrap();
    let receipt = fixture.fixture.transaction(&acknowledgement, false);
    assert!(matches!(
        fixture.fixture.commit_proposal(acknowledgement, receipt),
        TransitionResolution::Acknowledged { through: 3 }
    ));
    drop(fixture);
    let state = reopen(directory.path()).recover().unwrap();
    let ledger = OperationLedger::recover(identity().server, state.ledger.unwrap()).unwrap();
    assert!(matches!(
        ledger.lookup(&binding, original, Some(digest)).unwrap(),
        OperationLookup::AcknowledgedRetired
    ));
}
