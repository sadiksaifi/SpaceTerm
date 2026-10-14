use super::{
    StorageFailure,
    directory::{Directory as StorageDirectory, Identity, Kind},
};
use crate::private_filesystem::{Directory, Failure};
use serde::{Deserialize, Serialize};

const RECORD: &str = "organization-creation";
const LIMIT: usize = 4096;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Record {
    schema: u16,
    binding: [u8; 32],
    application: u32,
    directory: Identity,
    database: Option<Identity>,
    complete: bool,
}

/// Authority retained only by the original confirmed first-install request.
/// Its durable record binds the organization directory and the created inode.
pub(crate) struct InitialCreation {
    records: Directory,
    binding: [u8; 32],
}
impl InitialCreation {
    pub(crate) fn new(records: Directory, binding: [u8; 32]) -> Self {
        Self { records, binding }
    }
    pub(crate) fn reconcile(
        path: &std::path::Path,
        binding: [u8; 32],
    ) -> Result<(), crate::private_filesystem::Failure> {
        Directory::reconcile_publication(path, RECORD, LIMIT, |bytes| {
            let record: Record = serde_json::from_slice(bytes).map_err(|_| Failure::Unsafe)?;
            if record.schema != 1
                || record.binding != binding
                || record.application != crate::organization_store::schema::APPLICATION_ID
            {
                return Err(Failure::Unsafe);
            }
            Ok(())
        })
    }
    fn read(
        &self,
    ) -> Result<Option<(Record, crate::private_filesystem::Snapshot)>, StorageFailure> {
        self.records
            .read(RECORD, LIMIT)
            .map_err(classify)?
            .map(|snapshot| {
                let record: Record = serde_json::from_slice(&snapshot.bytes)
                    .map_err(|_| StorageFailure::InvalidState)?;
                if record.schema != 1 || record.binding != self.binding {
                    return Err(StorageFailure::InvalidState);
                }
                Ok((record, snapshot))
            })
            .transpose()
    }
    /// Publication precedes SQLite creation. An existing database without this
    /// original creation record is never admitted, even when empty.
    pub(super) fn prepare(
        &self,
        directory: &StorageDirectory,
        application: u32,
    ) -> Result<(), StorageFailure> {
        let database = directory.check(Kind::Database)?;
        if let Some((record, _)) = self.read()? {
            if record.application != application
                || record.directory != directory.identity()?
                || (record.database.is_some() && record.database != database)
                || (record.complete && database.is_none())
            {
                return Err(StorageFailure::InvalidState);
            }
            return Ok(());
        }
        if database.is_some() {
            // Without an original creation record, existing storage is not this
            // request's incomplete creation. Refuse it without initialization.
            return Err(StorageFailure::InvalidState);
        }
        let record = Record {
            schema: 1,
            binding: self.binding,
            application,
            directory: directory.identity()?,
            database: None,
            complete: false,
        };
        let bytes = serde_json::to_vec(&record).map_err(|_| StorageFailure::InvalidState)?;
        self.records.publish(RECORD, &bytes, None).map_err(classify)
    }
    /// Captures creation through the VFS authority before any schema mutation.
    pub(super) fn bind(&self, directory: &StorageDirectory) -> Result<bool, StorageFailure> {
        let (mut record, snapshot) = self.read()?.ok_or(StorageFailure::RecoveryRequired)?;
        let database = directory
            .check(Kind::Database)?
            .ok_or(StorageFailure::RecoveryRequired)?;
        if record.directory != directory.identity()?
            || record.database.is_some_and(|old| old != database)
        {
            return Err(StorageFailure::UnsafeStorage);
        }
        if record.database.is_none() {
            record.database = Some(database);
            let bytes = serde_json::to_vec(&record).map_err(|_| StorageFailure::InvalidState)?;
            self.records
                .publish(RECORD, &bytes, Some(&snapshot))
                .map_err(classify)?;
        }
        Ok(!record.complete)
    }
    /// Seals first-creation authority after the initialized organization validates.
    pub(crate) fn complete(&self) -> Result<(), StorageFailure> {
        let (mut record, snapshot) = self.read()?.ok_or(StorageFailure::RecoveryRequired)?;
        if record.complete {
            return Ok(());
        }
        if record.database.is_none() {
            return Err(StorageFailure::RecoveryRequired);
        }
        record.complete = true;
        let bytes = serde_json::to_vec(&record).map_err(|_| StorageFailure::InvalidState)?;
        self.records
            .publish(RECORD, &bytes, Some(&snapshot))
            .map_err(classify)
    }
}
fn classify(failure: Failure) -> StorageFailure {
    match failure {
        Failure::Unsafe | Failure::Conflict | Failure::AlreadyExists => {
            StorageFailure::UnsafeStorage
        }
        Failure::Indeterminate => StorageFailure::RecoveryRequired,
        Failure::Missing | Failure::Unavailable | Failure::Busy => StorageFailure::Unavailable,
        Failure::LimitExceeded => StorageFailure::InvalidState,
    }
}
