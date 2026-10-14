#![cfg(any(target_os = "macos", target_os = "linux"))]

use spaceterm_host::storage::{
    CommitDisposition, DATABASE_FILE_NAME, Database, Migration, StorageFailure,
};
use std::fs::{self, OpenOptions};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt, symlink};
use std::path::{Path, PathBuf};

const APPLICATION: u32 = 0x5354_4f52;
const MIGRATIONS: &[Migration<'_>] = &[Migration {
    sql: "CREATE TABLE items(id INTEGER PRIMARY KEY); INSERT INTO items VALUES(7);",
}];

fn fixture() -> tempfile::TempDir {
    #[cfg(target_os = "macos")]
    let root = PathBuf::from("/private/tmp");
    #[cfg(target_os = "linux")]
    let root = PathBuf::from(std::env::var_os("HOME").unwrap());
    let directory = tempfile::tempdir_in(root).unwrap();
    fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o700)).unwrap();
    directory
}

fn initialized() -> tempfile::TempDir {
    let directory = fixture();
    drop(Database::open_for_application(directory.path(), MIGRATIONS, APPLICATION).unwrap());
    directory
}

fn open(directory: &Path) -> Result<Database, StorageFailure> {
    Database::open_frozen_for_application(directory, MIGRATIONS, APPLICATION)
}

// Access times may change on reads. Inode, content, permissions and write/change
// timestamps must not change, including when opening fails or the handle closes.
#[derive(Debug, Eq, PartialEq)]
struct RetainedFile {
    name: PathBuf,
    device: u64,
    inode: u64,
    mode: u32,
    modified: (i64, i64),
    changed: (i64, i64),
    bytes: Vec<u8>,
}

fn retained(directory: &Path) -> Vec<RetainedFile> {
    let mut files: Vec<_> = fs::read_dir(directory)
        .unwrap()
        .map(|entry| {
            let entry = entry.unwrap();
            let metadata = fs::symlink_metadata(entry.path()).unwrap();
            RetainedFile {
                name: entry.file_name().into(),
                device: metadata.dev(),
                inode: metadata.ino(),
                mode: metadata.mode(),
                modified: (metadata.mtime(), metadata.mtime_nsec()),
                changed: (metadata.ctime(), metadata.ctime_nsec()),
                bytes: fs::read(entry.path()).unwrap(),
            }
        })
        .collect();
    files.sort_by(|left, right| left.name.cmp(&right.name));
    files
}

#[test]
fn frozen_open_reads_exact_application_schema_but_cannot_write() {
    let directory = initialized();
    let before = retained(directory.path());
    let mut database = open(directory.path()).unwrap();
    assert_eq!(database.schema_version(), Ok(1));
    assert_eq!(database.pre_migration_application_id(), Some(APPLICATION));
    assert_eq!(database.verify_application(APPLICATION), Ok(()));
    assert_eq!(database.is_new_storage(), Ok(false));
    assert_eq!(
        database.query_row("SELECT id FROM items", [], |row| row.get::<_, u64>(0)),
        Ok(7)
    );
    assert_eq!(
        database.transaction(|_| panic!("frozen handles must not enter write callbacks")),
        CommitDisposition::<()>::NotApplied(StorageFailure::Unavailable)
    );
    assert_eq!(
        database.read_transaction(|sql| sql.execute("INSERT INTO items VALUES(8)", [])),
        Err(StorageFailure::InvalidState)
    );
    assert_eq!(
        database.read_transaction(
            |sql| sql.query_row("SELECT id FROM items", [], |row| row.get::<_, u64>(0))
        ),
        Ok(7)
    );
    drop(database);
    assert_eq!(retained(directory.path()), before);
}

#[test]
fn frozen_open_never_creates_a_missing_database() {
    let directory = fixture();
    assert_eq!(
        open(directory.path()).unwrap_err(),
        StorageFailure::Unavailable
    );
    assert_eq!(fs::read_dir(directory.path()).unwrap().count(), 0);
}

#[test]
fn frozen_open_rejects_old_and_future_schemas_without_running_migrations() {
    let directory = initialized();
    let before = retained(directory.path());
    let future = [
        MIGRATIONS[0],
        Migration {
            sql: "CREATE TABLE migration_must_not_run(id INTEGER);",
        },
    ];
    assert_eq!(
        Database::open_frozen_for_application(directory.path(), &future, APPLICATION).unwrap_err(),
        StorageFailure::RecoveryRequired
    );
    assert_eq!(retained(directory.path()), before);
    assert_eq!(
        Database::open_frozen_for_application(directory.path(), &[], APPLICATION).unwrap_err(),
        StorageFailure::UnknownSchema {
            found: 1,
            supported: 0
        }
    );
    assert_eq!(retained(directory.path()), before);
}

#[test]
fn frozen_open_does_not_adopt_empty_or_foreign_application_files() {
    let directory = initialized();
    let before = retained(directory.path());
    assert_eq!(
        Database::open_frozen_for_application(directory.path(), MIGRATIONS, APPLICATION + 1)
            .unwrap_err(),
        StorageFailure::InvalidState
    );
    assert_eq!(retained(directory.path()), before);

    let empty = fixture();
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(empty.path().join(DATABASE_FILE_NAME))
        .unwrap();
    let before = retained(empty.path());
    assert_eq!(
        open(empty.path()).unwrap_err(),
        StorageFailure::RecoveryRequired
    );
    assert_eq!(retained(empty.path()), before);
}

#[test]
fn frozen_open_rejects_malformed_sqlite_and_foreign_key_corruption_without_repair() {
    let malformed = fixture();
    let file = malformed.path().join(DATABASE_FILE_NAME);
    fs::write(&file, b"not a SQLite database").unwrap();
    fs::set_permissions(&file, fs::Permissions::from_mode(0o600)).unwrap();
    let before = retained(malformed.path());
    assert_eq!(
        open(malformed.path()).unwrap_err(),
        StorageFailure::InvalidState
    );
    assert_eq!(retained(malformed.path()), before);

    let corrupt = fixture();
    let connection = rusqlite::Connection::open(corrupt.path().join(DATABASE_FILE_NAME)).unwrap();
    connection
        .execute_batch(
            "PRAGMA foreign_keys=OFF; PRAGMA application_id=1398034258; PRAGMA user_version=1; \
         CREATE TABLE parent(id INTEGER PRIMARY KEY); \
         CREATE TABLE child(parent_id INTEGER REFERENCES parent(id)); \
         INSERT INTO child VALUES(99);",
        )
        .unwrap();
    drop(connection);
    fs::set_permissions(
        corrupt.path().join(DATABASE_FILE_NAME),
        fs::Permissions::from_mode(0o600),
    )
    .unwrap();
    let before = retained(corrupt.path());
    assert_eq!(
        open(corrupt.path()).unwrap_err(),
        StorageFailure::InvalidState
    );
    assert_eq!(retained(corrupt.path()), before);
}

#[test]
fn frozen_open_never_recovers_or_removes_retained_sidecars() {
    for (suffix, failure) in [
        ("-journal", StorageFailure::RecoveryRequired),
        ("-wal", StorageFailure::InvalidState),
        ("-shm", StorageFailure::InvalidState),
    ] {
        let directory = initialized();
        let sidecar = directory
            .path()
            .join(format!("{DATABASE_FILE_NAME}{suffix}"));
        fs::write(&sidecar, b"retained unclean state").unwrap();
        fs::set_permissions(&sidecar, fs::Permissions::from_mode(0o600)).unwrap();
        let before = retained(directory.path());
        assert_eq!(open(directory.path()).unwrap_err(), failure);
        assert_eq!(retained(directory.path()), before);
    }
}

#[test]
fn frozen_open_does_not_convert_wal_header_without_sidecars() {
    let directory = initialized();
    let connection = rusqlite::Connection::open(directory.path().join(DATABASE_FILE_NAME)).unwrap();
    let mode: String = connection
        .query_row("PRAGMA journal_mode=WAL", [], |row| row.get(0))
        .unwrap();
    assert_eq!(mode, "wal");
    drop(connection);
    let before = retained(directory.path());
    // SQLite requires WAL support before it can read this header's schema
    // version. The private VFS has no shared-memory authority, so opening fails
    // unavailable rather than converting the database to rollback mode.
    assert_eq!(
        open(directory.path()).unwrap_err(),
        StorageFailure::Unavailable
    );
    assert_eq!(retained(directory.path()), before);
}

#[test]
fn frozen_open_preserves_private_directory_file_and_link_checks() {
    let directory = initialized();
    let database = directory.path().join(DATABASE_FILE_NAME);
    fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o750)).unwrap();
    assert_eq!(
        open(directory.path()).unwrap_err(),
        StorageFailure::UnsafeStorage
    );
    fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o700)).unwrap();
    fs::set_permissions(&database, fs::Permissions::from_mode(0o640)).unwrap();
    assert_eq!(
        open(directory.path()).unwrap_err(),
        StorageFailure::UnsafeStorage
    );
    fs::set_permissions(&database, fs::Permissions::from_mode(0o600)).unwrap();
    let alias = directory.path().join("alias");
    fs::hard_link(&database, &alias).unwrap();
    assert_eq!(
        open(directory.path()).unwrap_err(),
        StorageFailure::UnsafeStorage
    );
    fs::remove_file(&alias).unwrap();
    fs::rename(&database, &alias).unwrap();
    symlink("alias", &database).unwrap();
    assert_eq!(
        open(directory.path()).unwrap_err(),
        StorageFailure::UnsafeStorage
    );
}

#[test]
fn frozen_handles_recheck_sidecars_and_refuse_descriptor_access_upgrades() {
    let directory = initialized();
    let first = open(directory.path()).unwrap();
    let second = open(directory.path()).unwrap();
    assert_eq!(
        Database::open_existing_for_application(directory.path(), MIGRATIONS, APPLICATION)
            .unwrap_err(),
        StorageFailure::Unavailable
    );
    let journal = directory
        .path()
        .join(format!("{DATABASE_FILE_NAME}-journal"));
    fs::write(&journal, b"unclean").unwrap();
    fs::set_permissions(&journal, fs::Permissions::from_mode(0o600)).unwrap();
    let before = retained(directory.path());
    assert_eq!(
        first.query_row("SELECT id FROM items", [], |row| row.get::<_, u64>(0)),
        Err(StorageFailure::RecoveryRequired)
    );
    assert_eq!(
        second.verify_authority(),
        Err(StorageFailure::RecoveryRequired)
    );
    drop(first);
    drop(second);
    assert_eq!(retained(directory.path()), before);
}

use spaceterm_core::{
    identity::ServerId,
    organization::ServerDirectory,
    storage::{OrganizationStore, StoreFailure},
};
use spaceterm_host::{
    organization_store::{
        SqliteOrganizationStore, StoreIdentity, StoreLimits, StoreServices,
        bootstrap::{InitializationDisposition, NewServerOrigin, NewStoreSeed},
        pairing::SignedReceiptReader,
        schema,
    },
    startup::{AccountBuildKey, BuildIdentity},
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

fn services() -> StoreServices {
    StoreServices::production(
        ServerDirectory::new(identity().server, b"/fixture-home".to_vec()).unwrap(),
        std::sync::Arc::new(
            SignedReceiptReader::new(spaceterm_protocol::authentication::ServerPin {
                identity: identity().server.bytes(),
                public_key: identity().server_public_key,
            })
            .unwrap(),
        ),
    )
}

fn canonical_fixture() -> (tempfile::TempDir, SqliteOrganizationStore) {
    let directory = fixture();
    let database = Database::open_for_application(
        directory.path(),
        schema::migrations(),
        schema::APPLICATION_ID,
    )
    .unwrap();
    let InitializationDisposition::Durable { store, .. } = SqliteOrganizationStore::initialize_new(
        database,
        NewStoreSeed {
            identity: identity(),
            origin: NewServerOrigin::LocalHeadless,
        },
        StoreLimits::production(),
        services(),
    ) else {
        panic!("fixture initialization failed");
    };
    (directory, store)
}

fn canonical_open(
    directory: &Path,
    expected: StoreIdentity,
) -> Result<SqliteOrganizationStore, StoreFailure> {
    SqliteOrganizationStore::open_existing(
        Database::open_frozen_for_application(
            directory,
            schema::migrations(),
            schema::APPLICATION_ID,
        )
        .unwrap(),
        expected,
        StoreLimits::production(),
        services(),
    )
}

#[test]
fn frozen_database_reuses_complete_production_organization_recovery_without_writes() {
    let (directory, mut store) = canonical_fixture();
    let expected = store.recover().unwrap();
    drop(store);
    let before = retained(directory.path());
    let mut frozen = canonical_open(directory.path(), identity()).unwrap();
    assert_eq!(frozen.recover().unwrap(), expected);
    drop(frozen);
    assert_eq!(retained(directory.path()), before);
}

#[test]
fn frozen_production_recovery_rejects_noncanonical_inventory_and_invalid_domain_state() {
    for mutation in [
        "CREATE TABLE unexpected(id INTEGER PRIMARY KEY)",
        "UPDATE store_metadata SET legacy_seed_decision = 1",
        "DELETE FROM identity_high_water",
    ] {
        let (directory, store) = canonical_fixture();
        drop(store);
        let mut database = Database::open_existing_for_application(
            directory.path(),
            schema::migrations(),
            schema::APPLICATION_ID,
        )
        .unwrap();
        assert_eq!(
            database.transaction(|sql| sql.execute_batch(mutation)),
            CommitDisposition::Durable(())
        );
        drop(database);
        let before = retained(directory.path());
        assert_eq!(
            canonical_open(directory.path(), identity()).unwrap_err(),
            StoreFailure::InvalidDurableState
        );
        assert_eq!(retained(directory.path()), before);
    }
}

#[test]
fn frozen_production_recovery_checks_native_identity_binding_without_writes() {
    let (directory, store) = canonical_fixture();
    drop(store);
    let before = retained(directory.path());
    let expected = StoreIdentity {
        server_public_key: [30; 32],
        ..identity()
    };
    assert_eq!(
        canonical_open(directory.path(), expected).unwrap_err(),
        StoreFailure::IdentityMismatch
    );
    assert_eq!(retained(directory.path()), before);
}

#[test]
fn frozen_open_retains_only_an_o_rdonly_native_database_descriptor() {
    let directory = initialized();
    let database = open(directory.path()).unwrap();
    let expected = fs::metadata(directory.path().join(DATABASE_FILE_NAME)).unwrap();
    let mut matches = 0;
    // Inspect descriptor flags without opening or closing the SQLite inode.
    // Reopening it here could silently release this process's POSIX locks.
    for descriptor in fs::read_dir("/dev/fd").unwrap() {
        let descriptor = descriptor.unwrap();
        let Some(fd) = descriptor
            .file_name()
            .to_str()
            .and_then(|value| value.parse::<i32>().ok())
        else {
            continue;
        };
        // SAFETY: fstat initializes stat on success. A concurrently closed
        // unrelated descriptor is ignored, never dereferenced or closed here.
        let mut stat: libc::stat = unsafe { std::mem::zeroed() };
        if unsafe { libc::fstat(fd, &mut stat) } != 0
            || stat.st_dev as u64 != expected.dev()
            || stat.st_ino as u64 != expected.ino()
        {
            continue;
        }
        // SAFETY: F_GETFL reads flags from the retained descriptor.
        assert_eq!(
            unsafe { libc::fcntl(fd, libc::F_GETFL) } & libc::O_ACCMODE,
            libc::O_RDONLY
        );
        matches += 1;
    }
    assert_eq!(matches, 1);
    drop(database);
}

#[test]
fn frozen_open_never_borrows_a_live_writable_descriptor() {
    let directory = initialized();
    let writer =
        Database::open_existing_for_application(directory.path(), MIGRATIONS, APPLICATION).unwrap();
    let before = retained(directory.path());
    assert_eq!(
        open(directory.path()).unwrap_err(),
        StorageFailure::Unavailable
    );
    assert_eq!(writer.schema_version(), Ok(1));
    drop(writer);
    drop(open(directory.path()).unwrap());
    assert_eq!(retained(directory.path()), before);
}

#[test]
fn frozen_open_rejects_sqlite_integrity_failure_without_repair() {
    let directory = fixture();
    let file = directory.path().join(DATABASE_FILE_NAME);
    let connection = rusqlite::Connection::open(&file).unwrap();
    connection
        .execute_batch(
            "PRAGMA application_id=1398034258; PRAGMA user_version=1; \
         CREATE TABLE items(id INTEGER PRIMARY KEY, value TEXT NOT NULL); \
         CREATE INDEX items_by_value ON items(value); \
         INSERT INTO items VALUES(7, 'retained'); \
         PRAGMA writable_schema=ON; \
         DELETE FROM sqlite_schema WHERE name='items_by_value'; \
         PRAGMA writable_schema=OFF;",
        )
        .unwrap();
    drop(connection);
    fs::set_permissions(&file, fs::Permissions::from_mode(0o600)).unwrap();
    // Removing the index definition leaves its allocated B-tree page orphaned.
    // Establish that the header remains readable and SQLite's read-only integrity
    // checker, rather than schema-version parsing, detects this fixture's damage.
    let connection =
        rusqlite::Connection::open_with_flags(&file, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .unwrap();
    assert_eq!(
        connection
            .query_row("PRAGMA user_version", [], |row| row.get::<_, u32>(0))
            .unwrap(),
        1
    );
    assert_eq!(
        connection
            .query_row("PRAGMA application_id", [], |row| row.get::<_, u32>(0))
            .unwrap(),
        APPLICATION
    );
    assert_ne!(
        connection
            .query_row("PRAGMA integrity_check", [], |row| row.get::<_, String>(0))
            .unwrap(),
        "ok"
    );
    drop(connection);
    let before = retained(directory.path());
    assert_eq!(
        open(directory.path()).unwrap_err(),
        StorageFailure::InvalidState
    );
    assert_eq!(retained(directory.path()), before);
}

#[test]
fn frozen_production_recovery_rejects_check_constraint_corruption_without_repair() {
    let (directory, store) = canonical_fixture();
    drop(store);
    let file = directory.path().join(DATABASE_FILE_NAME);
    let connection = rusqlite::Connection::open(&file).unwrap();
    connection
        .execute_batch(
            "PRAGMA ignore_check_constraints=ON; \
         UPDATE store_metadata SET encoding_version=0; \
         PRAGMA ignore_check_constraints=OFF;",
        )
        .unwrap();
    assert_ne!(
        connection
            .query_row("PRAGMA integrity_check", [], |row| row.get::<_, String>(0))
            .unwrap(),
        "ok"
    );
    drop(connection);
    let before = retained(directory.path());
    // SQLite omits CHECK expressions when loading a read-only B-tree. Its
    // structural integrity check alone cannot admit canonical state. The real
    // schema/domain recovery must reject this retained encoding violation.
    assert_eq!(
        canonical_open(directory.path(), identity()).unwrap_err(),
        StoreFailure::InvalidDurableState
    );
    assert_eq!(retained(directory.path()), before);
}

#[test]
fn frozen_production_recovery_rejects_noncanonical_retained_boolean_integers() {
    for (table, column) in [
        ("worktrees", "is_main"),
        ("worktrees", "missing"),
        ("worktree_inventory", "listed"),
    ] {
        for invalid in [-1, 2] {
            let (directory, store) = canonical_fixture();
            drop(store);
            let mut database = Database::open_existing_for_application(
                directory.path(),
                schema::migrations(),
                schema::APPLICATION_ID,
            )
            .unwrap();
            assert_eq!(database.transaction(|sql| {
                sql.execute_batch(
                    "UPDATE identity_high_water SET repository=x'0000000000000001', worktree=x'0000000000000001'; \
                     INSERT INTO repositories VALUES(x'0000000000000001', x'2f666978747572652d7265706f7369746f7279'); \
                     INSERT INTO repository_field_versions VALUES(x'0000000000000001', x'0000000000000001'); \
                     INSERT INTO worktrees VALUES(x'0000000000000001', x'0000000000000001', x'2f666978747572652d7265706f7369746f7279', 1, 0); \
                     INSERT INTO worktree_inventory VALUES(x'0000000000000001', 1, x'0000000000000001');",
                )
            }), CommitDisposition::Durable(()));
            drop(database);
            // Prove the complete production recovery accepts the uncorrupted
            // main-worktree fixture before changing one raw stored boolean.
            let mut baseline = canonical_open(directory.path(), identity()).unwrap();
            let recovered = baseline.recover().unwrap();
            assert_eq!(recovered.organization.worktrees.len(), 1);
            assert!(recovered.organization.worktrees[0].main);
            drop(baseline);

            let connection =
                rusqlite::Connection::open(directory.path().join(DATABASE_FILE_NAME)).unwrap();
            connection
                .execute_batch("PRAGMA ignore_check_constraints=ON;")
                .unwrap();
            connection
                .execute(&format!("UPDATE {table} SET {column}=?1"), [invalid])
                .unwrap();
            connection
                .execute_batch("PRAGMA ignore_check_constraints=OFF;")
                .unwrap();
            assert_ne!(
                connection
                    .query_row("PRAGMA integrity_check", [], |row| row.get::<_, String>(0))
                    .unwrap(),
                "ok"
            );
            drop(connection);
            let before = retained(directory.path());
            assert_eq!(
                canonical_open(directory.path(), identity()).unwrap_err(),
                StoreFailure::InvalidDurableState,
                "{table}.{column} admitted noncanonical integer {invalid}",
            );
            assert_eq!(retained(directory.path()), before);
        }
    }
}

fn canonical_paired_pending_fixture() -> tempfile::TempDir {
    let (directory, store) = canonical_fixture();
    drop(store);
    let mut database = Database::open_existing_for_application(
        directory.path(),
        schema::migrations(),
        schema::APPLICATION_ID,
    )
    .unwrap();
    let one = 1_u64.to_be_bytes();
    let two = 2_u64.to_be_bytes();
    assert_eq!(database.transaction(|sql| {
        sql.execute_batch(
            "UPDATE identity_high_water SET pairing=x'0000000000000001', client=x'0000000000000001', operation_stream=x'0000000000000002'; \
             UPDATE store_metadata SET store_revision=x'0000000000000002';",
        )?;
        sql.execute(
            "INSERT INTO pairings VALUES(?1, ?2, ?3, ?4, ?1, 0, NULL)",
            rusqlite::params![one.as_slice(), [3_u8; 16].as_slice(), [4_u8; 32].as_slice(), [5_u8; 32].as_slice()],
        )?;
        sql.execute(
            "INSERT INTO operation_streams VALUES(?1, ?1, ?1, 0, NULL, 0, NULL, ?1, zeroblob(8), NULL, NULL, 1, 1, 1)",
            [one.as_slice()],
        )?;
        sql.execute(
            "INSERT INTO operation_streams VALUES(?1, ?2, ?2, 1, NULL, 0, NULL, zeroblob(8), zeroblob(8), 2, x'0000000000010000', 1, 1, 1)",
            rusqlite::params![two.as_slice(), one.as_slice()],
        )?;
        sql.execute("INSERT INTO paired_clients VALUES(?1, ?1, ?1, ?2)", rusqlite::params![one.as_slice(), two.as_slice()])?;
        sql.execute(
            "INSERT INTO operation_entries VALUES(?1, ?1, ?2, 0, 0, x'0000000000010000', ?3, zeroblob(8), ?3, NULL, 0, NULL, NULL, NULL)",
            rusqlite::params![one.as_slice(), [7_u8; 32].as_slice(), two.as_slice()],
        )?;
        Ok(())
    }), CommitDisposition::Durable(()));
    drop(database);
    // Keep the real ledger, pairing receipt reader and domain validators. This
    // fixture has one authorized pairing and a pending ordinary operation, but no
    // signed receipts, credentials, signer, native process or external service.
    let mut baseline = canonical_open(directory.path(), identity()).unwrap();
    assert!(baseline.recover().unwrap().pairing.is_some());
    drop(baseline);
    directory
}

fn corrupt_checked_fixture(directory: &Path, mutation: &str) {
    let connection = rusqlite::Connection::open(directory.join(DATABASE_FILE_NAME)).unwrap();
    connection
        .execute_batch("PRAGMA ignore_check_constraints=ON;")
        .unwrap();
    connection.execute_batch(mutation).unwrap();
    connection
        .execute_batch("PRAGMA ignore_check_constraints=OFF;")
        .unwrap();
    assert_ne!(
        connection
            .query_row("PRAGMA integrity_check", [], |row| row.get::<_, String>(0))
            .unwrap(),
        "ok"
    );
}

#[test]
fn frozen_production_recovery_rejects_revocation_fields_on_authorized_pairings() {
    let directory = canonical_paired_pending_fixture();
    corrupt_checked_fixture(
        directory.path(),
        "UPDATE pairings SET revoked_revision=x'0000000000000001'",
    );
    let before = retained(directory.path());
    assert_eq!(
        canonical_open(directory.path(), identity()).unwrap_err(),
        StoreFailure::InvalidDurableState
    );
    assert_eq!(retained(directory.path()), before);
}

#[test]
fn frozen_production_recovery_rejects_forbidden_ledger_variant_fields() {
    for mutation in [
        "UPDATE operation_streams SET enrollment_claim_digest=zeroblob(32) WHERE purpose=0",
        "UPDATE operation_streams SET enrollment_claim_digest=zeroblob(32) WHERE purpose=1",
        "UPDATE operation_streams SET retirement_reason=0 WHERE status=0",
        "UPDATE operation_streams SET dormant_bytes=x'0000000000010000' WHERE purpose=0",
        "UPDATE operation_entries SET completed_at=x'0000000000000002'",
        "UPDATE operation_entries SET compact_outcome=x'01'",
        "UPDATE operation_entries SET publication_revision=zeroblob(8)",
        "UPDATE operation_entries SET external_progress=x'01'",
    ] {
        let directory = canonical_paired_pending_fixture();
        corrupt_checked_fixture(directory.path(), mutation);
        let before = retained(directory.path());
        assert_eq!(
            canonical_open(directory.path(), identity()).unwrap_err(),
            StoreFailure::InvalidDurableState,
            "admitted forbidden variant field: {mutation}"
        );
        assert_eq!(retained(directory.path()), before);
    }
}
