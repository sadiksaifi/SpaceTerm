#![cfg(any(target_os = "macos", target_os = "linux"))]

use spaceterm_host::storage::{
    CommitDisposition, DATABASE_FILE_NAME, Database, Migration, StorageFailure,
};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(0);
const MIGRATIONS: &[Migration<'_>] = &[Migration {
    sql: "CREATE TABLE items(id INTEGER PRIMARY KEY, value BLOB);",
}];

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        #[cfg(target_os = "macos")]
        let root = PathBuf::from("/private/tmp");
        #[cfg(target_os = "linux")]
        let root = PathBuf::from(std::env::var_os("HOME").unwrap());
        let path = root.join(format!(
            "spaceterm-sqlite-durability-{}-{}",
            std::process::id(),
            NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        Self(path)
    }
    fn file(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}
fn count(database: &Database) -> i64 {
    database
        .query_row("SELECT count(*) FROM items", [], |row| row.get(0))
        .unwrap()
}

#[test]
fn commits_survive_reopen_and_new_migrations_preserve_existing_data() {
    let fixture = Fixture::new();
    let mut database = Database::open(&fixture.0, MIGRATIONS).unwrap();
    assert_eq!(
        database.transaction(|transaction| transaction
            .execute("INSERT INTO items VALUES(?1, ?2)", (1, "retained"))
            .map(|_| ())),
        CommitDisposition::Durable(())
    );
    drop(database);
    let migrations = [
        MIGRATIONS[0],
        Migration {
            sql: "ALTER TABLE items ADD COLUMN generation INTEGER NOT NULL DEFAULT 1;",
        },
    ];
    let database = Database::open(&fixture.0, &migrations).unwrap();
    assert_eq!(database.schema_version(), Ok(2));
    assert_eq!(
        database.query_row(
            "SELECT value, generation FROM items WHERE id=1",
            [],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        ),
        Ok(("retained".into(), 1))
    );
}

#[test]
fn unknown_newer_schema_is_a_typed_failure_and_never_reinitializes_data() {
    let fixture = Fixture::new();
    let migrations = [
        MIGRATIONS[0],
        Migration {
            sql: "CREATE TABLE newer(id INTEGER);",
        },
    ];
    drop(Database::open(&fixture.0, &migrations).unwrap());
    // Read only after the live SQLite handle is closed.
    let before = fs::read(fixture.file(DATABASE_FILE_NAME)).unwrap();
    assert_eq!(
        Database::open(&fixture.0, MIGRATIONS).unwrap_err(),
        StorageFailure::UnknownSchema {
            found: 2,
            supported: 1
        }
    );
    assert_eq!(fs::read(fixture.file(DATABASE_FILE_NAME)).unwrap(), before);
}

#[test]
fn migration_failure_rolls_back_every_pending_migration_and_the_version() {
    let fixture = Fixture::new();
    drop(Database::open(&fixture.0, MIGRATIONS).unwrap());
    let migrations = [
        MIGRATIONS[0],
        Migration {
            sql: "CREATE TABLE partial(id INTEGER);",
        },
        Migration {
            sql: "INSERT INTO missing VALUES(1);",
        },
    ];
    assert_eq!(
        Database::open(&fixture.0, &migrations).unwrap_err(),
        StorageFailure::InvalidState
    );
    let database = Database::open(&fixture.0, MIGRATIONS).unwrap();
    assert_eq!(database.schema_version(), Ok(1));
    assert_eq!(
        database.query_row(
            "SELECT count(*) FROM sqlite_schema WHERE name='partial'",
            [],
            |row| row.get::<_, i64>(0)
        ),
        Ok(0)
    );
}

#[test]
fn corrupt_storage_is_retained_instead_of_replaced() {
    let fixture = Fixture::new();
    fs::write(fixture.file(DATABASE_FILE_NAME), b"not a database").unwrap();
    fs::set_permissions(
        fixture.file(DATABASE_FILE_NAME),
        fs::Permissions::from_mode(0o600),
    )
    .unwrap();
    assert_eq!(
        Database::open(&fixture.0, MIGRATIONS).unwrap_err(),
        StorageFailure::InvalidState
    );
    assert_eq!(
        fs::read(fixture.file(DATABASE_FILE_NAME)).unwrap(),
        b"not a database"
    );
}

#[test]
fn foreign_key_check_rejects_existing_invalid_state() {
    let fixture = Fixture::new();
    // Prepare invalid retained state before opening the authority.
    let connection = rusqlite::Connection::open(fixture.file(DATABASE_FILE_NAME)).unwrap();
    connection.execute_batch("PRAGMA foreign_keys=OFF; CREATE TABLE parents(id INTEGER PRIMARY KEY); CREATE TABLE children(parent INTEGER REFERENCES parents(id)); INSERT INTO children VALUES(42); PRAGMA user_version=1;").unwrap();
    drop(connection);
    fs::set_permissions(
        fixture.file(DATABASE_FILE_NAME),
        fs::Permissions::from_mode(0o600),
    )
    .unwrap();
    assert_eq!(
        Database::open(&fixture.0, MIGRATIONS).unwrap_err(),
        StorageFailure::InvalidState
    );
}

#[test]
fn deferred_foreign_key_failure_at_commit_is_not_applied() {
    let fixture = Fixture::new();
    let migrations = [Migration {
        sql: "CREATE TABLE parents(id INTEGER PRIMARY KEY); CREATE TABLE children(parent INTEGER REFERENCES parents(id) DEFERRABLE INITIALLY DEFERRED);",
    }];
    let mut database = Database::open(&fixture.0, &migrations).unwrap();
    assert_eq!(
        database.transaction(|transaction| transaction
            .execute("INSERT INTO children VALUES(42)", [])
            .map(|_| ())),
        CommitDisposition::NotApplied(StorageFailure::InvalidState)
    );
    assert_eq!(
        database.query_row("SELECT count(*) FROM children", [], |row| row
            .get::<_, i64>(0)),
        Ok(0)
    );
    assert_eq!(
        database.transaction(|transaction| transaction
            .execute("INSERT INTO parents VALUES(42)", [])
            .map(|_| ())),
        CommitDisposition::Durable(())
    );
}

#[test]
fn callback_failure_rolls_back_without_poisoning_known_uncommitted_state() {
    let fixture = Fixture::new();
    let mut database = Database::open(&fixture.0, MIGRATIONS).unwrap();
    assert_eq!(
        database.transaction(|transaction| {
            transaction.execute("INSERT INTO items VALUES(1, NULL)", [])?;
            Err::<(), _>(StorageFailure::InvalidState)
        }),
        CommitDisposition::NotApplied(StorageFailure::InvalidState)
    );
    assert_eq!(count(&database), 0);
    assert_eq!(
        database.transaction(|transaction| transaction
            .execute("INSERT INTO items VALUES(1, NULL)", [])
            .map(|_| ())),
        CommitDisposition::Durable(())
    );
}

#[test]
fn public_sql_cannot_bypass_transaction_or_durability_policy() {
    let fixture = Fixture::new();
    let mut database = Database::open(&fixture.0, MIGRATIONS).unwrap();
    assert_eq!(
        database.query_row(
            "INSERT INTO items VALUES(1, NULL) RETURNING id",
            [],
            |row| row.get::<_, i64>(0)
        ),
        Err(StorageFailure::InvalidState)
    );
    for sql in [
        "COMMIT",
        "ROLLBACK",
        "PRAGMA synchronous=OFF",
        "PRAGMA foreign_keys=OFF",
        "PRAGMA user_version=99",
        "ATTACH ':memory:' AS other",
        "SAVEPOINT bypass",
    ] {
        assert_eq!(
            database.transaction(|transaction| {
                transaction.execute("INSERT INTO items VALUES(1, NULL)", [])?;
                transaction.execute_batch(sql)
            }),
            CommitDisposition::NotApplied(StorageFailure::InvalidState),
            "policy escape was admitted"
        );
        assert_eq!(count(&database), 0);
    }
}

#[test]
fn callback_unwind_fences_writes_until_reopen() {
    let fixture = Fixture::new();
    let mut database = Database::open(&fixture.0, MIGRATIONS).unwrap();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        database.transaction::<()>(|transaction| {
            transaction
                .execute("INSERT INTO items VALUES(1, NULL)", [])
                .unwrap();
            panic!("callback failed before returning a disposition")
        })
    }));
    assert!(result.is_err());
    assert_eq!(
        database.transaction(|_| Ok(())),
        CommitDisposition::NotApplied(StorageFailure::RecoveryRequired)
    );
    drop(database);
    let database = Database::open(&fixture.0, MIGRATIONS).unwrap();
    assert_eq!(count(&database), 0);
}

#[test]
fn killed_writer_recovers_a_spilled_transaction_without_partial_effects() {
    let fixture = Fixture::new();
    drop(Database::open(&fixture.0, MIGRATIONS).unwrap());
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "crash_writer_child", "--nocapture"])
        .env("SPACETERM_SQLITE_CRASH_CHILD", &fixture.0)
        .stdout(Stdio::null())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(30);
    while !fixture.file("ready").exists() {
        if let Some(status) = child.try_wait().unwrap() {
            panic!("writer exited before the crash point: {status}");
        }
        if Instant::now() >= deadline {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("writer did not reach the crash point");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    child.kill().unwrap();
    child.wait().unwrap();
    assert!(
        fixture
            .file(&format!("{DATABASE_FILE_NAME}-journal"))
            .exists()
    );
    let database = Database::open(&fixture.0, MIGRATIONS).unwrap();
    assert_eq!(count(&database), 0);
    assert_eq!(database.schema_version(), Ok(1));
}

#[test]
fn crash_writer_child() {
    let Some(directory) = std::env::var_os("SPACETERM_SQLITE_CRASH_CHILD") else {
        return;
    };
    let directory = PathBuf::from(directory);
    let mut database = Database::open(&directory, MIGRATIONS).unwrap();
    let _outcome = database.transaction::<()>(|transaction| {
        // Exceed SQLite's page cache so the main database is changed before COMMIT.
        transaction.execute_batch("WITH RECURSIVE ids(id) AS (SELECT 1 UNION ALL SELECT id+1 FROM ids WHERE id<2000) INSERT INTO items SELECT id, zeroblob(4096) FROM ids;")?;
        fs::write(directory.join("ready"), []).unwrap();
        loop { std::thread::sleep(Duration::from_secs(1)); }
    });
    panic!("crash fixture unexpectedly returned")
}

#[test]
fn sqlite_automatic_rollback_is_known_not_applied_without_io_failure() {
    let fixture = Fixture::new();
    let mut database = Database::open(&fixture.0, MIGRATIONS).unwrap();
    let outcome = database.transaction(|transaction| {
        transaction.execute_batch(
            "INSERT INTO items VALUES(1, NULL); INSERT OR ROLLBACK INTO items VALUES(1, NULL);",
        )
    });
    assert_eq!(
        outcome,
        CommitDisposition::NotApplied(StorageFailure::InvalidState)
    );
    assert_eq!(count(&database), 0);
    assert_eq!(
        database.transaction(|transaction| transaction
            .execute("INSERT INTO items VALUES(1, NULL)", [])
            .map(|_| ())),
        CommitDisposition::Durable(())
    );
}

#[test]
fn swallowed_automatic_rollback_cannot_start_an_untracked_autocommit_write() {
    let fixture = Fixture::new();
    let mut database = Database::open(&fixture.0, MIGRATIONS).unwrap();
    let outcome = database.transaction(|transaction| {
        assert_eq!(
            transaction.execute_batch(
                "INSERT INTO items VALUES(1, NULL); INSERT OR ROLLBACK INTO items VALUES(1, NULL);"
            ),
            Err(StorageFailure::InvalidState)
        );
        assert_eq!(
            transaction.execute("INSERT INTO items VALUES(2, NULL)", []),
            Err(StorageFailure::InvalidState)
        );
        Ok(())
    });
    assert_eq!(
        outcome,
        CommitDisposition::NotApplied(StorageFailure::InvalidState)
    );
    assert_eq!(count(&database), 0);
}
