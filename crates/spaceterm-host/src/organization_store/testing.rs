//! Private-fixture initialization through the real authority, available only in
//! test composition. This fixture entry point bypasses application-opening evidence;
//! production initialization requires the real authority evidence.

use super::bootstrap::{InitializationDisposition, NewStoreSeed, validate_identity};
use super::{SqliteOrganizationStore, StoreLimits, StoreServices};
use crate::storage::Database;

impl SqliteOrganizationStore {
    /// Initializes an explicitly supplied private test fixture. Complete empty
    /// inventory and resulting-state validation run under the same authority
    /// writer lock. Production code cannot call this feature-gated entry point.
    pub fn initialize_test_fixture(
        database: Database,
        seed: NewStoreSeed,
        limits: StoreLimits,
        services: StoreServices,
    ) -> InitializationDisposition {
        if let Err(error) = validate_identity(seed.identity)
            .and_then(|()| super::database_adapter::check_test_fixture(&database))
        {
            return InitializationDisposition::NotApplied(error);
        }
        Self::initialize_validated(database, seed, limits, services)
    }
}

/// Explicit organization-only fake of the frozen domain ports. It rejects every
/// operation and every nonempty auxiliary table. It is never production validation.
pub fn organization_only_services() -> StoreServices {
    StoreServices::new(Box::new(EmptyDomains), Box::new(EmptyDomains))
}
struct EmptyDomains;
impl spaceterm_core::storage::DurabilityValidator for EmptyDomains {
    fn validate_transaction_projection(
        &self,
        parts: &spaceterm_core::storage::TransactionParts,
    ) -> Result<(), spaceterm_core::storage::StoreFailure> {
        if parts.operation_writes.is_empty() {
            Ok(())
        } else {
            Err(spaceterm_core::storage::StoreFailure::RecoveryRequired)
        }
    }
    fn validate_recovery(
        &self,
        value: &spaceterm_core::storage::RecoveredState,
    ) -> Result<(), spaceterm_core::storage::StoreFailure> {
        value
            .organization
            .retained
            .as_ref()
            .ok_or(spaceterm_core::storage::StoreFailure::InvalidDurableState)?
            .validate_against(&value.organization)
            .map_err(|_| spaceterm_core::storage::StoreFailure::InvalidDurableState)
    }
}
impl super::OperationProjection for EmptyDomains {
    fn preflight(
        &self,
        writes: &[spaceterm_core::storage::OperationWrite],
        _: &mut super::limits::SizeBudget,
    ) -> Result<usize, spaceterm_core::storage::StoreFailure> {
        if writes.is_empty() {
            Ok(0)
        } else {
            Err(spaceterm_core::storage::StoreFailure::RecoveryRequired)
        }
    }
    fn apply(
        &self,
        _: &crate::storage::Transaction<'_>,
        parts: &spaceterm_core::storage::TransactionParts,
        _: &spaceterm_core::storage::RecoveredState,
    ) -> Result<usize, spaceterm_core::storage::StoreFailure> {
        if parts.operation_writes.is_empty() {
            Ok(0)
        } else {
            Err(spaceterm_core::storage::StoreFailure::RecoveryRequired)
        }
    }
    fn recover(
        &self,
        sql: &crate::storage::Transaction<'_>,
        _: &mut spaceterm_core::storage::RecoveredState,
        _: &StoreLimits,
        _: &mut super::limits::SizeBudget,
    ) -> Result<(), spaceterm_core::storage::StoreFailure> {
        for table in [
            "pairings",
            "paired_clients",
            "enrollment_authorizations",
            "pairing_receipts",
            "revocation_receipts",
            "operation_streams",
            "operation_entries",
            "git_repository_locators",
            "git_allocation_reservations",
            "git_write_intents",
            "native_retirements",
        ] {
            let count: u64 = sql
                .query_row(&format!("SELECT count(*) FROM {table}"), [], |row| {
                    row.get(0)
                })
                .map_err(super::database_adapter::classify)?;
            if count != 0 {
                return Err(spaceterm_core::storage::StoreFailure::RecoveryRequired);
            }
        }
        Ok(())
    }
}

impl SqliteOrganizationStore {
    /// Accesses this fixture's actual VFS faults without reopening its database.
    pub fn authority_test_control(&self) -> crate::storage::testing::TestControl {
        self.database.database.test_control()
    }
}
