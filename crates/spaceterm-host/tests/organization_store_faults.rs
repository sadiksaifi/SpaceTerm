//! Known rollback/busy tests use real authority transactions. VFS write/sync
//! uncertainty tests remain blocked by the missing feature-gated authority hooks;
//! these tests do not substitute a fake Indeterminate result.

use spaceterm_core::storage::*;
use spaceterm_host::organization_store::{
    bootstrap::*, limits::SizeBudget, testing::organization_only_services, *,
};
use spaceterm_host::startup::{AccountBuildKey, BuildIdentity};
use spaceterm_host::storage::{Database, Transaction};
use std::os::unix::fs::PermissionsExt;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

fn identity() -> StoreIdentity {
    StoreIdentity {
        server: spaceterm_core::identity::ServerId::new([11; 16]),
        account: AccountBuildKey {
            account_uid: 501,
            build_identity: BuildIdentity::Development,
        },
        server_public_key: [29; 32],
    }
}
fn fixture() -> (tempfile::TempDir, RecoveredState) {
    #[cfg(target_os = "macos")]
    let root = std::path::PathBuf::from("/private/tmp");
    #[cfg(target_os = "linux")]
    let root = std::path::PathBuf::from(std::env::var_os("HOME").unwrap());
    let directory = tempfile::tempdir_in(root).unwrap();
    std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let mut store = match SqliteOrganizationStore::initialize_test_fixture(
        Database::open(directory.path(), migrations()).unwrap(),
        NewStoreSeed {
            identity: identity(),
            origin: NewServerOrigin::LocalHeadless,
        },
        StoreLimits::production(),
        organization_only_services(),
    ) {
        InitializationDisposition::Durable { store, .. } => store,
        _ => panic!("fixture initialization failed"),
    };
    let state = store.recover().unwrap();
    (directory, state)
}
struct SqlFault {
    calls: Arc<AtomicUsize>,
}
impl DurabilityValidator for SqlFault {
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
    fn validate_recovery(&self, value: &RecoveredState) -> Result<(), StoreFailure> {
        value
            .organization
            .retained
            .as_ref()
            .ok_or(StoreFailure::InvalidDurableState)?
            .validate_against(&value.organization)
            .map_err(|_| StoreFailure::InvalidDurableState)
    }
}
impl OperationProjection for SqlFault {
    fn preflight(&self, _: &[OperationWrite], _: &mut SizeBudget) -> Result<usize, StoreFailure> {
        Ok(1)
    }
    fn apply(
        &self,
        sql: &Transaction<'_>,
        _: &TransactionParts,
        _: &RecoveredState,
    ) -> Result<usize, StoreFailure> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        // The adapter boundary injects a real statement error after one real row
        // change. The authority, not this fake, establishes rollback disposition.
        sql.execute(
            "UPDATE server_settings SET worktree_location = '~/sensitive-sentinel/{branch}'",
            [],
        )
        .map_err(|_| StoreFailure::InvalidTransaction)?;
        sql.execute("INSERT INTO absent_projection_table VALUES (1)", [])
            .map_err(|_| StoreFailure::InvalidTransaction)
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
fn fault_store(directory: &tempfile::TempDir, calls: Arc<AtomicUsize>) -> SqliteOrganizationStore {
    let services = StoreServices::new(
        Box::new(SqlFault {
            calls: calls.clone(),
        }),
        Box::new(SqlFault { calls }),
    );
    let mut store = SqliteOrganizationStore::from_test_database(
        Database::open_existing(directory.path(), migrations()).unwrap(),
        identity(),
        StoreLimits::production(),
        services,
    )
    .unwrap();
    store.recover().unwrap();
    store
}
fn proposal(before: &RecoveredState) -> DurableTransaction {
    DurableTransaction::new(TransactionParts {
        server: before.organization.server,
        expected_store_revision: before.store_revision,
        resulting_store_revision: before.store_revision.checked_successor().unwrap(),
        expected_organization_revision: before.organization.revision,
        resulting_organization_revision: before.organization.revision,
        expected_epoch: before.last_epoch,
        resulting_epoch: before.last_epoch,
        expected_high_water: before.high_water,
        resulting_high_water: before.high_water,
        organization_writes: vec![],
        operation_writes: vec![],
    })
    .unwrap()
}

#[test]
fn statement_failure_with_confirmed_rollback_leaves_all_tables_unchanged() {
    let (directory, before) = fixture();
    let calls = Arc::new(AtomicUsize::new(0));
    let mut store = fault_store(&directory, calls.clone());
    let result = store.commit(&proposal(&before));
    assert_eq!(
        result,
        CommitDisposition::NotApplied(StoreFailure::InvalidTransaction)
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert!(!format!("{result:?}").contains("sensitive-sentinel"));
    assert_eq!(store.recover().unwrap(), before);
    drop(store);
    let mut reopened = SqliteOrganizationStore::from_test_database(
        Database::open_existing(directory.path(), migrations()).unwrap(),
        identity(),
        StoreLimits::production(),
        organization_only_services(),
    )
    .unwrap();
    assert_eq!(reopened.recover().unwrap(), before);
}

#[test]
fn failure_before_transaction_is_not_applied_without_projection_effects() {
    let (directory, before) = fixture();
    let calls = Arc::new(AtomicUsize::new(0));
    let mut store = fault_store(&directory, calls.clone());
    let mut writer = Database::open_existing(directory.path(), migrations()).unwrap();
    assert_eq!(
        writer.transaction(|_| {
            assert_eq!(
                store.commit(&proposal(&before)),
                CommitDisposition::NotApplied(StoreFailure::Unavailable)
            );
            assert_eq!(calls.load(Ordering::SeqCst), 0);
            Ok(())
        }),
        spaceterm_host::storage::CommitDisposition::Durable(())
    );
    assert_eq!(store.recover().unwrap(), before);
}
