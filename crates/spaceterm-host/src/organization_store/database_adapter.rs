//! Translation of the authority's dispositions, never a second SQLite opener.

use super::{StoreIdentity, schema};
use crate::storage::{
    CommitDisposition as NativeDisposition, Database, StorageFailure, Transaction,
};
use spaceterm_core::storage::{CommitDisposition, CommitReceipt, StoreFailure};

pub(super) struct DatabaseAdapter {
    database: Database,
    #[cfg(feature = "test-support")]
    fixture_snapshot: bool,
}
impl DatabaseAdapter {
    #[cfg(feature = "test-support")]
    pub(super) fn test_fixture(database: Database) -> Self {
        Self {
            database,
            fixture_snapshot: true,
        }
    }

    pub(super) fn snapshot<T>(
        &mut self,
        read: impl FnOnce(&Transaction<'_>) -> Result<T, StoreFailure>,
    ) -> Result<T, StoreFailure> {
        #[cfg(feature = "test-support")]
        if self.fixture_snapshot {
            return match transact(&mut self.database, read) {
                Disposition::Durable(value) => Ok(value),
                Disposition::NotApplied(error) => Err(error),
                Disposition::Indeterminate => Err(StoreFailure::RecoveryRequired),
            };
        }
        // Database currently has only independent query_row calls. They do not
        // establish a consistent multi-table recovery snapshot (issue #520).
        let _ = read;
        Err(StoreFailure::RecoveryRequired)
    }

    pub(super) fn commit(
        &mut self,
        apply: impl FnOnce(&Transaction<'_>) -> Result<CommitReceipt, StoreFailure>,
    ) -> CommitDisposition {
        match transact(&mut self.database, apply) {
            Disposition::Durable(receipt) => CommitDisposition::Durable(receipt),
            Disposition::NotApplied(error) => CommitDisposition::NotApplied(error),
            Disposition::Indeterminate => CommitDisposition::Indeterminate,
        }
    }
}

enum Disposition<T> {
    Durable(T),
    NotApplied(StoreFailure),
    Indeterminate,
}

fn transact<T>(
    database: &mut Database,
    apply: impl FnOnce(&Transaction<'_>) -> Result<T, StoreFailure>,
) -> Disposition<T> {
    let mut rejection = None;
    let native = database.transaction(|sql| match apply(sql) {
        Ok(value) => Ok(value),
        Err(error) => {
            rejection = Some(error);
            // Preserve the domain classification only after confirmed rollback.
            // Indeterminate must override it, even for a caller-side rejection.
            Err(StorageFailure::InvalidState)
        }
    });
    match native {
        NativeDisposition::Durable(value) => Disposition::Durable(value),
        NativeDisposition::NotApplied(error) => {
            Disposition::NotApplied(rejection.unwrap_or_else(|| classify(error)))
        }
        NativeDisposition::Indeterminate => Disposition::Indeterminate,
    }
}

pub(super) fn classify(error: StorageFailure) -> StoreFailure {
    match error {
        StorageFailure::Unavailable => StoreFailure::Unavailable,
        StorageFailure::UnsafeStorage => StoreFailure::UnsafeStorage,
        StorageFailure::UnknownSchema { .. } => StoreFailure::UnknownSchema,
        StorageFailure::InvalidState => StoreFailure::InvalidDurableState,
        StorageFailure::Io => StoreFailure::Io,
        StorageFailure::RecoveryRequired => StoreFailure::RecoveryRequired,
    }
}

pub(super) fn check_existing_prerequisites(
    database: &Database,
    expected: StoreIdentity,
) -> Result<(), StoreFailure> {
    database.verify_authority().map_err(classify)?;
    schema::check_version(database.schema_version().map_err(classify)?).map_err(classify)?;
    if expected.server.bytes() == [0; 16]
        || expected.server_public_key == [0; 32]
        || expected.account.account_uid == 0
        || expected.account.account_uid == u32::MAX
    {
        return Err(StoreFailure::IdentityMismatch);
    }
    // Do not call schema::validate_existing: the current authority forbids the
    // application-ID read and lacks pre-migration ownership evidence. Metadata
    // containing our application ID is not a substitute (issue #520).
    Err(StoreFailure::RecoveryRequired)
}

#[cfg(feature = "test-support")]
pub(super) fn check_test_fixture(database: &Database) -> Result<(), StoreFailure> {
    schema::check_version(database.schema_version().map_err(classify)?).map_err(classify)?;
    if database.schema_version().map_err(classify)? != schema::SCHEMA_VERSION {
        return Err(StoreFailure::InvalidDurableState);
    }
    schema::validate_inventory(database).map_err(classify)
}
