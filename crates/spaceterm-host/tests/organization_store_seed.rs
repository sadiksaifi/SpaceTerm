//! Bootstrap tests use private real-authority fixtures, not native new-storage evidence.

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
    let database = Database::open(directory.path(), migrations()).unwrap();
    (directory, database)
}

fn initialize(database: Database, origin: NewServerOrigin) -> SqliteOrganizationStore {
    match SqliteOrganizationStore::initialize_test_fixture(
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
    let reopened = Database::open_existing(directory.path(), migrations()).unwrap();
    let result = SqliteOrganizationStore::initialize_test_fixture(
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
        InitializationDisposition::NotApplied(StoreFailure::InvalidTransaction)
    ));
    let database = Database::open_existing(directory.path(), migrations()).unwrap();
    let mut store = SqliteOrganizationStore::from_test_database(
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
fn production_initialization_requires_native_new_storage_evidence() {
    let (_directory, database) = database();
    let result = SqliteOrganizationStore::initialize_new(
        database,
        NewStoreSeed {
            identity: support::identity(),
            origin: NewServerOrigin::LocalHeadless,
        },
        StoreLimits::production(),
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
    let result = SqliteOrganizationStore::initialize_test_fixture(
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
    let result = SqliteOrganizationStore::initialize_test_fixture(
        Database::open_existing(directory.path(), migrations()).unwrap(),
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
        InitializationDisposition::NotApplied(StoreFailure::InvalidTransaction)
    ));
    let database = Database::open_existing(directory.path(), migrations()).unwrap();
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
    let mut store = SqliteOrganizationStore::from_test_database(
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
    let result = SqliteOrganizationStore::initialize_test_fixture(
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
    let database = Database::open_existing(directory.path(), migrations()).unwrap();
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
