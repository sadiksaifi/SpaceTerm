use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use super::directory::mutex;
use super::vfs::FaultPoint;
use super::{CommitDisposition, Database, Migration, StorageFailure};

static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(0);
const MIGRATIONS: &[Migration<'_>] = &[Migration {
    sql: "CREATE TABLE items(id INTEGER PRIMARY KEY);",
}];

struct Fixture {
    path: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        #[cfg(target_os = "macos")]
        let root = PathBuf::from("/private/tmp");
        #[cfg(target_os = "linux")]
        let root = PathBuf::from(std::env::var_os("HOME").unwrap());
        let path = root.join(format!(
            "spaceterm-sqlite-fault-{}-{}",
            std::process::id(),
            NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        Self { path }
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.path).unwrap();
    }
}

fn insert(database: &mut Database) -> CommitDisposition {
    database.transaction(|transaction| {
        transaction
            .execute("INSERT INTO items VALUES(1)", [])
            .map(|_| ())
    })
}

fn count(database: &Database) -> i64 {
    database
        .query_row("SELECT count(*) FROM items", [], |row| row.get(0))
        .unwrap()
}

#[test]
fn before_commit_failure_is_not_applied_and_keeps_the_handle_writable() {
    let fixture = Fixture::new();
    let mut database = Database::open(&fixture.path, MIGRATIONS).unwrap();
    *mutex(&database.registration.context.faults) = Some((FaultPoint::BeforeCommit, 0));
    assert_eq!(
        insert(&mut database),
        CommitDisposition::NotApplied(StorageFailure::Io)
    );
    assert_eq!(count(&database), 0);
    assert_eq!(insert(&mut database), CommitDisposition::Durable(()));
}

#[test]
fn failure_after_commit_before_return_requires_recovery_of_the_original_effect() {
    let fixture = Fixture::new();
    let mut database = Database::open(&fixture.path, MIGRATIONS).unwrap();
    *mutex(&database.registration.context.faults) = Some((FaultPoint::AfterCommit, 0));
    assert_eq!(insert(&mut database), CommitDisposition::Indeterminate);
    assert_eq!(
        insert(&mut database),
        CommitDisposition::NotApplied(StorageFailure::RecoveryRequired)
    );
    drop(database);
    let database = Database::open(&fixture.path, MIGRATIONS).unwrap();
    assert_eq!(count(&database), 1);
}

#[test]
fn sync_failure_stays_indeterminate_after_cleanup_and_reopen_validates_atomic_state() {
    for skip in 0..3 {
        let fixture = Fixture::new();
        let mut database = Database::open(&fixture.path, MIGRATIONS).unwrap();
        *mutex(&database.registration.context.faults) = Some((FaultPoint::Sync, skip));
        assert_eq!(insert(&mut database), CommitDisposition::Indeterminate);
        database.registration.context.directory.sync().unwrap();
        let entry = database
            .registration
            .context
            .directory
            .open_entry(super::directory::Kind::Database, false)
            .unwrap();
        // SAFETY: this is the existing VFS-owned descriptor, not a reopened inode.
        assert_eq!(unsafe { libc::fsync(entry.fd()) }, 0);
        drop(entry);
        database.verify_authority().unwrap();
        assert_eq!(
            insert(&mut database),
            CommitDisposition::NotApplied(StorageFailure::RecoveryRequired)
        );
        assert_eq!(
            database.query_row("SELECT count(*) FROM items", [], |row| row.get::<_, i64>(0)),
            Err(StorageFailure::RecoveryRequired)
        );
        drop(database);
        let database = Database::open(&fixture.path, MIGRATIONS).unwrap();
        assert!(matches!(count(&database), 0 | 1));
    }
}

#[test]
fn write_failure_requires_reopen_instead_of_claiming_a_clean_rollback() {
    let fixture = Fixture::new();
    let mut database = Database::open(&fixture.path, MIGRATIONS).unwrap();
    *mutex(&database.registration.context.faults) = Some((FaultPoint::Write, 0));
    let outcome = database.transaction(|transaction| {
        assert_eq!(
            transaction.execute("INSERT INTO items VALUES(1)", []),
            Err(StorageFailure::Io)
        );
        assert_eq!(
            transaction.execute("INSERT INTO items VALUES(2)", []),
            Err(StorageFailure::RecoveryRequired)
        );
        Ok(())
    });
    assert_eq!(outcome, CommitDisposition::Indeterminate);
    assert_eq!(
        insert(&mut database),
        CommitDisposition::NotApplied(StorageFailure::RecoveryRequired)
    );
    drop(database);
    let database = Database::open(&fixture.path, MIGRATIONS).unwrap();
    assert_eq!(count(&database), 0);
}

#[test]
fn directory_sync_failure_after_journal_deletion_is_not_reported_as_rollback() {
    let fixture = Fixture::new();
    let mut database = Database::open(&fixture.path, MIGRATIONS).unwrap();
    *mutex(&database.registration.context.faults) = Some((FaultPoint::DirectorySync, 0));
    assert_eq!(insert(&mut database), CommitDisposition::Indeterminate);
    drop(database);
    let database = Database::open(&fixture.path, MIGRATIONS).unwrap();
    assert_eq!(count(&database), 1);
}

#[test]
fn concurrent_journal_observation_and_commit_keep_both_connections_healthy() {
    use std::sync::mpsc::sync_channel;
    use std::time::Duration;

    let fixture = Fixture::new();
    let mut writer = Database::open(&fixture.path, MIGRATIONS).unwrap();
    let observer = Database::open_existing(&fixture.path, MIGRATIONS).unwrap();
    let directory = observer.registration.context.directory.clone();
    let (ready, journal_created) = sync_channel(1);
    let (commit, start_commit) = sync_channel(1);
    let writer = std::thread::spawn(move || {
        let outcome = writer.transaction(|transaction| {
            transaction.execute("INSERT INTO items VALUES(1)", [])?;
            ready.send(()).unwrap();
            start_commit.recv().unwrap();
            Ok(())
        });
        (writer, outcome)
    });
    journal_created
        .recv_timeout(Duration::from_secs(10))
        .unwrap();
    let (sampled, journal_sampled) = sync_channel(1);
    let (resume, resume_observation) = sync_channel(1);
    *mutex(&directory.journal_observation) = Some(super::directory::JournalObservation {
        sampled,
        resume: resume_observation,
    });
    let observer = std::thread::spawn(move || {
        let outcome = observer.verify_authority();
        (observer, outcome)
    });
    let guarded = journal_sampled
        .recv_timeout(Duration::from_secs(10))
        .unwrap();
    commit.send(()).unwrap();
    if guarded {
        // A serialized stat/cache update holds up deletion. Let it finish first.
        resume.send(()).unwrap();
    }
    let (mut writer, committed) = writer.join().unwrap();
    if !guarded {
        // Reproduce the stale-stat race exactly, without a probabilistic loop:
        // deletion completes before the sampled journal identity is cached.
        resume.send(()).unwrap();
    }
    let (mut observer, observed) = observer.join().unwrap();
    assert_eq!(committed, CommitDisposition::Durable(()));
    assert_eq!(observed, Ok(()));
    writer.verify_authority().unwrap();
    observer.verify_authority().unwrap();
    assert_eq!(count(&writer), 1);
    assert_eq!(count(&observer), 1);
    assert_eq!(
        writer.transaction(|transaction| transaction
            .execute("INSERT INTO items VALUES(2)", [])
            .map(|_| ())),
        CommitDisposition::Durable(())
    );
    assert_eq!(
        observer.transaction(|transaction| transaction
            .execute("INSERT INTO items VALUES(3)", [])
            .map(|_| ())),
        CommitDisposition::Durable(())
    );
    assert_eq!(count(&writer), 3);
    assert_eq!(count(&observer), 3);
}

#[cfg(target_os = "macos")]
#[test]
fn journal_deletion_is_full_synced_before_a_durable_commit() {
    use super::vfs::DurabilityEvent;

    let fixture = Fixture::new();
    let mut database = Database::open(&fixture.path, MIGRATIONS).unwrap();
    mutex(&database.registration.context.durability_events).clear();
    assert_eq!(insert(&mut database), CommitDisposition::Durable(()));
    let events = mutex(&database.registration.context.durability_events);
    let deleted = events
        .iter()
        .position(|event| *event == DurabilityEvent::JournalDeleted)
        .unwrap();
    assert!(events[..deleted].contains(&DurabilityEvent::JournalSync));
    assert!(events[..deleted].contains(&DurabilityEvent::DatabaseSync));
    assert_eq!(
        &events[deleted..],
        &[
            DurabilityEvent::JournalDeleted,
            DurabilityEvent::DirectorySync,
            DurabilityEvent::DeleteFullSync,
        ]
    );
}

#[cfg(target_os = "macos")]
#[test]
fn failed_journal_deletion_full_sync_fences_the_handle_until_reopen() {
    let fixture = Fixture::new();
    let mut database = Database::open(&fixture.path, MIGRATIONS).unwrap();
    *mutex(&database.registration.context.faults) = Some((FaultPoint::DeleteFullSync, 0));
    assert_eq!(insert(&mut database), CommitDisposition::Indeterminate);
    database.registration.context.directory.sync().unwrap();
    let entry = database
        .registration
        .context
        .directory
        .open_entry(super::directory::Kind::Database, false)
        .unwrap();
    // A successful later device barrier is not proof of the uncertain commit.
    // SAFETY: open_entry returned the existing VFS descriptor, never a reopened inode.
    assert_eq!(unsafe { libc::fsync(entry.fd()) }, 0);
    assert_eq!(unsafe { libc::fcntl(entry.fd(), libc::F_FULLFSYNC) }, 0);
    drop(entry);
    database.verify_authority().unwrap();
    assert_eq!(
        insert(&mut database),
        CommitDisposition::NotApplied(StorageFailure::RecoveryRequired)
    );
    assert_eq!(
        database.query_row("SELECT count(*) FROM items", [], |row| row.get::<_, i64>(0)),
        Err(StorageFailure::RecoveryRequired)
    );
    drop(database);
    let database = Database::open_existing(&fixture.path, MIGRATIONS).unwrap();
    assert_eq!(count(&database), 1);
}
