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

    /// Only original creation recovery can finish a pending record publication.
    /// Validate its application and actual database object before replaying it.
    pub(crate) fn reconcile(
        records_path: &std::path::Path,
        database_path: &std::path::Path,
        binding: [u8; 32],
        application: u32,
    ) -> Result<(), Failure> {
        let directory = StorageDirectory::open(database_path).map_err(recovery_failure)?;
        directory.check_all().map_err(recovery_failure)?;
        let records = Directory::open_for_recovery(records_path)?.ok_or(Failure::Missing)?;
        if let Some(snapshot) = records.read_for_recovery(RECORD, LIMIT)? {
            if snapshot.bytes.len() > LIMIT {
                return Err(Failure::LimitExceeded);
            }
            let record: Record =
                serde_json::from_slice(&snapshot.bytes).map_err(|_| Failure::Unsafe)?;
            if record.schema != 1 || record.binding != binding {
                return Err(Failure::Unsafe);
            }
            validate_object(&record, &directory, application).map_err(recovery_failure)?;
        } else if directory
            .check(Kind::Database)
            .map_err(recovery_failure)?
            .is_some()
        {
            return Err(Failure::Unsafe);
        }
        // The private publication engine can close a proven pre-effect phase
        // without invoking its candidate validator. Admit the current record
        // above as well, so that path cannot clear a foreign object's fence.
        Directory::reconcile_publication(records_path, RECORD, LIMIT, |bytes| {
            let record: Record = serde_json::from_slice(bytes).map_err(|_| Failure::Unsafe)?;
            if record.schema != 1 || record.binding != binding {
                return Err(Failure::Unsafe);
            }
            validate_object(&record, &directory, application).map_err(recovery_failure)
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

    /// Intent precedes creation, but grants no authority over an existing inode.
    pub(super) fn prepare(
        &self,
        directory: &StorageDirectory,
        application: u32,
    ) -> Result<(), StorageFailure> {
        if let Some((record, _)) = self.read()? {
            return validate_object(&record, directory, application);
        }
        if directory.check(Kind::Database)?.is_some() {
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

    /// Retain the exclusive-created object's identity before publishing its fixed
    /// database name. A crash before identity publication leaves only an inert
    /// staging object, never an existing database that a later opener may adopt.
    /// Fresh allocation consumes a bounded namespace budget. Exhaustion refuses
    /// without deleting unbound objects; an already-bound object needs no slot.
    pub(super) fn publish_database(
        &self,
        directory: &StorageDirectory,
    ) -> Result<(), StorageFailure> {
        let (mut record, snapshot) = self.read()?.ok_or(StorageFailure::RecoveryRequired)?;
        validate_object(&record, directory, record.application)?;
        let database = match record.database {
            Some(database) => database,
            None => {
                let database = directory.create_initial_database()?;
                #[cfg(feature = "test-support")]
                super::testing::initialization_boundary(7)?;
                record.database = Some(database);
                let bytes =
                    serde_json::to_vec(&record).map_err(|_| StorageFailure::InvalidState)?;
                self.records
                    .publish(RECORD, &bytes, Some(&snapshot))
                    .map_err(classify)?;
                database
            }
        };
        #[cfg(feature = "test-support")]
        super::testing::initialization_boundary(8)?;
        directory.publish_initial_database(database)
    }

    /// An existing fixed database is admitted only by its already-durable binding.
    pub(super) fn bind(&self, directory: &StorageDirectory) -> Result<bool, StorageFailure> {
        let (record, _) = self.read()?.ok_or(StorageFailure::RecoveryRequired)?;
        if record.directory != directory.identity()?
            || record.database.is_none()
            || record.database != directory.check(Kind::Database)?
        {
            return Err(StorageFailure::UnsafeStorage);
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
        #[cfg(feature = "test-support")]
        super::testing::initialization_boundary(9)?;
        record.complete = true;
        let bytes = serde_json::to_vec(&record).map_err(|_| StorageFailure::InvalidState)?;
        self.records
            .publish(RECORD, &bytes, Some(&snapshot))
            .map_err(classify)
    }
}

fn validate_object(
    record: &Record,
    directory: &StorageDirectory,
    application: u32,
) -> Result<(), StorageFailure> {
    if record.application != application || record.directory != directory.identity()? {
        return Err(StorageFailure::InvalidState);
    }
    match record.database {
        Some(database) => directory.verify_initial_database(database, !record.complete),
        None if !record.complete && directory.check(Kind::Database)?.is_none() => Ok(()),
        _ => Err(StorageFailure::InvalidState),
    }
}

fn recovery_failure(failure: StorageFailure) -> Failure {
    match failure {
        StorageFailure::Unavailable | StorageFailure::Io => Failure::Unavailable,
        StorageFailure::RecoveryRequired => Failure::Indeterminate,
        StorageFailure::UnsafeStorage
        | StorageFailure::InvalidState
        | StorageFailure::UnknownSchema { .. } => Failure::Unsafe,
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
