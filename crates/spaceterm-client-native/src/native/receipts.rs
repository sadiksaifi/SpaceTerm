//! Durable CLI results precede owner settlement and output. The latest 1024
//! settled results remain recoverable by original ID; unsettled receipts are
//! never evicted. The host supplies account/build-isolated local storage authority.

use serde::{Deserialize, Serialize};
use spaceterm_client::management_cli::ManagementFailure;
use spaceterm_core::{
    operation_ledger_contracts::CompactOutcome,
    operations::{OperationBinding, OperationId},
};
use spaceterm_host::{
    account_service::{ServiceProgress, ServiceSupervisor},
    native_deployment::contracts::NativeRequestId,
    storage::{CommitDisposition, Database, Migration, StorageFailure},
};

const APPLICATION_ID: u32 = 0x53544352;
const MAX_RECEIPTS: usize = 1024;
const MAX_BYTES: usize = 64 * 1024;
const MIGRATIONS: &[Migration<'_>] = &[Migration {
    sql: "CREATE TABLE cli_results(ordinal INTEGER PRIMARY KEY, key TEXT NOT NULL UNIQUE, payload BLOB NOT NULL CHECK(length(payload) BETWEEN 1 AND 65536), settled INTEGER NOT NULL CHECK(settled IN (0,1))) STRICT",
}];

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub(super) enum Receipt {
    Native(ServiceProgress),
    Protocol {
        binding: OperationBinding,
        outcome: CompactOutcome,
    },
}
impl Receipt {
    pub fn key(&self) -> Result<String, ManagementFailure> {
        match self {
            Self::Native(progress) => Ok(native_key(progress.request)),
            Self::Protocol { binding, .. } => protocol_key(binding.id),
        }
    }
}
pub(super) fn native_key(request: NativeRequestId) -> String {
    format!("native_{}", crate::cli::output::hex(request.as_bytes()))
}
pub(super) fn protocol_key(operation: OperationId) -> Result<String, ManagementFailure> {
    serde_json::to_string(&operation)
        .map(|id| format!("protocol_{id}"))
        .map_err(|_| ManagementFailure::UnsafeNativeState)
}

fn database(
    supervisor: &ServiceSupervisor,
    create: bool,
) -> Result<Option<Database>, ManagementFailure> {
    let Some(directory) = supervisor
        .cli_result_directory(create)
        .map_err(super::classify_service)?
    else {
        return Ok(None);
    };
    let result = if create {
        Database::open_for_application(&directory, MIGRATIONS, APPLICATION_ID)
    } else {
        Database::open_existing_for_application(&directory, MIGRATIONS, APPLICATION_ID)
    };
    result.map(Some).map_err(classify)
}
fn decode(bytes: &[u8]) -> Result<Receipt, ManagementFailure> {
    if bytes.len() > MAX_BYTES {
        return Err(ManagementFailure::LimitExceeded);
    }
    serde_json::from_slice(bytes).map_err(|_| ManagementFailure::UnsafeNativeState)
}
pub(super) fn load(
    supervisor: &ServiceSupervisor,
    key: &str,
) -> Result<Option<Receipt>, ManagementFailure> {
    let Some(database) = database(supervisor, false)? else {
        return Ok(None);
    };
    let bytes: Option<Vec<u8>> = database
        .query_row(
            "SELECT (SELECT payload FROM cli_results WHERE key=?1)",
            [key],
            |row| row.get(0),
        )
        .map_err(classify)?;
    let result = bytes.as_deref().map(decode).transpose()?;
    if result
        .as_ref()
        .is_some_and(|receipt| receipt.key().as_deref() != Ok(key))
    {
        return Err(ManagementFailure::UnsafeNativeState);
    }
    Ok(result)
}
pub(super) fn retain(
    supervisor: &ServiceSupervisor,
    receipt: &Receipt,
) -> Result<(), ManagementFailure> {
    let key = receipt.key()?;
    let bytes = serde_json::to_vec(receipt).map_err(|_| ManagementFailure::UnsafeNativeState)?;
    if bytes.len() > MAX_BYTES {
        return Err(ManagementFailure::LimitExceeded);
    }
    let mut database = database(supervisor, true)?.ok_or(ManagementFailure::StorageUnavailable)?;
    durable(database.transaction(|tx| {
        let original: Option<Vec<u8>> = tx.query_row("SELECT (SELECT payload FROM cli_results WHERE key=?1)", [&key], |row| row.get(0))?;
        if let Some(original) = original {
            if original != bytes { return Err(StorageFailure::InvalidState); }
            return Ok(());
        }
        let count: usize = tx.query_row("SELECT count(*) FROM cli_results", (), |row| row.get(0))?;
        if count >= MAX_RECEIPTS
            && tx.execute("DELETE FROM cli_results WHERE ordinal=(SELECT min(ordinal) FROM cli_results WHERE settled=1)", ())? != 1 {
            return Err(StorageFailure::Unavailable);
        }
        tx.execute("INSERT INTO cli_results(key,payload,settled) VALUES(?1,?2,0)", (key.as_str(), bytes.as_slice()))?;
        Ok(())
    }))
}
pub(super) fn unsettled(supervisor: &ServiceSupervisor) -> Result<Vec<Receipt>, ManagementFailure> {
    let Some(database) = database(supervisor, false)? else {
        return Ok(Vec::new());
    };
    let keys: String = database.query_row("SELECT COALESCE(json_group_array(key),'[]') FROM (SELECT key FROM cli_results WHERE settled=0 ORDER BY ordinal)", (), |row| row.get(0)).map_err(classify)?;
    let keys: Vec<String> =
        serde_json::from_str(&keys).map_err(|_| ManagementFailure::UnsafeNativeState)?;
    if keys.len() > MAX_RECEIPTS {
        return Err(ManagementFailure::LimitExceeded);
    }
    drop(database);
    keys.iter()
        .map(|key| load(supervisor, key)?.ok_or(ManagementFailure::UnsafeNativeState))
        .collect()
}
pub(super) fn settle(
    supervisor: &ServiceSupervisor,
    receipt: &Receipt,
) -> Result<(), ManagementFailure> {
    let key = receipt.key()?;
    let mut database = database(supervisor, false)?.ok_or(ManagementFailure::StorageUnavailable)?;
    durable(database.transaction(|tx| {
        if tx.execute("UPDATE cli_results SET settled=1 WHERE key=?1", [key])? != 1 {
            return Err(StorageFailure::InvalidState);
        }
        Ok(())
    }))
}
fn durable(result: CommitDisposition) -> Result<(), ManagementFailure> {
    match result {
        CommitDisposition::Durable(()) => Ok(()),
        CommitDisposition::NotApplied(failure) => Err(classify(failure)),
        CommitDisposition::Indeterminate => Err(ManagementFailure::Indeterminate),
    }
}
fn classify(failure: StorageFailure) -> ManagementFailure {
    match failure {
        StorageFailure::Unavailable | StorageFailure::Io => ManagementFailure::StorageUnavailable,
        StorageFailure::RecoveryRequired => ManagementFailure::Indeterminate,
        _ => ManagementFailure::UnsafeNativeState,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use spaceterm_host::{
        account_service::{
            ServiceOutcome, ServicePhase, ServiceRequestState, test_support::AccountServiceFixture,
        },
        startup::BuildIdentity,
    };
    fn receipt(number: u64) -> Receipt {
        let mut bytes = [1; 16];
        bytes[8..].copy_from_slice(&number.to_be_bytes());
        Receipt::Native(ServiceProgress {
            request: NativeRequestId::new(bytes).unwrap(),
            request_digest: spaceterm_core::operations::RequestDigest::from_bytes([2; 32]),
            phase: ServicePhase::Completed,
            state: ServiceRequestState::Completed(ServiceOutcome::Started),
        })
    }
    #[test]
    fn receipt_capacity_never_evicts_unsettled_results() {
        for settled in [false, true] {
            let root = tempfile::tempdir().unwrap();
            let account =
                AccountServiceFixture::new(root.path(), BuildIdentity::Development).unwrap();
            let supervisor = account.reopen();
            let mut database = database(&supervisor, true).unwrap().unwrap();
            let seeded = database.transaction(|tx| {
                for number in 1..=MAX_RECEIPTS as u64 {
                    let result = receipt(number);
                    let bytes = serde_json::to_vec(&result).unwrap();
                    tx.execute(
                        "INSERT INTO cli_results(key,payload,settled) VALUES(?1,?2,?3)",
                        (
                            result.key().unwrap(),
                            bytes,
                            i64::from(settled && number != 1),
                        ),
                    )?;
                }
                Ok(())
            });
            assert_eq!(seeded, CommitDisposition::Durable(()));
            drop(database);
            let candidate = receipt(MAX_RECEIPTS as u64 + 1);
            let result = retain(&supervisor, &candidate);
            if settled {
                assert_eq!(result, Ok(()));
                assert_eq!(
                    load(&account.reopen(), &receipt(2).key().unwrap()).unwrap(),
                    None
                );
                assert_eq!(
                    load(&account.reopen(), &candidate.key().unwrap()).unwrap(),
                    Some(candidate)
                );
            } else {
                assert_eq!(result, Err(ManagementFailure::StorageUnavailable));
                assert_eq!(
                    load(&account.reopen(), &receipt(2).key().unwrap()).unwrap(),
                    Some(receipt(2))
                );
            }
            assert_eq!(
                load(&account.reopen(), &receipt(1).key().unwrap()).unwrap(),
                Some(receipt(1))
            );
        }
    }
    #[test]
    fn conflicting_receipt_does_not_replace_original_lost_output() {
        let root = tempfile::tempdir().unwrap();
        let account = AccountServiceFixture::new(root.path(), BuildIdentity::Development).unwrap();
        let original = receipt(1);
        retain(&account.reopen(), &original).unwrap();
        let mut changed = original.clone();
        let Receipt::Native(progress) = &mut changed else {
            unreachable!()
        };
        progress.state = ServiceRequestState::Completed(ServiceOutcome::Stopped);
        assert_eq!(
            retain(&account.reopen(), &changed),
            Err(ManagementFailure::UnsafeNativeState)
        );
        assert_eq!(
            load(&account.reopen(), &original.key().unwrap()).unwrap(),
            Some(original)
        );
    }
}
