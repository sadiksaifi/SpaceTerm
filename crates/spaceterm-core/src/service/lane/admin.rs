//! Account-private storage for pending service operations and their settled outcomes.
use crate::{CoreError, Directory, LockWait};
use spaceterm_client::RecoveryGuard;
use std::time::Instant;

use super::client::LaneError;
pub use spaceterm_client::service::admin::*;

/// The record of one route and identity's admitted operations, kept in `directory`.
const PENDING: &str = "service-operations.json";
const PENDING_FILE: &std::ffi::CStr = c"service-operations.json";
/// The outcomes one route and identity acknowledged, kept beside its pending operations so a
/// surface that asks about one after SpaceTerm Core forgot its receipt still reads it.
const OUTCOMES: &str = "service-outcomes.json";
const OUTCOMES_FILE: &std::ffi::CStr = c"service-outcomes.json";
/// Held across each read-modify-write of the pending operations or the kept outcomes.
const RECORD_LOCK: &str = "service-operations.lock";
/// Held shared by each flow that follows an operation it recorded, and exclusively while a lane
/// connection settles the identity's leftovers, so that settlement never acknowledges a receipt
/// a running flow still waits to read.
const FOLLOWING: &str = "service-operations-following.lock";
struct PrivateOperations(Directory);

pub fn pending_operations(directory: Directory) -> PendingOperations {
    PendingOperations::new(Box::new(PrivateOperations(directory)))
}

impl OperationStorage for PrivateOperations {
    fn lock(&self, deadline: Instant) -> Result<Box<dyn RecoveryGuard>, LaneError> {
        waited(self.0.wait_lock(
            RECORD_LOCK,
            deadline.saturating_duration_since(Instant::now()),
            None,
        ))
    }

    fn read(&self, record: OperationRecords) -> Result<Option<Vec<u8>>, CoreError> {
        self.0.read_bytes(match record {
            OperationRecords::Pending => PENDING,
            OperationRecords::Outcomes => OUTCOMES,
        })
    }

    fn write(&self, record: OperationRecords, bytes: &[u8]) -> Result<(), CoreError> {
        self.0.write_bytes(
            match record {
                OperationRecords::Pending => PENDING_FILE,
                OperationRecords::Outcomes => OUTCOMES_FILE,
            },
            bytes,
        )
    }

    fn follow(&self, deadline: Instant) -> Result<Box<dyn RecoveryGuard>, LaneError> {
        waited(
            self.0
                .wait_lock_shared(
                    FOLLOWING,
                    deadline.saturating_duration_since(Instant::now()),
                )
                .map_err(|_| CoreError::StorageUnavailable),
        )
    }

    fn try_settle(&self) -> Result<Option<Box<dyn RecoveryGuard>>, CoreError> {
        self.0
            .try_lock(FOLLOWING)
            .map(|lock| lock.map(|lock| Box::new(lock) as Box<dyn RecoveryGuard>))
    }
}

fn waited(result: Result<LockWait, CoreError>) -> Result<Box<dyn RecoveryGuard>, LaneError> {
    match result {
        Ok(LockWait::Locked(lock)) => Ok(Box::new(lock)),
        Ok(LockWait::Held | LockWait::Cancelled) => Err(LaneError::Deadline),
        Err(error) => Err(LaneError::Core(error)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_pending_record_keeps_at_most_sixteen_operations_and_forgets_removed_ones() {
        // The record refuses a path through a symbolic link, as macOS's temporary directory is.
        let root = std::env::temp_dir().canonicalize().unwrap().join(format!(
            "spaceterm-pending-{}",
            crate::service::operations::random_operation_id().unwrap()
        ));
        let pending = pending_operations(Directory::open(&root, true).unwrap());
        let ids: Vec<_> = (0..16)
            .map(|_| crate::service::operations::random_operation_id().unwrap())
            .collect();
        for id in &ids {
            pending.add(id).unwrap();
        }
        assert_eq!(
            pending.add(&crate::service::operations::random_operation_id().unwrap()),
            Err(CoreError::Capacity)
        );
        pending.remove(&ids[0].to_string()).unwrap();
        let listed = pending.list().unwrap();
        assert_eq!(listed.len(), 16 - 1);
        assert!(!listed.contains(&ids[0].to_string()));
        let _ = std::fs::remove_dir_all(root);
    }
}
