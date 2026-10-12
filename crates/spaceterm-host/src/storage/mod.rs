//! Descriptor-relative durable SQLite authority for headless server state.
//!
//! The database uses a DELETE rollback journal, not WAL. Organization writes are
//! serialized and small; rollback mode avoids a second shared-memory authority and
//! makes the commit boundary explicit: sync journal, sync database, delete journal,
//! sync directory, then on macOS F_FULLFSYNC the retained database descriptor to
//! flush the deletion from the device's volatile cache. There is no checkpoint or
//! background durability window. Every SQLite xSync performs fsync and, on macOS,
//! F_FULLFSYNC without a weaker fallback. Directory creation/deletion is also
//! synchronized. Unsupported synchronization
//! fails closed; successful filesystem calls are not physical power-loss evidence.
//! Admitted filesystems are local, ownership-enforcing APFS/HFS on macOS and the
//! ext-family, XFS, and Btrfs on Linux. Volatile and unproven filesystems fail closed.
//!
//! File operations use openat/fstatat/unlinkat against retained, no-follow private
//! directory authority. macOS extended ACLs are rejected even when mode bits are
//! private. Only the VFS owns database descriptors. Connections share
//! an inode descriptor and implement SQLite's POSIX byte-range locking protocol.
//! Authority checks never open another descriptor for a live database inode.
//! These checks reject unsafe sampled ownership, links, and replacements. They do
//! not defend against arbitrary owning-account code racing or modifying the data.
//!
//! A commit-time I/O failure is uncertain, even if rollback or a later sync succeeds.
//! An uncertain handle rejects subsequent operations. Drop it, reopen, validate,
//! and reconcile the original operation identity before publishing or retrying.

use std::path::Path;

use rusqlite::{Connection, Params, Row};
use thiserror::Error;

#[cfg(any(target_os = "macos", target_os = "linux"))]
mod directory;
#[cfg(any(target_os = "macos", target_os = "linux"))]
mod locking;
#[cfg(all(test, any(target_os = "macos", target_os = "linux")))]
mod tests;
#[cfg(any(target_os = "macos", target_os = "linux"))]
mod vfs;

/// Faults and crash barriers for private fixtures, absent from production builds.
#[cfg(all(
    any(test, feature = "test-support"),
    any(target_os = "macos", target_os = "linux")
))]
pub mod testing;

/// Fixed database entry name within the supplied private directory.
pub const DATABASE_FILE_NAME: &str = "organization.sqlite3";

/// Content-free storage failure classification.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum StorageFailure {
    /// The volume or database is unavailable or locked. Existing data is retained.
    #[error("storage is unavailable or locked")]
    Unavailable,
    /// File identity, ownership, mode, or traversal is unsafe.
    #[error("storage authority is unsafe")]
    UnsafeStorage,
    /// This executable cannot read a newer schema.
    #[error("storage schema is newer than supported")]
    UnknownSchema {
        /// Observed schema version.
        found: u32,
        /// Highest supported schema version.
        supported: u32,
    },
    /// Integrity, foreign-key, SQL, or migration validation failed.
    #[error("storage state is invalid")]
    InvalidState,
    /// A native write, read, or synchronization failed.
    #[error("storage I/O failed")]
    Io,
    /// An uncertain commit requires reopen and validation before further operations.
    #[error("storage recovery is required")]
    RecoveryRequired,
}

/// One ordered, transactional schema migration.
#[derive(Clone, Copy)]
pub struct Migration<'a> {
    /// SQL applied once when advancing to this migration's one-based version.
    /// Transaction control, pragmas, attachment, and extension loading are forbidden.
    pub sql: &'a str,
}

impl std::fmt::Debug for Migration<'_> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("Migration(..)")
    }
}

/// Outcome of one atomic transaction, before any publication or acknowledgement.
#[derive(Debug, Eq, PartialEq)]
#[must_use]
pub enum CommitDisposition<T = ()> {
    /// SQLite committed and all required synchronization completed.
    Durable(T),
    /// No transaction was committed. The failure contains no SQL or native details.
    NotApplied(StorageFailure),
    /// Effects may be durable. Reopen and reconcile, never blindly repeat the write.
    Indeterminate,
}

/// Durable SQLite connection rooted in retained private directory authority.
///
/// The directory must already exist, belong to the effective account, and have
/// mode 0700. Every database/journal file must be singly linked and mode 0600.
/// No missing/unavailable/invalid store is deleted or replaced by this module.
/// SQL entry points are intended for the host's schema adapter, not remote SQL.
pub struct Database {
    // Field order is significant: Connection closes before its registered VFS.
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    connection: Connection,
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    registration: vfs::Registration,
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    recovery_required: bool,
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    pre_migration_application_id: Option<u32>,
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    owned_application_id: Option<u32>,
}

impl std::fmt::Debug for Database {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("Database(..)")
    }
}

impl Database {
    /// Opens retained storage without authority to create a missing database.
    ///
    /// Established server startup and recovery should use this entry point.
    /// Absence is unavailable storage, never authority to replace an identity.
    ///
    /// # Errors
    /// Returns the same bounded failures as [`Self::open`], including `Unavailable`
    /// when the database entry is missing.
    pub fn open_existing(
        directory: &Path,
        migrations: &[Migration<'_>],
    ) -> Result<Self, StorageFailure> {
        Self::open_inner(directory, migrations, false, None)
    }

    /// Opens, recovers, validates, and migrates the fixed organization database.
    ///
    /// This entry point permits creating a database in the already-private directory.
    /// Use it only when initial storage creation is intended. Established startup
    /// and recovery use [`Self::open_existing`], never creation after a missing store.
    /// All pending migrations run in one immediate transaction. Newer schema
    /// detection precedes migrations and journal-policy changes. Integrity and
    /// foreign-key validation run before migration and again before its commit.
    /// A busy or unavailable store is a typed failure, never replacement authority.
    ///
    /// # Errors
    /// Returns bounded classifications for unsafe authority, unavailable storage,
    /// unsupported schema, invalid data, or I/O. Unsupported hosts fail closed.
    pub fn open(directory: &Path, migrations: &[Migration<'_>]) -> Result<Self, StorageFailure> {
        Self::open_inner(directory, migrations, true, None)
    }

    /// Creates or opens storage only for the specified nonzero application ID.
    /// Existing empty or foreign databases are never adopted. Ownership is checked
    /// before migrations, and new ownership commits with the migration version.
    pub fn open_for_application(
        directory: &Path,
        migrations: &[Migration<'_>],
        application_id: u32,
    ) -> Result<Self, StorageFailure> {
        Self::open_inner(directory, migrations, true, Some(application_id))
    }

    /// Opens registered application storage without creating a missing database.
    pub fn open_existing_for_application(
        directory: &Path,
        migrations: &[Migration<'_>],
        application_id: u32,
    ) -> Result<Self, StorageFailure> {
        Self::open_inner(directory, migrations, false, Some(application_id))
    }

    /// Reports creation by this opener's exclusive descriptor-relative create,
    /// never by an empty file, schema or missing metadata. Evidence remains bound
    /// to this live authority and does not survive disposal or reopen.
    pub fn is_new_storage(&self) -> Result<bool, StorageFailure> {
        #[cfg(any(target_os = "macos", target_os = "linux"))]
        {
            self.admit()?;
            Ok(self
                .registration
                .context
                .created
                .load(std::sync::atomic::Ordering::Relaxed))
        }
        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        {
            Err(StorageFailure::Unavailable)
        }
    }

    pub(crate) fn claim_new_storage(&mut self) -> Result<(), StorageFailure> {
        #[cfg(any(target_os = "macos", target_os = "linux"))]
        {
            self.admit()?;
            if !self
                .registration
                .context
                .created
                .swap(false, std::sync::atomic::Ordering::Relaxed)
            {
                return Err(StorageFailure::RecoveryRequired);
            }
            Ok(())
        }
        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        {
            Err(StorageFailure::Unavailable)
        }
    }

    /// Original application ID observed before any migration or policy change.
    /// Generic opening supplies no application-ownership evidence.
    pub fn pre_migration_application_id(&self) -> Option<u32> {
        #[cfg(any(target_os = "macos", target_os = "linux"))]
        {
            self.pre_migration_application_id
        }
        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        {
            None
        }
    }

    /// Checks current application ownership through the retained connection.
    pub fn application_id(&self) -> Result<u32, StorageFailure> {
        #[cfg(any(target_os = "macos", target_os = "linux"))]
        {
            self.admit()?;
            application_id(&self.connection)
                .map_err(|error| self.registration.context.failure_or(error))
        }
        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        {
            Err(StorageFailure::Unavailable)
        }
    }

    /// Verifies retained pre-migration ownership and current application binding.
    pub fn verify_application(&self, expected: u32) -> Result<(), StorageFailure> {
        #[cfg(any(target_os = "macos", target_os = "linux"))]
        {
            if self.owned_application_id.is_none() {
                return Err(StorageFailure::RecoveryRequired);
            }
            if self.owned_application_id != Some(expected) || self.application_id()? != expected {
                return Err(StorageFailure::InvalidState);
            }
            Ok(())
        }
        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        {
            let _ = expected;
            Err(StorageFailure::Unavailable)
        }
    }

    fn open_inner(
        directory: &Path,
        migrations: &[Migration<'_>],
        create: bool,
        owner: Option<u32>,
    ) -> Result<Self, StorageFailure> {
        #[cfg(any(target_os = "macos", target_os = "linux"))]
        {
            use rusqlite::{OpenFlags, config::DbConfig};
            let supported =
                u32::try_from(migrations.len()).map_err(|_| StorageFailure::InvalidState)?;
            if supported > i32::MAX as u32 {
                return Err(StorageFailure::InvalidState);
            }
            if owner.is_some_and(|id| id == 0 || id > i32::MAX as u32) {
                return Err(StorageFailure::InvalidState);
            }
            let directory = directory::Directory::open(directory)?;
            directory.check_all()?;
            if !create && directory.check(directory::Kind::Database)?.is_none() {
                return Err(StorageFailure::Unavailable);
            }
            directory.sync()?;
            let registration = vfs::Registration::new(directory)?;
            let mut flags = OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX;
            if create {
                flags |= OpenFlags::SQLITE_OPEN_CREATE;
            }
            let connection = Connection::open_with_flags_and_vfs(
                DATABASE_FILE_NAME,
                flags,
                registration.name()?,
            )
            .map_err(|error| registration.context.classify(error))?;
            let mut database = Self {
                connection,
                registration,
                recovery_required: false,
                pre_migration_application_id: None,
                owned_application_id: None,
            };
            database
                .connection
                .busy_timeout(std::time::Duration::ZERO)
                .map_err(classify_sqlite)?;
            database
                .connection
                .set_db_config(DbConfig::SQLITE_DBCONFIG_DEFENSIVE, true)
                .map_err(classify_sqlite)?;
            database.connection.execute_batch(
                "PRAGMA foreign_keys=ON; PRAGMA trusted_schema=OFF; PRAGMA temp_store=MEMORY; \
                 PRAGMA mmap_size=0; PRAGMA synchronous=FULL; PRAGMA fullfsync=ON; PRAGMA checkpoint_fullfsync=ON;"
            ).map_err(|error| database.registration.context.classify(error))?;
            let found = version(&database.connection)
                .map_err(|error| database.registration.context.failure_or(error))?;
            check_version(found, supported)?;
            let source_application = application_id(&database.connection)?;
            let created = database.is_new_storage()?;
            if let Some(expected) = owner {
                if created {
                    let objects: u64 = database
                        .connection
                        .query_row("SELECT count(*) FROM sqlite_schema", [], |row| row.get(0))
                        .map_err(classify_sqlite)?;
                    if source_application != 0 || found != 0 || objects != 0 {
                        return Err(StorageFailure::InvalidState);
                    }
                } else if source_application != expected {
                    return Err(StorageFailure::InvalidState);
                }
                database.pre_migration_application_id = Some(source_application);
            }
            validate(&database.connection)
                .map_err(|error| database.registration.context.classify(error))?;
            let mode: String = database
                .connection
                .query_row("PRAGMA journal_mode=DELETE", [], |row| row.get(0))
                .map_err(|error| database.registration.context.classify(error))?;
            if mode != "delete" {
                return Err(StorageFailure::InvalidState);
            }
            if found < supported || (owner.is_some() && created) {
                let outcome = database.transaction(|transaction| {
                    // Recheck under the writer lock: another opener may have migrated.
                    let found = schema_operation(transaction, version)?;
                    check_version(found, supported)?;
                    if let Some(expected) = owner {
                        schema_operation(transaction, |connection| {
                            let current = application_id(connection)?;
                            if current != if created { 0 } else { expected } {
                                return Err(StorageFailure::InvalidState);
                            }
                            connection
                                .pragma_update(None, "application_id", expected)
                                .map_err(classify_sqlite)
                        })?;
                    }
                    for (index, migration) in migrations.iter().enumerate().skip(found as usize) {
                        transaction.execute_batch(migration.sql)?;
                        // Only the internal schema adapter advances this marker.
                        schema_operation(transaction, |connection| {
                            connection
                                .pragma_update(None, "user_version", index + 1)
                                .map_err(classify_sqlite)
                        })?;
                    }
                    schema_operation(transaction, |connection| {
                        validate(connection).map_err(classify_sqlite)
                    })
                });
                match outcome {
                    CommitDisposition::Durable(()) => {}
                    CommitDisposition::NotApplied(error) => return Err(error),
                    CommitDisposition::Indeterminate => {
                        return Err(StorageFailure::RecoveryRequired);
                    }
                }
            }
            database.owned_application_id = owner;
            database.admit()?;
            Ok(database)
        }
        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        {
            let _ = (directory, migrations, create, owner);
            Err(StorageFailure::Unavailable)
        }
    }

    /// Checks retained handles and directory-relative identities without reopening
    /// any live database, journal, WAL, or SHM inode.
    ///
    /// # Errors
    /// Rejects changed authority, unsafe sidecars, and unavailable native inspection.
    pub fn verify_authority(&self) -> Result<(), StorageFailure> {
        #[cfg(any(target_os = "macos", target_os = "linux"))]
        {
            self.registration.context.directory.check_all()
        }
        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        {
            Err(StorageFailure::Unavailable)
        }
    }

    /// Reads the validated, one-based schema version (zero for an empty schema).
    ///
    /// # Errors
    /// Returns a bounded failure if authority changed or recovery is required.
    pub fn schema_version(&self) -> Result<u32, StorageFailure> {
        #[cfg(any(target_os = "macos", target_os = "linux"))]
        {
            self.admit()?;
            version(&self.connection).map_err(|error| self.registration.context.failure_or(error))
        }
        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        {
            Err(StorageFailure::Unavailable)
        }
    }

    /// Executes one read-only query and decodes its first row.
    ///
    /// SQL and SQLite errors are never included in the returned failure. The row
    /// decoder may read domain data, but its errors are reduced to classifications.
    ///
    /// # Errors
    /// Rejects writes, unsafe SQL capabilities, changed authority, missing/invalid
    /// rows, unavailable storage, and handles requiring recovery.
    pub fn query_row<T, P: Params, F>(
        &self,
        sql: &str,
        parameters: P,
        decode: F,
    ) -> Result<T, StorageFailure>
    where
        F: FnOnce(&Row<'_>) -> rusqlite::Result<T>,
    {
        #[cfg(any(target_os = "macos", target_os = "linux"))]
        {
            self.admit()?;
            let _guard = AuthorizationGuard::new(&self.connection, true);
            query(&self.connection, sql, parameters, decode)
                .map_err(|error| self.registration.context.classify(error))
        }
        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        {
            let _ = (sql, parameters, decode);
            Err(StorageFailure::Unavailable)
        }
    }

    /// Reads multiple tables from one SQLite snapshot with read-only SQL authority.
    /// The callback cannot write, commit, attach databases or change pragmas.
    /// A panic or uncertain I/O fences the handle until disposal and reopen.
    pub fn read_transaction<T>(
        &mut self,
        read: impl FnOnce(&Transaction<'_>) -> Result<T, StorageFailure>,
    ) -> Result<T, StorageFailure> {
        #[cfg(any(target_os = "macos", target_os = "linux"))]
        {
            self.admit()?;
            let context = &self.registration.context;
            context.reset();
            self.recovery_required = true;
            let native = match self
                .connection
                .transaction_with_behavior(rusqlite::TransactionBehavior::Deferred)
            {
                Ok(native) => native,
                Err(error) => {
                    let failure = context.classify(error);
                    if !context.uncertain.load(std::sync::atomic::Ordering::Relaxed) {
                        self.recovery_required = false;
                    }
                    return Err(failure);
                }
            };
            let result = {
                let _guard = AuthorizationGuard::new(&native, true);
                read(&Transaction {
                    connection: &native,
                    context,
                })
            };
            if !rollback(native) || context.uncertain.load(std::sync::atomic::Ordering::Relaxed) {
                return Err(StorageFailure::RecoveryRequired);
            }
            self.recovery_required = false;
            self.admit()?;
            result
        }
        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        {
            let _ = read;
            Err(StorageFailure::Unavailable)
        }
    }

    /// Runs one immediate transaction and reports its durability disposition.
    ///
    /// Callback failure rolls back. Commit/sync/write I/O uncertainty poisons the
    /// handle even if SQLite's cleanup succeeds. Only drop/reopen and recovery
    /// validation can clear that state. A caller must reconcile operation identities
    /// from recovered data rather than treating a repeat successful sync as proof.
    pub fn transaction<T>(
        &mut self,
        apply: impl FnOnce(&Transaction<'_>) -> Result<T, StorageFailure>,
    ) -> CommitDisposition<T> {
        #[cfg(any(target_os = "macos", target_os = "linux"))]
        {
            use std::sync::atomic::Ordering;
            if let Err(error) = self.admit() {
                return CommitDisposition::NotApplied(error);
            }
            let context = &self.registration.context;
            context.reset();
            // Keep the handle fenced if the callback unwinds before cleanup can
            // establish a disposition. Known outcomes below clear this fence.
            self.recovery_required = true;
            let native = match self
                .connection
                .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            {
                Ok(transaction) => transaction,
                Err(error) => {
                    let classified = context.classify(error);
                    if context.uncertain.load(Ordering::Relaxed) {
                        self.recovery_required = true;
                        return CommitDisposition::Indeterminate;
                    }
                    self.recovery_required = false;
                    return CommitDisposition::NotApplied(classified);
                }
            };
            let result = {
                let _guard = AuthorizationGuard::new(&native, false);
                apply(&Transaction {
                    connection: &native,
                    context,
                })
            };
            #[cfg(any(test, feature = "test-support"))]
            let result = if context.hit(vfs::FaultPoint::BeforeCommit) {
                Err(StorageFailure::Io)
            } else {
                result
            };
            let value = match result {
                Ok(value) => value,
                Err(error) => {
                    let rolled_back = rollback(native);
                    if !rolled_back || context.uncertain.load(Ordering::Relaxed) {
                        self.recovery_required = true;
                        return CommitDisposition::Indeterminate;
                    }
                    self.recovery_required = false;
                    return CommitDisposition::NotApplied(error);
                }
            };
            // Keep the native transaction alive until an explicit rollback has
            // also been checked. Transaction::commit's Drop cleanup discards
            // rollback errors, which cannot establish NotApplied here.
            if let Err(error) = native.execute_batch("COMMIT") {
                let classified = context.classify(error);
                let rolled_back = rollback(native);
                if !rolled_back
                    || context.uncertain.load(Ordering::Relaxed)
                    || classified == StorageFailure::Io
                {
                    return CommitDisposition::Indeterminate;
                }
                self.recovery_required = false;
                return CommitDisposition::NotApplied(classified);
            }
            drop(native);
            #[cfg(any(test, feature = "test-support"))]
            if context.hit(vfs::FaultPoint::AfterCommit) {
                context.uncertain.store(true, Ordering::Relaxed);
            }
            if context.uncertain.load(Ordering::Relaxed) {
                self.recovery_required = true;
                return CommitDisposition::Indeterminate;
            }
            if self.verify_authority().is_err() {
                self.recovery_required = true;
                return CommitDisposition::Indeterminate;
            }
            self.recovery_required = false;
            CommitDisposition::Durable(value)
        }
        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        {
            let _ = apply;
            CommitDisposition::NotApplied(StorageFailure::Unavailable)
        }
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    fn admit(&self) -> Result<(), StorageFailure> {
        if self.recovery_required
            || self
                .registration
                .context
                .uncertain
                .load(std::sync::atomic::Ordering::Relaxed)
        {
            return Err(StorageFailure::RecoveryRequired);
        }
        self.verify_authority()
    }
}

/// Restricted SQL access within an atomic database transaction.
///
/// This wrapper never exposes the native connection or independent transaction
/// control. Pragmas, ATTACH/DETACH, savepoints, and extension loading are denied.
pub struct Transaction<'a> {
    connection: &'a Connection,
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    context: &'a vfs::Context,
}

impl Transaction<'_> {
    /// Executes one statement with bound parameters.
    ///
    /// # Errors
    /// Returns only content-free SQL/storage failure classifications.
    pub fn execute<P: Params>(&self, sql: &str, parameters: P) -> Result<usize, StorageFailure> {
        self.admit()?;
        #[cfg(all(
            any(test, feature = "test-support"),
            any(target_os = "macos", target_os = "linux")
        ))]
        self.context
            .sql_calls
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.connection
            .execute(sql, parameters)
            .map_err(|error| self.classify(error))
    }

    /// Executes a batch of statements inside the existing transaction.
    ///
    /// # Errors
    /// Transaction control and unsafe capabilities are denied. Any statement
    /// failure must be propagated by the callback to roll back the whole batch.
    pub fn execute_batch(&self, sql: &str) -> Result<(), StorageFailure> {
        self.admit()?;
        #[cfg(all(
            any(test, feature = "test-support"),
            any(target_os = "macos", target_os = "linux")
        ))]
        self.context
            .sql_calls
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.connection
            .execute_batch(sql)
            .map_err(|error| self.classify(error))
    }

    /// Executes a read-only query within the transaction.
    ///
    /// # Errors
    /// Returns only bounded classifications for invalid rows, SQL, or storage.
    pub fn query_row<T, P: Params, F>(
        &self,
        sql: &str,
        parameters: P,
        decode: F,
    ) -> Result<T, StorageFailure>
    where
        F: FnOnce(&Row<'_>) -> rusqlite::Result<T>,
    {
        self.admit()?;
        #[cfg(all(
            any(test, feature = "test-support"),
            any(target_os = "macos", target_os = "linux")
        ))]
        self.context
            .sql_calls
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        query(self.connection, sql, parameters, decode).map_err(|error| self.classify(error))
    }

    fn admit(&self) -> Result<(), StorageFailure> {
        #[cfg(any(target_os = "macos", target_os = "linux"))]
        if self
            .context
            .uncertain
            .load(std::sync::atomic::Ordering::Relaxed)
        {
            return Err(StorageFailure::RecoveryRequired);
        }
        // ON CONFLICT ROLLBACK can end a transaction without explicit SQL
        // transaction control. Never let a later statement autocommit outside
        // the callback's original atomic boundary.
        if self.connection.is_autocommit() {
            return Err(StorageFailure::InvalidState);
        }
        Ok(())
    }

    fn classify(&self, error: rusqlite::Error) -> StorageFailure {
        #[cfg(any(target_os = "macos", target_os = "linux"))]
        {
            self.context.classify(error)
        }
        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        {
            classify_sqlite(error)
        }
    }
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn rollback(transaction: rusqlite::Transaction<'_>) -> bool {
    // SQLite may already have rolled back after a constraint failure. This is
    // known NotApplied only when the VFS has NOT recorded I/O uncertainty.
    if transaction.is_autocommit() {
        true
    } else {
        transaction.rollback().is_ok()
    }
}

fn query<T, P: Params, F>(
    connection: &Connection,
    sql: &str,
    parameters: P,
    decode: F,
) -> rusqlite::Result<T>
where
    F: FnOnce(&Row<'_>) -> rusqlite::Result<T>,
{
    let mut statement = connection.prepare(sql)?;
    if !statement.readonly() {
        return Err(rusqlite::Error::InvalidQuery);
    }
    statement.query_row(parameters, decode)
}

fn classify_sqlite(error: rusqlite::Error) -> StorageFailure {
    use rusqlite::ErrorCode;
    match error {
        rusqlite::Error::SqliteFailure(error, _) => match error.code {
            ErrorCode::DatabaseBusy
            | ErrorCode::DatabaseLocked
            | ErrorCode::PermissionDenied
            | ErrorCode::CannotOpen
            | ErrorCode::ReadOnly
            | ErrorCode::DiskFull => StorageFailure::Unavailable,
            ErrorCode::SystemIoFailure => StorageFailure::Io,
            _ => StorageFailure::InvalidState,
        },
        _ => StorageFailure::InvalidState,
    }
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn version(connection: &Connection) -> Result<u32, StorageFailure> {
    let found: i64 = connection
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .map_err(classify_sqlite)?;
    u32::try_from(found).map_err(|_| StorageFailure::InvalidState)
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn application_id(connection: &Connection) -> Result<u32, StorageFailure> {
    let found: i64 = connection
        .query_row("PRAGMA application_id", [], |row| row.get(0))
        .map_err(classify_sqlite)?;
    u32::try_from(found).map_err(|_| StorageFailure::InvalidState)
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn check_version(found: u32, supported: u32) -> Result<(), StorageFailure> {
    if found > supported {
        Err(StorageFailure::UnknownSchema { found, supported })
    } else {
        Ok(())
    }
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn validate(connection: &Connection) -> rusqlite::Result<()> {
    let mut integrity = connection.prepare("PRAGMA integrity_check")?;
    let mut rows = integrity.query([])?;
    let Some(row) = rows.next()? else {
        return Err(rusqlite::Error::InvalidQuery);
    };
    if row.get::<_, String>(0)? != "ok" || rows.next()?.is_some() {
        return Err(rusqlite::Error::InvalidQuery);
    }
    let mut foreign = connection.prepare("PRAGMA foreign_key_check")?;
    if foreign.query([])?.next()?.is_some() {
        return Err(rusqlite::Error::InvalidQuery);
    }
    Ok(())
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn authorize(connection: &Connection, readonly: bool) {
    use rusqlite::hooks::{AuthAction, AuthContext, Authorization};
    connection.authorizer(Some(move |context: AuthContext<'_>| {
        let denied = match context.action {
            AuthAction::Pragma { .. }
            | AuthAction::Attach { .. }
            | AuthAction::Detach { .. }
            | AuthAction::Transaction { .. }
            | AuthAction::Savepoint { .. }
            | AuthAction::Unknown { .. } => true,
            AuthAction::Function { function_name }
                if function_name.eq_ignore_ascii_case("load_extension") =>
            {
                true
            }
            AuthAction::Read { .. }
            | AuthAction::Select
            | AuthAction::Function { .. }
            | AuthAction::Recursive => false,
            _ => readonly,
        };
        if denied {
            Authorization::Deny
        } else {
            Authorization::Allow
        }
    }));
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn clear_authorizer(connection: &Connection) {
    connection
        .authorizer(None::<fn(rusqlite::hooks::AuthContext<'_>) -> rusqlite::hooks::Authorization>);
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn schema_operation<T>(
    transaction: &Transaction<'_>,
    operation: impl FnOnce(&Connection) -> Result<T, StorageFailure>,
) -> Result<T, StorageFailure> {
    clear_authorizer(transaction.connection);
    let result = operation(transaction.connection);
    authorize(transaction.connection, false);
    result.map_err(|error| transaction.context.failure_or(error))
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
struct AuthorizationGuard<'a>(&'a Connection);
#[cfg(any(target_os = "macos", target_os = "linux"))]
impl<'a> AuthorizationGuard<'a> {
    fn new(connection: &'a Connection, readonly: bool) -> Self {
        authorize(connection, readonly);
        Self(connection)
    }
}
#[cfg(any(target_os = "macos", target_os = "linux"))]
impl Drop for AuthorizationGuard<'_> {
    fn drop(&mut self) {
        clear_authorizer(self.0);
    }
}
