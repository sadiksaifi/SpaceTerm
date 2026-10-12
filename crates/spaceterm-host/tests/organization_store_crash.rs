//! Owned-process crash tests at the public authority callback and post-receipt
//! boundaries. These do not cover a VFS sync barrier or physical power loss.

use spaceterm_core::storage::*;
use spaceterm_host::organization_store::{
    bootstrap::*, limits::SizeBudget, testing::organization_only_services, *,
};
use spaceterm_host::startup::{AccountBuildKey, BuildIdentity};
use spaceterm_host::storage::{Database, Transaction};
use std::io::{Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const BARRIER: &[u8] = b"STORAGE-TEST-BARRIER\n";
const CHILD_STAGE: &str = "SPACETERM_STORAGE_TEST_STAGE";
const CHILD_DIRECTORY: &str = "SPACETERM_STORAGE_TEST_DIRECTORY";
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
fn barrier() -> ! {
    std::io::stdout().write_all(BARRIER).unwrap();
    std::io::stdout().flush().unwrap();
    loop {
        std::thread::sleep(Duration::from_millis(20));
    }
}
struct BeforeCommit;
impl DurabilityValidator for BeforeCommit {
    fn validate_transaction_projection(&self, _: &TransactionParts) -> Result<(), StoreFailure> {
        Err(StoreFailure::RecoveryRequired)
    }
    fn validate_recovery(&self, _: &RecoveredState) -> Result<(), StoreFailure> {
        // Initialization reaches validation after all three records are inserted
        // inside Database::transaction, before the authority commits.
        barrier()
    }
}
impl OperationProjection for BeforeCommit {
    fn preflight(&self, _: &[OperationWrite], _: &mut SizeBudget) -> Result<usize, StoreFailure> {
        Err(StoreFailure::RecoveryRequired)
    }
    fn apply(
        &self,
        _: &Transaction<'_>,
        _: &TransactionParts,
        _: &RecoveredState,
    ) -> Result<usize, StoreFailure> {
        Err(StoreFailure::RecoveryRequired)
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
fn child(stage: &str) -> ! {
    let directory = std::path::PathBuf::from(std::env::var_os(CHILD_DIRECTORY).unwrap());
    let database = Database::open_existing(&directory, migrations()).unwrap();
    let services = if stage == "before-commit" {
        StoreServices::new(Box::new(BeforeCommit), Box::new(BeforeCommit))
    } else {
        organization_only_services()
    };
    let result = SqliteOrganizationStore::initialize_test_fixture(
        database,
        NewStoreSeed {
            identity: identity(),
            origin: NewServerOrigin::LocalDesktop {
                legacy_worktree_location: Some(
                    LegacyWorktreeLocation::parse("~/retained-seed/{branch}".into()).unwrap(),
                ),
            },
        },
        StoreLimits::production(),
        services,
    );
    assert!(matches!(result, InitializationDisposition::Durable { .. }));
    // The durable authority receipt exists, but no simulated caller reply exists.
    barrier()
}
struct OwnedChild(Child);
impl Drop for OwnedChild {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline {
            if self.0.try_wait().ok().flatten().is_some() {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}
fn crash_at(test: &str, stage: &str) -> tempfile::TempDir {
    #[cfg(target_os = "macos")]
    let root = std::path::PathBuf::from("/private/tmp");
    #[cfg(target_os = "linux")]
    let root = std::path::PathBuf::from(std::env::var_os("HOME").unwrap());
    let directory = tempfile::tempdir_in(root).unwrap();
    std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    drop(Database::open(directory.path(), migrations()).unwrap());
    let mut owned = OwnedChild(
        Command::new(std::env::current_exe().unwrap())
            .args(["--exact", test, "--nocapture"])
            .env(CHILD_STAGE, stage)
            .env(CHILD_DIRECTORY, directory.path())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let mut output = owned.0.stdout.take().unwrap();
    let (send, receive) = std::sync::mpsc::channel();
    let reader = std::thread::spawn(move || {
        // Bounded output scan; neither paths nor retained records are printed.
        let mut seen = Vec::new();
        let mut byte = [0];
        for _ in 0..65_536 {
            if output.read_exact(&mut byte).is_err() {
                break;
            }
            seen.push(byte[0]);
            if seen.ends_with(BARRIER) {
                let _ = send.send(());
                return;
            }
        }
    });
    let reached = receive.recv_timeout(Duration::from_secs(30));
    owned.0.kill().unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if owned.0.try_wait().unwrap().is_some() {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "owned fixture child did not exit"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    reader.join().unwrap();
    assert!(
        reached.is_ok(),
        "owned fixture child did not reach storage barrier"
    );
    directory
}

#[test]
fn crash_before_initialization_commit_recovers_empty_complete_inventory() {
    if let Ok(stage) = std::env::var(CHILD_STAGE) {
        child(&stage);
    }
    let directory = crash_at(
        "crash_before_initialization_commit_recovers_empty_complete_inventory",
        "before-commit",
    );
    let database = Database::open_existing(directory.path(), migrations()).unwrap();
    for table in schema::TABLES {
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
fn crash_after_initialization_commit_before_reply_recovers_complete_seed() {
    if let Ok(stage) = std::env::var(CHILD_STAGE) {
        child(&stage);
    }
    let directory = crash_at(
        "crash_after_initialization_commit_before_reply_recovers_complete_seed",
        "after-commit",
    );
    let database = Database::open_existing(directory.path(), migrations()).unwrap();
    assert_eq!(
        database
            .query_row(
                "SELECT legacy_seed_decision FROM store_metadata",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        1
    );
    let mut store = SqliteOrganizationStore::from_test_database(
        database,
        identity(),
        StoreLimits::production(),
        organization_only_services(),
    )
    .unwrap();
    let recovered = store.recover().unwrap();
    assert_eq!(recovered.store_revision.get(), 1);
    assert_eq!(
        recovered
            .organization
            .settings
            .worktree_location
            .value
            .as_str(),
        "~/retained-seed/{branch}"
    );
    assert!(recovered.organization.workspaces.is_empty());
}
