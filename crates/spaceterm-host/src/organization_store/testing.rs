//! Private-fixture initialization through the real authority, available only in
//! test composition. This does not certify native new-storage or application-ID
//! ownership. Real VFS fault/barrier controls remain unavailable from the authority.

use super::bootstrap::{InitializationDisposition, NewStoreSeed, selection, validate_identity};
use super::{
    Health, SqliteOrganizationStore, StoreLimits, StoreServices, database_adapter::DatabaseAdapter,
};
use crate::startup::BuildIdentity;
use crate::storage::Database;
use spaceterm_core::authority::{OrganizationRevision, StoreRevision};
use spaceterm_core::storage::CommitReceipt;

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
        let (origin, legacy, template) = match selection(&seed.origin) {
            Ok(value) => value,
            Err(error) => return InitializationDisposition::NotApplied(error),
        };
        let mut adapter = DatabaseAdapter::test_fixture(database);
        let identity = seed.identity;
        let disposition = adapter.commit(|sql| {
            // Missing metadata alone cannot distinguish a fixture from partially
            // initialized state. Any retained row rejects the whole bootstrap.
            for table in super::schema::TABLES {
                let count: u64 = sql.query_row(&format!("SELECT count(*) FROM {table}"), [], |row| row.get(0)).map_err(super::database_adapter::classify)?;
                if count != 0 {
                    return Err(spaceterm_core::storage::StoreFailure::InvalidTransaction);
                }
            }
            let zero = 0_u64.to_be_bytes();
            let one = 1_u64.to_be_bytes();
            let build = match identity.account.build_identity { BuildIdentity::Release => 0, BuildIdentity::Preflight => 1, BuildIdentity::Development => 2 };
            sql.execute("INSERT INTO store_metadata VALUES (1, ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?8, ?9, ?10)", rusqlite::params![super::schema::APPLICATION_ID, super::schema::ENCODING_VERSION, identity.server.bytes().as_slice(), u64::from(identity.account.account_uid).to_be_bytes().as_slice(), build, identity.server_public_key.as_slice(), one.as_slice(), zero.as_slice(), origin, legacy]).map_err(super::database_adapter::classify)?;
            sql.execute("INSERT INTO identity_high_water VALUES (1, ?1, ?1, ?1, ?1, ?1, ?1, ?1, ?1, ?1)", [zero.as_slice()]).map_err(super::database_adapter::classify)?;
            sql.execute("INSERT INTO server_settings VALUES (1, ?1, ?2, ?2)", rusqlite::params![template.as_str(), one.as_slice()]).map_err(super::database_adapter::classify)?;
            super::recovery::load(sql, identity, &limits, &services)?;
            Ok(CommitReceipt { store_revision: StoreRevision::new(1), organization_revision: OrganizationRevision::new(0) })
        });
        match disposition {
            spaceterm_core::storage::CommitDisposition::Durable(receipt) => {
                InitializationDisposition::Durable {
                    store: Self {
                        database: adapter,
                        identity,
                        limits,
                        services,
                        health: Health::Healthy,
                    },
                    receipt,
                }
            }
            spaceterm_core::storage::CommitDisposition::NotApplied(error) => {
                InitializationDisposition::NotApplied(error)
            }
            spaceterm_core::storage::CommitDisposition::Indeterminate => {
                InitializationDisposition::Indeterminate
            }
        }
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
