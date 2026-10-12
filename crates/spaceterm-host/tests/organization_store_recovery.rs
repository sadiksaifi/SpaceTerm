//! Bounded recovery tests with real authority reads and explicit domain fakes.

use spaceterm_core::authority::StoreRevision;
use spaceterm_core::identity::ServerId;
use spaceterm_core::storage::{
    DurabilityValidator, OperationWrite, OrganizationStore, RecoveredState, StoreFailure,
    TransactionParts,
};
use spaceterm_host::organization_store::{
    OperationProjection, SqliteOrganizationStore, StoreIdentity, StoreLimits, StoreServices,
    limits::{Collection, SizeBudget},
    migrations,
};
use spaceterm_host::startup::{AccountBuildKey, BuildIdentity};
use spaceterm_host::storage::{CommitDisposition, Database, StorageFailure, Transaction};
use std::os::unix::fs::PermissionsExt;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

fn identity() -> StoreIdentity {
    StoreIdentity {
        server: ServerId::new([11; 16]),
        account: AccountBuildKey {
            account_uid: 501,
            build_identity: BuildIdentity::Development,
        },
        server_public_key: [29; 32],
    }
}
fn fixture() -> (tempfile::TempDir, Database) {
    #[cfg(target_os = "macos")]
    let root = std::path::PathBuf::from("/private/tmp");
    #[cfg(target_os = "linux")]
    let root = std::path::PathBuf::from(std::env::var_os("HOME").unwrap());
    let directory = tempfile::tempdir_in(root).unwrap();
    std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let mut database = Database::open(directory.path(), migrations()).unwrap();
    let zero = 0_u64.to_be_bytes();
    let one = 1_u64.to_be_bytes();
    let two = 2_u64.to_be_bytes();
    let three = 3_u64.to_be_bytes();
    let uid = 501_u64.to_be_bytes();
    assert_eq!(database.transaction(|sql| {
        sql.execute("INSERT INTO store_metadata VALUES (1, 1398034258, 1, ?1, ?2, 2, ?3, ?4, ?4, ?4, 1, 0)", rusqlite::params![identity().server.bytes().as_slice(), uid.as_slice(), identity().server_public_key.as_slice(), zero.as_slice()])?;
        sql.execute("INSERT INTO identity_high_water VALUES (1, ?1, ?1, ?1, ?2, ?2, ?2, ?2, ?2, ?3)", rusqlite::params![one.as_slice(), zero.as_slice(), three.as_slice()])?;
        sql.execute("INSERT INTO server_settings VALUES (1, '~/worktrees/{branch}', ?1, ?1)", [zero.as_slice()])?;
        sql.execute("INSERT INTO workspaces VALUES (?1, ?1, 0, 'automatic assignment', NULL, ?1)", [one.as_slice()])?;
        sql.execute("INSERT INTO workspace_field_versions VALUES (?1, ?1, ?1)", [one.as_slice()])?;
        sql.execute("INSERT INTO workspace_name_bases VALUES (?1, 'automatic base', 'automatic assignment')", [one.as_slice()])?;
        sql.execute("INSERT INTO tabs VALUES (?1, ?1, NULL, ?1, ?2)", rusqlite::params![one.as_slice(), two.as_slice()])?;
        sql.execute("INSERT INTO tab_field_versions VALUES (?1, ?1)", [one.as_slice()])?;
        sql.execute("INSERT INTO panes VALUES (?1, ?1, ?2, 0, NULL, NULL, NULL, ?3, NULL)", rusqlite::params![one.as_slice(), three.as_slice(), b"/does-not-exist/retained".as_slice()])?;
        sql.execute("INSERT INTO pane_field_versions VALUES (?1, ?1)", [one.as_slice()])?;
        sql.execute("INSERT INTO pane_generation_high_water VALUES (?1, ?2, ?3)", rusqlite::params![one.as_slice(), 17_u64.to_be_bytes().as_slice(), 23_u64.to_be_bytes().as_slice()])?;
        sql.execute("INSERT INTO tab_creation_replay VALUES (?1, ?2, ?1)", rusqlite::params![one.as_slice(), zero.as_slice()])?;
        Ok(())
    }), CommitDisposition::Durable(()));
    (directory, database)
}

struct OrganizationFixture {
    reads: Arc<AtomicUsize>,
}
impl DurabilityValidator for OrganizationFixture {
    fn validate_transaction_projection(
        &self,
        _parts: &TransactionParts,
    ) -> Result<(), StoreFailure> {
        Err(StoreFailure::RecoveryRequired)
    }
    fn validate_recovery(&self, recovered: &RecoveredState) -> Result<(), StoreFailure> {
        recovered
            .organization
            .retained
            .as_ref()
            .ok_or(StoreFailure::InvalidDurableState)?
            .validate_against(&recovered.organization)
            .map_err(|_| StoreFailure::InvalidDurableState)
    }
}
impl OperationProjection for OrganizationFixture {
    fn preflight(
        &self,
        _writes: &[OperationWrite],
        _budget: &mut SizeBudget,
    ) -> Result<usize, StoreFailure> {
        Err(StoreFailure::RecoveryRequired)
    }
    fn apply(
        &self,
        _sql: &Transaction<'_>,
        _parts: &TransactionParts,
        _previous: &RecoveredState,
    ) -> Result<usize, StoreFailure> {
        Err(StoreFailure::RecoveryRequired)
    }
    fn recover(
        &self,
        sql: &Transaction<'_>,
        _recovered: &mut RecoveredState,
        _limits: &StoreLimits,
        _budget: &mut SizeBudget,
    ) -> Result<(), StoreFailure> {
        self.reads.fetch_add(1, Ordering::SeqCst);
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
            let count: u64 = sql
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
fn store(
    database: Database,
    expected: StoreIdentity,
    limits: StoreLimits,
    reads: Arc<AtomicUsize>,
) -> SqliteOrganizationStore {
    SqliteOrganizationStore::from_test_database(
        database,
        expected,
        limits,
        StoreServices::new(
            Box::new(OrganizationFixture {
                reads: reads.clone(),
            }),
            Box::new(OrganizationFixture { reads }),
        ),
    )
    .unwrap()
}
fn mutate(
    database: &mut Database,
    mutation: impl FnOnce(&Transaction<'_>) -> Result<(), StorageFailure>,
) {
    assert_eq!(
        database.transaction(mutation),
        CommitDisposition::Durable(())
    );
}

#[test]
fn recovery_collection_limit_and_plus_one_precede_domain_decoding() {
    for collection in [
        Collection::Workspaces,
        Collection::Tabs,
        Collection::Panes,
        Collection::CreationReplay,
    ] {
        let (directory, database) = fixture();
        let reads = Arc::new(AtomicUsize::new(0));
        let mut at_limit = store(
            database,
            identity(),
            StoreLimits::production()
                .with_test_ceiling(collection, 1)
                .unwrap(),
            reads.clone(),
        );
        assert_eq!(at_limit.recover().unwrap().organization.panes.len(), 1);
        assert_eq!(reads.load(Ordering::SeqCst), 1);
        drop(at_limit);
        reads.store(0, Ordering::SeqCst);
        let database = Database::open_existing(directory.path(), migrations()).unwrap();
        let mut over_limit = store(
            database,
            identity(),
            StoreLimits::production()
                .with_test_ceiling(collection, 0)
                .unwrap(),
            reads.clone(),
        );
        assert_eq!(
            over_limit.recover().unwrap_err(),
            StoreFailure::InvalidDurableState
        );
        assert_eq!(reads.load(Ordering::SeqCst), 0);
    }
}

#[test]
fn wrong_server_account_build_or_public_key_is_rejected() {
    let (directory, database) = fixture();
    drop(database);
    for expected in [
        StoreIdentity {
            server: ServerId::new([12; 16]),
            ..identity()
        },
        StoreIdentity {
            account: AccountBuildKey {
                account_uid: 502,
                ..identity().account
            },
            ..identity()
        },
        StoreIdentity {
            account: AccountBuildKey {
                build_identity: BuildIdentity::Release,
                ..identity().account
            },
            ..identity()
        },
        StoreIdentity {
            server_public_key: [30; 32],
            ..identity()
        },
    ] {
        let reads = Arc::new(AtomicUsize::new(0));
        let mut store = store(
            Database::open_existing(directory.path(), migrations()).unwrap(),
            expected,
            StoreLimits::production(),
            reads.clone(),
        );
        assert_eq!(store.recover().unwrap_err(), StoreFailure::IdentityMismatch);
        assert_eq!(reads.load(Ordering::SeqCst), 0);
    }
}

#[test]
fn high_water_below_retained_identity_is_rejected() {
    let (_directory, mut database) = fixture();
    mutate(&mut database, |sql| {
        sql.execute(
            "UPDATE identity_high_water SET pane = ?1",
            [0_u64.to_be_bytes().as_slice()],
        )?;
        Ok(())
    });
    let mut store = store(
        database,
        identity(),
        StoreLimits::production(),
        Arc::new(AtomicUsize::new(0)),
    );
    assert_eq!(
        store.recover().unwrap_err(),
        StoreFailure::InvalidDurableState
    );
}

#[test]
fn high_water_below_retained_creation_ordinal_is_rejected() {
    let (_directory, mut database) = fixture();
    mutate(&mut database, |sql| {
        sql.execute(
            "UPDATE identity_high_water SET creation_ordinal = ?1",
            [2_u64.to_be_bytes().as_slice()],
        )?;
        Ok(())
    });
    let mut store = store(
        database,
        identity(),
        StoreLimits::production(),
        Arc::new(AtomicUsize::new(0)),
    );
    assert_eq!(
        store.recover().unwrap_err(),
        StoreFailure::InvalidDurableState
    );
}

#[test]
fn missing_comparison_row_is_rejected_before_domain_decoding() {
    let (_directory, mut database) = fixture();
    mutate(&mut database, |sql| {
        sql.execute("DELETE FROM pane_generation_high_water", [])?;
        Ok(())
    });
    let reads = Arc::new(AtomicUsize::new(0));
    let mut store = store(
        database,
        identity(),
        StoreLimits::production(),
        reads.clone(),
    );
    assert_eq!(
        store.recover().unwrap_err(),
        StoreFailure::InvalidDurableState
    );
    assert_eq!(reads.load(Ordering::SeqCst), 0);
}

#[test]
fn topology_cycle_is_rejected_instead_of_repaired() {
    let (_directory, mut database) = fixture();
    mutate(&mut database, |sql| {
        sql.execute(
            "UPDATE identity_high_water SET pane = ?1, creation_ordinal = ?2",
            rusqlite::params![
                2_u64.to_be_bytes().as_slice(),
                4_u64.to_be_bytes().as_slice()
            ],
        )?;
        sql.execute("UPDATE panes SET creation_kind = 1, creation_source = ?1, split_axis = 0, split_edge = 1", [2_u64.to_be_bytes().as_slice()])?;
        sql.execute(
            "INSERT INTO panes VALUES (?1, ?2, ?3, 1, ?2, 0, 1, ?4, NULL)",
            rusqlite::params![
                2_u64.to_be_bytes().as_slice(),
                1_u64.to_be_bytes().as_slice(),
                4_u64.to_be_bytes().as_slice(),
                b"/cycle".as_slice()
            ],
        )?;
        sql.execute(
            "INSERT INTO pane_field_versions VALUES (?1, ?1)",
            [2_u64.to_be_bytes().as_slice()],
        )?;
        sql.execute(
            "INSERT INTO pane_generation_high_water VALUES (?1, ?1, ?1)",
            [2_u64.to_be_bytes().as_slice()],
        )?;
        sql.execute(
            "INSERT INTO tab_creation_replay VALUES (?1, ?1, ?2)",
            rusqlite::params![
                1_u64.to_be_bytes().as_slice(),
                2_u64.to_be_bytes().as_slice()
            ],
        )?;
        Ok(())
    });
    let mut store = store(
        database,
        identity(),
        StoreLimits::production(),
        Arc::new(AtomicUsize::new(0)),
    );
    assert_eq!(
        store.recover().unwrap_err(),
        StoreFailure::InvalidDurableState
    );
}

#[test]
fn replay_position_gap_is_rejected_without_collection_reordering() {
    let (_directory, mut database) = fixture();
    mutate(&mut database, |sql| {
        sql.execute(
            "UPDATE tab_creation_replay SET position = ?1",
            [1_u64.to_be_bytes().as_slice()],
        )?;
        Ok(())
    });
    let mut store = store(
        database,
        identity(),
        StoreLimits::production(),
        Arc::new(AtomicUsize::new(0)),
    );
    assert_eq!(
        store.recover().unwrap_err(),
        StoreFailure::InvalidDurableState
    );
}

#[test]
fn unsupported_record_encoding_is_preserved_without_reset() {
    let (directory, mut database) = fixture();
    mutate(&mut database, |sql| {
        sql.execute("UPDATE store_metadata SET encoding_version = 2", [])?;
        Ok(())
    });
    let reads = Arc::new(AtomicUsize::new(0));
    let mut store = store(
        database,
        identity(),
        StoreLimits::production(),
        reads.clone(),
    );
    assert_eq!(store.recover().unwrap_err(), StoreFailure::UnknownSchema);
    assert_eq!(reads.load(Ordering::SeqCst), 0);
    drop(store);
    let database = Database::open_existing(directory.path(), migrations()).unwrap();
    assert_eq!(
        database
            .query_row("SELECT encoding_version FROM store_metadata", [], |row| row
                .get::<_, u32>(0))
            .unwrap(),
        2
    );
}

#[test]
fn recovery_preserves_nonexistent_directory_and_numeric_runtime_comparisons() {
    let (_directory, database) = fixture();
    let mut store = store(
        database,
        identity(),
        StoreLimits::production(),
        Arc::new(AtomicUsize::new(0)),
    );
    let recovered = store.recover().unwrap();
    assert_eq!(
        recovered.organization.panes[0]
            .restoration_directory()
            .bytes(),
        b"/does-not-exist/retained"
    );
    let retained = recovered.organization.retained.unwrap();
    assert_eq!(retained.pane_generations()[0].session().get(), 17);
    assert_eq!(retained.pane_generations()[0].control().get(), 23);
    assert!(recovered.native_retirements.is_empty());
    assert_eq!(
        store.read_auxiliary(StoreRevision::new(1)).unwrap_err(),
        StoreFailure::RevisionConflict
    );
}

#[test]
fn auxiliary_repository_count_is_checked_before_domain_decode() {
    let (_directory, mut database) = fixture();
    mutate(&mut database, |sql| {
        for key in 1..=2_u64 {
            sql.execute(
                "INSERT INTO git_repository_locators VALUES (?1, ?2, ?2)",
                rusqlite::params![key.to_be_bytes().as_slice(), vec![b'x'; 4096]],
            )?;
        }
        // No ledger/intent codec runs: the count/byte preflight rejects first.
        Ok(())
    });
    // Each row is within the hard table ceiling. Lowering the repository count
    // proves preflight covers auxiliary collections before calling their decoder.
    let reads = Arc::new(AtomicUsize::new(0));
    let mut store = store(
        database,
        identity(),
        StoreLimits::production()
            .with_test_ceiling(Collection::Repositories, 1)
            .unwrap(),
        reads.clone(),
    );
    assert_eq!(
        store.recover().unwrap_err(),
        StoreFailure::InvalidDurableState
    );
    assert_eq!(reads.load(Ordering::SeqCst), 0);
}

#[test]
fn normalized_creation_replay_is_not_reconstructed_from_ordinals_or_root() {
    let (_directory, mut database) = fixture();
    mutate(&mut database, |sql| {
        sql.execute(
            "UPDATE identity_high_water SET pane = ?1, creation_ordinal = ?2",
            rusqlite::params![
                2_u64.to_be_bytes().as_slice(),
                4_u64.to_be_bytes().as_slice()
            ],
        )?;
        sql.execute("UPDATE panes SET creation_kind = 1, creation_source = ?1, split_axis = 0, split_edge = 1", [2_u64.to_be_bytes().as_slice()])?;
        sql.execute(
            "INSERT INTO panes VALUES (?1, ?2, ?3, 0, NULL, NULL, NULL, ?4, NULL)",
            rusqlite::params![
                2_u64.to_be_bytes().as_slice(),
                1_u64.to_be_bytes().as_slice(),
                4_u64.to_be_bytes().as_slice(),
                b"/normalized".as_slice()
            ],
        )?;
        sql.execute(
            "INSERT INTO pane_field_versions VALUES (?1, ?1)",
            [2_u64.to_be_bytes().as_slice()],
        )?;
        sql.execute(
            "INSERT INTO pane_generation_high_water VALUES (?1, ?1, ?1)",
            [2_u64.to_be_bytes().as_slice()],
        )?;
        sql.execute(
            "UPDATE tab_creation_replay SET pane_id = ?1",
            [2_u64.to_be_bytes().as_slice()],
        )?;
        sql.execute(
            "INSERT INTO tab_creation_replay VALUES (?1, ?1, ?1)",
            [1_u64.to_be_bytes().as_slice()],
        )?;
        Ok(())
    });
    let mut store = store(
        database,
        identity(),
        StoreLimits::production(),
        Arc::new(AtomicUsize::new(0)),
    );
    let recovered = store.recover().unwrap();
    assert_eq!(recovered.organization.tabs[0].root_pane.local(), 1);
    assert!(
        recovered.organization.panes[0].creation_ordinal
            < recovered.organization.panes[1].creation_ordinal
    );
    let order = recovered
        .organization
        .retained
        .as_ref()
        .unwrap()
        .creation_defaults()[0]
        .replay_order();
    assert_eq!(
        order.iter().map(|pane| pane.local()).collect::<Vec<_>>(),
        [2, 1]
    );
}
