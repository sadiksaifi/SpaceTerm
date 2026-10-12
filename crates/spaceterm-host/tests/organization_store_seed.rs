//! Production bootstrap and opening against private authority-owned fixtures.

use spaceterm_core::storage::StoreFailure;
use spaceterm_host::organization_store::bootstrap::{
    InitializationDisposition, LegacyWorktreeLocation, NewServerOrigin, NewStoreSeed,
};
use spaceterm_host::organization_store::{SqliteOrganizationStore, StoreLimits, migrations};
use spaceterm_host::storage::Database;

mod support {
    use spaceterm_host::organization_store::{StoreIdentity, StoreServices};
    pub fn services() -> StoreServices {
        spaceterm_host::organization_store::testing::organization_only_services()
    }
    pub fn identity() -> StoreIdentity {
        StoreIdentity {
            server: spaceterm_core::identity::ServerId::new([11; 16]),
            account: spaceterm_host::startup::AccountBuildKey {
                account_uid: 501,
                build_identity: spaceterm_host::startup::BuildIdentity::Development,
            },
            server_public_key: [29; 32],
        }
    }
}

fn database() -> (tempfile::TempDir, Database) {
    #[cfg(target_os = "macos")]
    let root = std::path::PathBuf::from("/private/tmp");
    #[cfg(target_os = "linux")]
    let root = std::path::PathBuf::from(std::env::var_os("HOME").unwrap());
    let directory = tempfile::tempdir_in(root).unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let database = Database::open_for_application(
        directory.path(),
        migrations(),
        spaceterm_host::organization_store::schema::APPLICATION_ID,
    )
    .unwrap();
    (directory, database)
}

fn initialize(database: Database, origin: NewServerOrigin) -> SqliteOrganizationStore {
    match SqliteOrganizationStore::initialize_new(
        database,
        NewStoreSeed {
            identity: support::identity(),
            origin,
        },
        StoreLimits::production(),
        support::services(),
    ) {
        InitializationDisposition::Durable { mut store, receipt } => {
            let recovered = store.recover_bundle().unwrap().core;
            assert_eq!(receipt.store_revision, recovered.store_revision);
            assert_eq!(
                receipt.organization_revision,
                recovered.organization.revision
            );
            store
        }
        _ => panic!("fixture initialization failed"),
    }
}

#[test]
fn new_local_desktop_server_seeds_legacy_template_once() {
    let (directory, database) = database();
    let template = "/retained-legacy/{repository}/{branch}";
    let mut store = initialize(
        database,
        NewServerOrigin::LocalDesktop {
            legacy_worktree_location: Some(LegacyWorktreeLocation::parse(template.into()).unwrap()),
        },
    );
    assert_eq!(
        store
            .recover_bundle()
            .unwrap()
            .core
            .organization
            .settings
            .worktree_location
            .value
            .as_str(),
        template
    );
    drop(store);
    let reopened = Database::open_existing_for_application(
        directory.path(),
        migrations(),
        spaceterm_host::organization_store::schema::APPLICATION_ID,
    )
    .unwrap();
    let result = SqliteOrganizationStore::initialize_new(
        reopened,
        NewStoreSeed {
            identity: support::identity(),
            origin: NewServerOrigin::LocalHeadless,
        },
        StoreLimits::production(),
        support::services(),
    );
    assert!(matches!(
        result,
        InitializationDisposition::NotApplied(StoreFailure::RecoveryRequired)
    ));
    let database = Database::open_existing_for_application(
        directory.path(),
        migrations(),
        spaceterm_host::organization_store::schema::APPLICATION_ID,
    )
    .unwrap();
    let mut store = SqliteOrganizationStore::open_existing_with_services(
        database,
        support::identity(),
        StoreLimits::production(),
        support::services(),
    )
    .unwrap();
    assert_eq!(
        store
            .recover_bundle()
            .unwrap()
            .core
            .organization
            .settings
            .worktree_location
            .value
            .as_str(),
        template
    );
}

#[test]
fn new_headless_or_remote_server_uses_server_default() {
    for origin in [
        NewServerOrigin::LocalHeadless,
        NewServerOrigin::Remote,
        NewServerOrigin::LocalDesktop {
            legacy_worktree_location: None,
        },
    ] {
        let (_directory, database) = database();
        let mut store = initialize(database, origin);
        let recovered = store.recover_bundle().unwrap().core;
        assert_eq!(
            recovered
                .organization
                .settings
                .worktree_location
                .value
                .as_str(),
            spaceterm_host::git::path_template::DEFAULT_WORKTREE_PATH_TEMPLATE
        );
        assert!(
            recovered
                .organization
                .retained
                .unwrap()
                .remote_access()
                .value
                .endpoints()
                .is_empty()
        );
    }
}

#[test]
fn invalid_seed_is_rejected_before_initialization() {
    for value in [
        "",
        "relative/{branch}",
        "~/../{branch}",
        "~/{unknown}",
        "~/{repository}",
        "~/{branch}\0",
        "~/{branch}\n",
    ] {
        assert!(LegacyWorktreeLocation::parse(value.into()).is_err());
    }
    assert!(LegacyWorktreeLocation::parse(format!("/{}{{branch}}", "a".repeat(1024))).is_err());
    let sentinel = "/private-template-sentinel/{branch}";
    let value = LegacyWorktreeLocation::parse(sentinel.into()).unwrap();
    assert!(!format!("{value:?}").contains(sentinel));
}

#[test]
fn incomplete_initialization_does_not_replace_native_identity() {
    let (directory, database) = database();
    drop(database);
    let database = Database::open_existing_for_application(
        directory.path(),
        migrations(),
        spaceterm_host::organization_store::schema::APPLICATION_ID,
    )
    .unwrap();
    let result = SqliteOrganizationStore::initialize_new(
        database,
        NewStoreSeed {
            identity: support::identity(),
            origin: NewServerOrigin::LocalHeadless,
        },
        StoreLimits::production(),
        support::services(),
    );
    assert!(matches!(
        result,
        InitializationDisposition::NotApplied(StoreFailure::RecoveryRequired)
    ));
}

#[test]
fn existing_partial_inventory_does_not_authorize_initialization() {
    let (_directory, mut database) = database();
    // A retained row outside bootstrap invalidates the complete empty inventory.
    assert_eq!(
        database.transaction(|sql| {
            let zero = [0_u8; 8];
            sql.execute(
                "INSERT INTO identity_high_water VALUES (1, ?1, ?1, ?1, ?1, ?1, ?1, ?1, ?1, ?1)",
                [zero.as_slice()],
            )?;
            Ok(())
        }),
        spaceterm_host::storage::CommitDisposition::Durable(())
    );
    let result = SqliteOrganizationStore::initialize_new(
        database,
        NewStoreSeed {
            identity: support::identity(),
            origin: NewServerOrigin::LocalHeadless,
        },
        StoreLimits::production(),
        support::services(),
    );
    assert!(matches!(
        result,
        InitializationDisposition::NotApplied(StoreFailure::InvalidTransaction)
    ));
}

#[test]
fn desktop_discovery_cannot_seed_existing_headless_server() {
    let (directory, database) = database();
    let store = initialize(database, NewServerOrigin::LocalHeadless);
    drop(store);
    let result = SqliteOrganizationStore::initialize_new(
        Database::open_existing_for_application(
            directory.path(),
            migrations(),
            spaceterm_host::organization_store::schema::APPLICATION_ID,
        )
        .unwrap(),
        NewStoreSeed {
            identity: support::identity(),
            origin: NewServerOrigin::LocalDesktop {
                legacy_worktree_location: Some(
                    LegacyWorktreeLocation::parse("~/later-desktop/{branch}".into()).unwrap(),
                ),
            },
        },
        StoreLimits::production(),
        support::services(),
    );
    assert!(matches!(
        result,
        InitializationDisposition::NotApplied(StoreFailure::RecoveryRequired)
    ));
    let database = Database::open_existing_for_application(
        directory.path(),
        migrations(),
        spaceterm_host::organization_store::schema::APPLICATION_ID,
    )
    .unwrap();
    assert_eq!(
        database
            .query_row(
                "SELECT initialization_origin, legacy_seed_decision FROM store_metadata",
                [],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?))
            )
            .unwrap(),
        (1, 0)
    );
    let mut store = SqliteOrganizationStore::open_existing_with_services(
        database,
        support::identity(),
        StoreLimits::production(),
        support::services(),
    )
    .unwrap();
    assert_eq!(
        store
            .recover_bundle()
            .unwrap()
            .core
            .organization
            .settings
            .worktree_location
            .value
            .as_str(),
        spaceterm_host::git::path_template::DEFAULT_WORKTREE_PATH_TEMPLATE
    );
}

#[test]
fn seed_and_initialization_marker_are_atomic() {
    use spaceterm_core::storage::*;
    use spaceterm_host::organization_store::{
        OperationProjection, StoreServices, limits::SizeBudget,
    };
    use spaceterm_host::storage::Transaction;
    struct RejectResult;
    impl DurabilityValidator for RejectResult {
        fn validate_transaction_projection(
            &self,
            _: &TransactionParts,
        ) -> Result<(), StoreFailure> {
            Err(StoreFailure::InvalidTransaction)
        }
        fn validate_recovery(&self, _: &RecoveredState) -> Result<(), StoreFailure> {
            Err(StoreFailure::InvalidDurableState)
        }
    }
    impl OperationProjection for RejectResult {
        fn preflight(
            &self,
            _: &[OperationWrite],
            _: &mut SizeBudget,
        ) -> Result<usize, StoreFailure> {
            Ok(0)
        }
        fn apply(
            &self,
            _: &Transaction<'_>,
            _: &TransactionParts,
            _: &RecoveredState,
        ) -> Result<usize, StoreFailure> {
            Ok(0)
        }
        fn recover(
            &self,
            _: &Transaction<'_>,
            _: &mut RecoveredState,
            _: &StoreLimits,
            _: &mut SizeBudget,
        ) -> Result<(), StoreFailure> {
            Ok(())
        }
    }
    let (directory, database) = database();
    let result = SqliteOrganizationStore::initialize_new(
        database,
        NewStoreSeed {
            identity: support::identity(),
            origin: NewServerOrigin::LocalDesktop {
                legacy_worktree_location: Some(
                    LegacyWorktreeLocation::parse("~/rejected-seed/{branch}".into()).unwrap(),
                ),
            },
        },
        StoreLimits::production(),
        StoreServices::new(Box::new(RejectResult), Box::new(RejectResult)),
    );
    assert!(matches!(
        result,
        InitializationDisposition::NotApplied(StoreFailure::InvalidDurableState)
    ));
    let database = Database::open_existing_for_application(
        directory.path(),
        migrations(),
        spaceterm_host::organization_store::schema::APPLICATION_ID,
    )
    .unwrap();
    for table in spaceterm_host::organization_store::schema::TABLES {
        assert_eq!(
            database
                .query_row(&format!("SELECT count(*) FROM {table}"), [], |row| row
                    .get::<_, u64>(0))
                .unwrap(),
            0
        );
    }
}

#[test]
fn production_open_validate_recover_uses_real_authority_and_advances_epoch() {
    use spaceterm_core::storage::*;
    let (directory, database) = database();
    let mut store = initialize(database, NewServerOrigin::LocalHeadless);
    let before = store.recover_bundle().unwrap().core;
    drop(store);
    let mut store = SqliteOrganizationStore::open_existing_with_services(
        Database::open_existing_for_application(
            directory.path(),
            migrations(),
            spaceterm_host::organization_store::schema::APPLICATION_ID,
        )
        .unwrap(),
        support::identity(),
        StoreLimits::production(),
        support::services(),
    )
    .unwrap();
    assert_eq!(store.recover().unwrap(), before);
    let proposal = DurableTransaction::new(TransactionParts {
        server: before.organization.server,
        expected_store_revision: before.store_revision,
        resulting_store_revision: before.store_revision.checked_successor().unwrap(),
        expected_organization_revision: before.organization.revision,
        resulting_organization_revision: before.organization.revision,
        expected_epoch: before.last_epoch,
        resulting_epoch: before.last_epoch.checked_successor().unwrap(),
        expected_high_water: before.high_water,
        resulting_high_water: before.high_water,
        organization_writes: vec![],
        operation_writes: vec![],
    })
    .unwrap();
    assert!(matches!(
        store.commit(&proposal),
        CommitDisposition::Durable(_)
    ));
    drop(store);
    let mut store = SqliteOrganizationStore::open_existing_with_services(
        Database::open_existing_for_application(
            directory.path(),
            migrations(),
            spaceterm_host::organization_store::schema::APPLICATION_ID,
        )
        .unwrap(),
        support::identity(),
        StoreLimits::production(),
        support::services(),
    )
    .unwrap();
    let recovered = store.recover().unwrap();
    assert_eq!(recovered.last_epoch, proposal.parts().resulting_epoch);
    assert_eq!(
        recovered.store_revision,
        proposal.parts().resulting_store_revision
    );
}

#[test]
fn schema_v1_initialization_is_transactional() {
    use spaceterm_host::storage::testing::FaultPoint;
    for point in [
        FaultPoint::BeforeCommit,
        FaultPoint::Write,
        FaultPoint::Sync,
        FaultPoint::AfterCommit,
    ] {
        let (directory, database) = database();
        let control = database.test_control();
        control.fail_once(point, 0);
        let result = SqliteOrganizationStore::initialize_new(
            database,
            NewStoreSeed {
                identity: support::identity(),
                origin: NewServerOrigin::LocalDesktop {
                    legacy_worktree_location: Some(
                        LegacyWorktreeLocation::parse("~/atomic-seed/{branch}".into()).unwrap(),
                    ),
                },
            },
            StoreLimits::production(),
            support::services(),
        );
        if point == FaultPoint::BeforeCommit {
            assert!(matches!(
                result,
                InitializationDisposition::NotApplied(StoreFailure::Io)
            ));
        } else {
            assert!(matches!(result, InitializationDisposition::Indeterminate));
        }
        assert!(!control.fault_pending());
        let database = Database::open_existing_for_application(
            directory.path(),
            migrations(),
            spaceterm_host::organization_store::schema::APPLICATION_ID,
        )
        .unwrap();
        let metadata: u64 = database
            .query_row("SELECT count(*) FROM store_metadata", [], |r| r.get(0))
            .unwrap();
        for table in ["identity_high_water", "server_settings"] {
            assert_eq!(
                database
                    .query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r
                        .get::<_, u64>(0))
                    .unwrap(),
                metadata
            );
        }
        if point == FaultPoint::AfterCommit {
            assert_eq!(metadata, 1);
        }
        assert!(matches!(
            SqliteOrganizationStore::initialize_new(
                database,
                NewStoreSeed {
                    identity: support::identity(),
                    origin: NewServerOrigin::Remote
                },
                StoreLimits::production(),
                support::services()
            ),
            InitializationDisposition::NotApplied(StoreFailure::RecoveryRequired)
        ));
    }
}
