#![cfg(any(target_os = "macos", target_os = "linux"))]

use spaceterm_host::storage::{
    CommitDisposition, DATABASE_FILE_NAME, Database, Migration, StorageFailure,
};
use std::fs::{self, OpenOptions};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt, symlink};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(0);
const MIGRATIONS: &[Migration<'_>] = &[Migration {
    sql: "CREATE TABLE items(id INTEGER PRIMARY KEY);",
}];

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        #[cfg(target_os = "macos")]
        let root = PathBuf::from("/private/tmp");
        #[cfg(target_os = "linux")]
        let root = PathBuf::from(std::env::var_os("HOME").unwrap());
        let path = root.join(format!(
            "spaceterm-sqlite-authority-{}-{}",
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

#[test]
fn opens_a_private_directory_and_creates_a_database() {
    let fixture = Fixture::new();
    let database = Database::open(&fixture.0, MIGRATIONS).unwrap();
    assert_eq!(database.schema_version(), Ok(1));
    assert_eq!(
        fs::symlink_metadata(fixture.file(DATABASE_FILE_NAME))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
}

#[test]
fn unavailable_directory_does_not_create_or_replace_storage() {
    let fixture = Fixture::new();
    let missing = fixture.file("missing");
    assert_eq!(
        Database::open(&missing, MIGRATIONS).unwrap_err(),
        StorageFailure::Unavailable
    );
    assert!(!missing.exists());
}

#[test]
fn rejects_nonprivate_directories_and_unsafe_database_permissions() {
    let fixture = Fixture::new();
    fs::set_permissions(&fixture.0, fs::Permissions::from_mode(0o750)).unwrap();
    assert_eq!(
        Database::open(&fixture.0, MIGRATIONS).unwrap_err(),
        StorageFailure::UnsafeStorage
    );
    fs::set_permissions(&fixture.0, fs::Permissions::from_mode(0o700)).unwrap();
    drop(Database::open(&fixture.0, MIGRATIONS).unwrap());
    fs::set_permissions(
        fixture.file(DATABASE_FILE_NAME),
        fs::Permissions::from_mode(0o640),
    )
    .unwrap();
    assert_eq!(
        Database::open(&fixture.0, MIGRATIONS).unwrap_err(),
        StorageFailure::UnsafeStorage
    );
}

#[test]
fn rejects_symlink_traversal_in_every_directory_component() {
    let fixture = Fixture::new();
    fs::create_dir(fixture.file("real")).unwrap();
    fs::set_permissions(fixture.file("real"), fs::Permissions::from_mode(0o700)).unwrap();
    symlink("real", fixture.file("alias")).unwrap();
    assert_eq!(
        Database::open(&fixture.file("alias"), MIGRATIONS).unwrap_err(),
        StorageFailure::UnsafeStorage
    );
}

#[test]
fn rejects_relative_and_parent_component_paths() {
    let fixture = Fixture::new();
    assert_eq!(
        Database::open(std::path::Path::new("."), MIGRATIONS).unwrap_err(),
        StorageFailure::UnsafeStorage
    );
    assert_eq!(
        Database::open(&fixture.0.join(".."), MIGRATIONS).unwrap_err(),
        StorageFailure::UnsafeStorage
    );
}

#[test]
fn rejects_database_symlinks_and_hard_links_without_touching_the_target() {
    let fixture = Fixture::new();
    fs::write(fixture.file("target"), b"retained bytes").unwrap();
    fs::set_permissions(fixture.file("target"), fs::Permissions::from_mode(0o600)).unwrap();
    symlink("target", fixture.file(DATABASE_FILE_NAME)).unwrap();
    assert_eq!(
        Database::open(&fixture.0, MIGRATIONS).unwrap_err(),
        StorageFailure::UnsafeStorage
    );
    fs::remove_file(fixture.file(DATABASE_FILE_NAME)).unwrap();
    fs::hard_link(fixture.file("target"), fixture.file(DATABASE_FILE_NAME)).unwrap();
    assert_eq!(
        Database::open(&fixture.0, MIGRATIONS).unwrap_err(),
        StorageFailure::UnsafeStorage
    );
    assert_eq!(fs::read(fixture.file("target")).unwrap(), b"retained bytes");
}

#[test]
fn rejects_unsafe_journal_wal_and_shm_entries_before_sqlite_opens_them() {
    for suffix in ["-journal", "-wal", "-shm"] {
        let fixture = Fixture::new();
        drop(Database::open(&fixture.0, MIGRATIONS).unwrap());
        let sidecar = fixture.file(&format!("{DATABASE_FILE_NAME}{suffix}"));
        symlink("untouched", &sidecar).unwrap();
        assert_eq!(
            Database::open(&fixture.0, MIGRATIONS).unwrap_err(),
            StorageFailure::UnsafeStorage
        );
        fs::remove_file(&sidecar).unwrap();
        fs::write(fixture.file("untouched"), []).unwrap();
        fs::set_permissions(fixture.file("untouched"), fs::Permissions::from_mode(0o600)).unwrap();
        fs::hard_link(fixture.file("untouched"), sidecar).unwrap();
        assert_eq!(
            Database::open(&fixture.0, MIGRATIONS).unwrap_err(),
            StorageFailure::UnsafeStorage
        );
    }
}

#[test]
fn rollback_policy_does_not_discard_existing_wal_or_shm_state() {
    for suffix in ["-wal", "-shm"] {
        let fixture = Fixture::new();
        drop(Database::open(&fixture.0, MIGRATIONS).unwrap());
        let sidecar = fixture.file(&format!("{DATABASE_FILE_NAME}{suffix}"));
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&sidecar)
            .unwrap();
        assert_eq!(
            Database::open(&fixture.0, MIGRATIONS).unwrap_err(),
            StorageFailure::InvalidState
        );
        assert!(sidecar.exists());
    }
}

#[test]
fn retained_authority_rejects_database_replacement_without_reopening_it() {
    let fixture = Fixture::new();
    let mut database = Database::open(&fixture.0, MIGRATIONS).unwrap();
    fs::rename(fixture.file(DATABASE_FILE_NAME), fixture.file("original")).unwrap();
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(fixture.file(DATABASE_FILE_NAME))
        .unwrap();
    assert_eq!(
        database.verify_authority(),
        Err(StorageFailure::UnsafeStorage)
    );
    assert_eq!(
        database.transaction(|_| Ok(())),
        CommitDisposition::NotApplied(StorageFailure::UnsafeStorage)
    );
}

#[test]
fn retained_authority_rejects_a_replaced_directory() {
    let fixture = Fixture::new();
    let directory = fixture.file("private");
    fs::create_dir(&directory).unwrap();
    fs::set_permissions(&directory, fs::Permissions::from_mode(0o700)).unwrap();
    let database = Database::open(&directory, MIGRATIONS).unwrap();
    fs::rename(&directory, fixture.file("original")).unwrap();
    fs::create_dir(&directory).unwrap();
    fs::set_permissions(&directory, fs::Permissions::from_mode(0o700)).unwrap();
    assert_eq!(
        database.verify_authority(),
        Err(StorageFailure::UnsafeStorage)
    );
}

#[test]
fn replaced_live_journal_is_rejected_and_the_commit_is_not_acknowledged() {
    let fixture = Fixture::new();
    let mut database = Database::open(&fixture.0, MIGRATIONS).unwrap();
    let journal = fixture.file(&format!("{DATABASE_FILE_NAME}-journal"));
    let outcome = database.transaction(|transaction| {
        transaction.execute("INSERT INTO items VALUES(1)", [])?;
        assert!(journal.exists());
        fs::rename(&journal, fixture.file("original-journal")).unwrap();
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&journal)
            .unwrap();
        assert_eq!(
            database_authority_without_borrow(&fixture),
            StorageFailure::UnsafeStorage
        );
        Ok(())
    });
    assert_eq!(outcome, CommitDisposition::Indeterminate);
    assert_eq!(
        database.verify_authority(),
        Err(StorageFailure::UnsafeStorage)
    );
}

fn database_authority_without_borrow(fixture: &Fixture) -> StorageFailure {
    Database::open(&fixture.0, MIGRATIONS).unwrap_err()
}

#[test]
fn authority_checks_and_second_connection_close_preserve_posix_locks() {
    let fixture = Fixture::new();
    let mut first = Database::open(&fixture.0, MIGRATIONS).unwrap();
    let mut second = Database::open(&fixture.0, MIGRATIONS).unwrap();
    let outcome = first.transaction(|transaction| {
        transaction.execute("INSERT INTO items VALUES(1)", [])?;
        for _ in 0..20 {
            second.verify_authority().unwrap();
        }
        assert_eq!(
            second.transaction(|_| panic!("locked callback must not run")),
            CommitDisposition::<()>::NotApplied(StorageFailure::Unavailable)
        );
        drop(second);
        let status = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "native_sqlite_lock_child", "--nocapture"])
            .env("SPACETERM_SQLITE_LOCK_CHILD", &fixture.0)
            .stdout(Stdio::null())
            .status()
            .unwrap();
        assert!(status.success(), "another process acquired the writer lock");
        Ok(())
    });
    assert_eq!(outcome, CommitDisposition::Durable(()));
}

#[test]
fn native_sqlite_lock_child() {
    let Some(directory) = std::env::var_os("SPACETERM_SQLITE_LOCK_CHILD") else {
        return;
    };
    // A different process uses SQLite's native VFS to verify interoperability with
    // the byte-range protocol. No path-based helper touches the parent's live inode.
    let connection =
        rusqlite::Connection::open(PathBuf::from(directory).join(DATABASE_FILE_NAME)).unwrap();
    let result = connection.execute_batch("BEGIN IMMEDIATE");
    let busy = matches!(result, Err(rusqlite::Error::SqliteFailure(error, _)) if error.code == rusqlite::ErrorCode::DatabaseBusy);
    std::process::exit(if busy { 0 } else { 1 });
}

#[cfg(target_os = "linux")]
#[test]
fn volatile_tmpfs_is_not_a_durable_database_authority() {
    use std::os::fd::AsRawFd;
    let root = fs::File::open("/tmp").unwrap();
    // SAFETY: fstatfs fills the provided native structure for an opened directory.
    let mut filesystem: libc::statfs = unsafe { std::mem::zeroed() };
    assert_eq!(
        unsafe { libc::fstatfs(root.as_raw_fd(), &mut filesystem) },
        0
    );
    if filesystem.f_type != 0x0102_1994 {
        return;
    }
    let path =
        PathBuf::from("/tmp").join(format!("spaceterm-sqlite-volatile-{}", std::process::id()));
    fs::create_dir(&path).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
    let fixture = Fixture(path);
    assert_eq!(
        Database::open(&fixture.0, MIGRATIONS).unwrap_err(),
        StorageFailure::UnsafeStorage
    );
    assert!(!fixture.file(DATABASE_FILE_NAME).exists());
}

#[test]
fn an_active_reader_blocks_commit_and_a_busy_writer_rolls_back_cleanly() {
    let fixture = Fixture::new();
    let mut writer = Database::open(&fixture.0, MIGRATIONS).unwrap();
    assert_eq!(
        writer.transaction(|transaction| transaction
            .execute("INSERT INTO items VALUES(1)", [])
            .map(|_| ())),
        CommitDisposition::Durable(())
    );
    let reader = Database::open(&fixture.0, MIGRATIONS).unwrap();
    let value = reader.query_row("SELECT id FROM items", [], |row| {
        assert_eq!(
            writer.transaction(|transaction| transaction
                .execute("INSERT INTO items VALUES(2)", [])
                .map(|_| ())),
            CommitDisposition::NotApplied(StorageFailure::Unavailable)
        );
        row.get::<_, i64>(0)
    });
    assert_eq!(value, Ok(1));
    assert_eq!(
        writer.query_row("SELECT count(*) FROM items", [], |row| row.get::<_, i64>(0)),
        Ok(1)
    );
    assert_eq!(
        writer.transaction(|transaction| transaction
            .execute("INSERT INTO items VALUES(2)", [])
            .map(|_| ())),
        CommitDisposition::Durable(())
    );
}

#[test]
fn established_startup_never_creates_a_missing_database() {
    let fixture = Fixture::new();
    drop(Database::open(&fixture.0, MIGRATIONS).unwrap());
    drop(Database::open_existing(&fixture.0, MIGRATIONS).unwrap());
    fs::remove_file(fixture.file(DATABASE_FILE_NAME)).unwrap();
    assert_eq!(
        Database::open_existing(&fixture.0, MIGRATIONS).unwrap_err(),
        StorageFailure::Unavailable
    );
    assert!(!fixture.file(DATABASE_FILE_NAME).exists());
}

#[cfg(target_os = "macos")]
#[test]
fn extended_acls_cannot_substitute_for_private_directory_or_file_modes() {
    let fixture = Fixture::new();
    assert!(
        Command::new("/bin/chmod")
            .args(["+a", "everyone allow read,search"])
            .arg(&fixture.0)
            .status()
            .unwrap()
            .success()
    );
    assert_eq!(
        fs::metadata(&fixture.0).unwrap().permissions().mode() & 0o777,
        0o700
    );
    assert_eq!(
        Database::open(&fixture.0, MIGRATIONS).unwrap_err(),
        StorageFailure::UnsafeStorage
    );
    assert!(
        Command::new("/bin/chmod")
            .arg("-N")
            .arg(&fixture.0)
            .status()
            .unwrap()
            .success()
    );
    let database = Database::open(&fixture.0, MIGRATIONS).unwrap();
    assert!(
        Command::new("/bin/chmod")
            .args(["+a", "everyone allow read,write"])
            .arg(fixture.file(DATABASE_FILE_NAME))
            .status()
            .unwrap()
            .success()
    );
    assert_eq!(
        fs::metadata(fixture.file(DATABASE_FILE_NAME))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    assert_eq!(
        database.verify_authority(),
        Err(StorageFailure::UnsafeStorage)
    );
}
