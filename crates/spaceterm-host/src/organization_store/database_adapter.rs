//! Translation of the authority's dispositions, never a second SQLite opener.

use super::{StoreIdentity, schema};
use crate::storage::{
    CommitDisposition as NativeDisposition, Database, StorageFailure, Transaction,
};
use spaceterm_core::storage::{CommitDisposition, CommitReceipt, StoreFailure};

pub(super) struct DatabaseAdapter {
    pub(super) database: Database,
}
impl DatabaseAdapter {
    pub(super) fn new(database: Database) -> Self {
        Self { database }
    }

    pub(super) fn snapshot<T>(
        &mut self,
        read: impl FnOnce(&Transaction<'_>) -> Result<T, StoreFailure>,
    ) -> Result<T, StoreFailure> {
        let mut rejection = None;
        let result = self.database.read_transaction(|sql| match read(sql) {
            Ok(value) => Ok(value),
            Err(error) => {
                rejection = Some(error);
                Err(StorageFailure::InvalidState)
            }
        });
        result.map_err(|error| {
            if error == StorageFailure::RecoveryRequired {
                StoreFailure::RecoveryRequired
            } else {
                rejection.unwrap_or_else(|| classify(error))
            }
        })
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
    schema::validate_existing(database).map_err(classify)
}

#[cfg(feature = "test-support")]
pub(super) fn check_test_fixture(database: &Database) -> Result<(), StoreFailure> {
    schema::check_version(database.schema_version().map_err(classify)?).map_err(classify)?;
    if database.schema_version().map_err(classify)? != schema::SCHEMA_VERSION {
        return Err(StoreFailure::InvalidDurableState);
    }
    schema::validate_inventory(database).map_err(classify)
}
