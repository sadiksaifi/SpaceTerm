//! SQLite projection of the core's single organization durability boundary.
//!
//! Domain owners supply complete borrowed validation and operation projections.
//! Application ownership and creation evidence come from the SQLite authority.
//! Every recovery uses its read-only multi-table snapshot.

pub mod auxiliary;
pub mod bootstrap;
pub mod codec;
mod database_adapter;
mod git_intents;
mod ledger;
pub mod limits;
mod organization;
pub mod pairing;
mod recovery;
pub mod schema;
#[cfg(feature = "test-support")]
pub mod testing;
mod transaction;
mod validation;

use spaceterm_core::authority::StoreRevision;
use spaceterm_core::identity::ServerId;
use spaceterm_core::storage::{
    CommitDisposition, DurabilityValidator, DurableTransaction, OperationWrite, OrganizationStore,
    RecoveredState, StoreFailure,
};

use crate::startup::AccountBuildKey;
use crate::storage::{Database, Transaction};
use database_adapter::DatabaseAdapter;
pub use limits::StoreLimits;
pub use schema::migrations;

/// Exact native server/account/build binding. These values grant no file authority.
#[derive(Clone, Copy, Eq, PartialEq)]
pub struct StoreIdentity {
    /// Permanent server identity from native registration.
    pub server: ServerId,
    /// Owning native account and isolated build identity.
    pub account: AccountBuildKey,
    /// Public verification key of that exact server.
    pub server_public_key: [u8; 32],
}
impl std::fmt::Debug for StoreIdentity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("StoreIdentity([redacted])")
    }
}

/// SQL helpers for the ledger, pairing, Git and native-retirement owners.
/// Helpers borrow the one authority transaction and never commit independently.
/// Recovery must fill the canonical core slots, including empty ledger/pairing
/// inventories where their format requires them. It must reject unhandled rows.
/// This port is not a domain validator or permission to execute external effects.
pub trait OperationProjection: Send + Sync {
    /// Checks operation encodings and charges their entire prepared residency.
    /// Returns a conservative upper bound on changed SQL rows, including deletes.
    fn preflight(
        &self,
        writes: &[OperationWrite],
        budget: &mut limits::SizeBudget,
    ) -> Result<usize, StoreFailure>;

    /// Applies operation projections after comparing the complete committed state.
    /// Returns the number of changed rows. Any error rolls back organization too.
    fn apply(
        &self,
        sql: &Transaction<'_>,
        parts: &spaceterm_core::storage::TransactionParts,
        previous: &RecoveredState,
    ) -> Result<usize, StoreFailure>;

    /// Decodes every operation and auxiliary table in the same bounded snapshot.
    /// Row counts and stored payload bytes have already been charged. Helpers must
    /// also charge any additional decoded allocations before making them.
    fn recover(
        &self,
        sql: &Transaction<'_>,
        recovered: &mut RecoveredState,
        limits: &StoreLimits,
        budget: &mut limits::SizeBudget,
    ) -> Result<(), StoreFailure>;
}

/// Required owner-supplied services. There is no permissive production default.
pub struct StoreServices {
    validator: Box<dyn DurabilityValidator>,
    operations: Box<dyn OperationProjection>,
}
impl StoreServices {
    /// Composes canonical ledger SQL and the real reducer, ledger, pairing and Git
    /// validators. The caller supplies the native server home and pairing owner's
    /// exact-byte receipt reader. There is no default signer or credential fallback.
    pub fn production(
        server_home: spaceterm_core::organization::ServerDirectory,
        receipts: std::sync::Arc<dyn pairing::PairingReceiptReader>,
    ) -> Self {
        Self::new(
            Box::new(validation::DomainValidator {
                home: server_home,
                receipts: receipts.clone(),
            }),
            Box::new(ledger::LedgerSql(receipts)),
        )
    }

    /// Composes the real reducer, ledger, Git and exact-byte receipt validator with
    /// the operation SQL projection. Test doubles belong only in test composition.
    pub fn new(
        validator: Box<dyn DurabilityValidator>,
        operations: Box<dyn OperationProjection>,
    ) -> Self {
        Self {
            validator,
            operations,
        }
    }
}
impl DurabilityValidator for StoreServices {
    fn validate_transaction_projection(
        &self,
        parts: &spaceterm_core::storage::TransactionParts,
    ) -> Result<(), StoreFailure> {
        self.validator.validate_transaction_projection(parts)
    }
    fn validate_recovery(&self, recovered: &RecoveredState) -> Result<(), StoreFailure> {
        self.validator.validate_recovery(recovered)
    }
}
impl std::fmt::Debug for StoreServices {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("StoreServices(..)")
    }
}

/// A fully validated recovery bundle. Auxiliary facts live in the canonical core
/// slots rather than a second host-owned copy with an independent revision.
#[derive(Eq, PartialEq)]
pub struct RecoveredBundle {
    /// Organization, ledger, pairing, Git and native retirement facts together.
    pub core: RecoveredState,
}
impl std::fmt::Debug for RecoveredBundle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("RecoveredBundle([redacted])")
    }
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum Health {
    AwaitingRecovery,
    Healthy,
    Poisoned,
}

/// Serialized, owning SQLite organization handle. Drop and securely reopen after
/// uncertain durability; rereading this connection cannot clear its health latch.
pub struct SqliteOrganizationStore {
    database: DatabaseAdapter,
    identity: StoreIdentity,
    limits: StoreLimits,
    services: StoreServices,
    health: Health,
}
impl std::fmt::Debug for SqliteOrganizationStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SqliteOrganizationStore(..)")
    }
}

impl SqliteOrganizationStore {
    /// Opens existing application-owned storage with explicit domain services.
    /// Full recovery validation completes before the handle can commit.
    pub fn open_existing(
        database: Database,
        expected: StoreIdentity,
        limits: StoreLimits,
        services: StoreServices,
    ) -> Result<Self, StoreFailure> {
        database_adapter::check_existing_prerequisites(&database, expected)?;
        let mut store = Self {
            database: DatabaseAdapter::new(database),
            identity: expected,
            limits,
            services,
            health: Health::AwaitingRecovery,
        };
        store.recover_bundle()?;
        Ok(store)
    }

    /// Explicit-service opening; identical to [`Self::open_existing`].
    pub fn open_existing_with_services(
        database: Database,
        expected: StoreIdentity,
        limits: StoreLimits,
        services: StoreServices,
    ) -> Result<Self, StoreFailure> {
        Self::open_existing(database, expected, limits, services)
    }

    /// Uses a private fixture through the real SQLite authority. The test-only
    /// opener may lack application ownership. Production opening never uses it.
    #[cfg(feature = "test-support")]
    pub fn from_test_database(
        database: Database,
        expected: StoreIdentity,
        limits: StoreLimits,
        services: StoreServices,
    ) -> Result<Self, StoreFailure> {
        database_adapter::check_test_fixture(&database)?;
        Ok(Self {
            database: DatabaseAdapter::new(database),
            identity: expected,
            limits,
            services,
            health: Health::AwaitingRecovery,
        })
    }

    /// Recovers every projection in one snapshot and opens mutation admission only
    /// after complete validation. A poisoned connection is never reread.
    pub fn recover_bundle(&mut self) -> Result<RecoveredBundle, StoreFailure> {
        if self.health == Health::Poisoned {
            return Err(StoreFailure::RecoveryRequired);
        }
        self.health = Health::AwaitingRecovery;
        let result = self
            .database
            .snapshot(|sql| recovery::load(sql, self.identity, &self.limits, &self.services));
        match result {
            Ok(core) => {
                self.health = Health::Healthy;
                Ok(RecoveredBundle { core })
            }
            Err(error) => {
                if error == StoreFailure::RecoveryRequired {
                    self.health = Health::Poisoned;
                }
                Err(error)
            }
        }
    }

    /// Reads all canonical auxiliary facts at exactly the requested store revision.
    /// No caller needs to downcast trait recovery to discover durable state.
    pub fn read_auxiliary(
        &mut self,
        expected: StoreRevision,
    ) -> Result<RecoveredBundle, StoreFailure> {
        if self.health != Health::Healthy {
            return Err(StoreFailure::RecoveryRequired);
        }
        let result = self.database.snapshot(|sql| {
            let metadata = recovery::metadata(sql, self.identity)?;
            if metadata.store_revision != expected {
                return Err(StoreFailure::RevisionConflict);
            }
            recovery::load(sql, self.identity, &self.limits, &self.services)
        });
        match result {
            Ok(core) => Ok(RecoveredBundle { core }),
            Err(error) => {
                self.fence_failure(error);
                Err(error)
            }
        }
    }

    /// Merges canonical auxiliary operation slots and makes one physical commit.
    /// Only auxiliary arms are accepted here; ledger writes already belong in the
    /// original proposal. No host-only durability path exists.
    pub fn commit_with_auxiliary(
        &mut self,
        proposal: &DurableTransaction,
        writes: &[OperationWrite],
    ) -> CommitDisposition {
        if self.health != Health::Healthy {
            return CommitDisposition::NotApplied(StoreFailure::RecoveryRequired);
        }
        if writes.is_empty() {
            return self.commit_checked(proposal);
        }
        if writes.len() > limits::MAX_AUXILIARY_WRITES
            || writes.iter().any(|write| {
                !matches!(
                    write,
                    OperationWrite::Pairing(_)
                        | OperationWrite::GitRepository(_)
                        | OperationWrite::GitIntent(_)
                        | OperationWrite::GitAllocation(_)
                        | OperationWrite::NativeRetirement(_)
                )
            })
        {
            return CommitDisposition::NotApplied(StoreFailure::InvalidTransaction);
        }
        // Preflight before cloning any variable-sized proposal or auxiliary value.
        let mut budget = self.limits.transaction_budget();
        if let Err(error) =
            transaction::preflight(proposal.parts(), &self.limits, &self.services, &mut budget)
        {
            return CommitDisposition::NotApplied(error);
        }
        if let Err(error) = self.services.operations.preflight(writes, &mut budget) {
            return CommitDisposition::NotApplied(error);
        }
        if proposal
            .parts()
            .operation_writes
            .len()
            .checked_add(writes.len())
            .is_none_or(|n| n > limits::MAX_AUXILIARY_WRITES)
        {
            return CommitDisposition::NotApplied(StoreFailure::InvalidTransaction);
        }
        let mut parts = proposal.parts().clone();
        parts.operation_writes.extend_from_slice(writes);
        match DurableTransaction::new_with_validation(parts, self.services.validator.as_ref()) {
            Ok(merged) => self.commit_checked(&merged),
            Err(error) => CommitDisposition::NotApplied(transaction::proposal_failure(error)),
        }
    }

    fn commit_checked(&mut self, proposal: &DurableTransaction) -> CommitDisposition {
        if self.health != Health::Healthy {
            return CommitDisposition::NotApplied(StoreFailure::RecoveryRequired);
        }
        let mut budget = self.limits.transaction_budget();
        if let Err(error) =
            transaction::preflight(proposal.parts(), &self.limits, &self.services, &mut budget)
        {
            return CommitDisposition::NotApplied(error);
        }
        // A callback panic cannot leave an apparently healthy store.
        self.health = Health::Poisoned;
        let disposition = self.database.commit(|sql| {
            transaction::apply(
                sql,
                proposal.parts(),
                self.identity,
                &self.limits,
                &self.services,
            )
        });
        match disposition {
            CommitDisposition::Durable(_) => self.health = Health::Healthy,
            CommitDisposition::NotApplied(error) => {
                self.health = Health::Healthy;
                self.fence_failure(error);
            }
            CommitDisposition::Indeterminate => {}
        }
        disposition
    }

    fn fence_failure(&mut self, error: StoreFailure) {
        match error {
            StoreFailure::RecoveryRequired => self.health = Health::Poisoned,
            StoreFailure::InvalidDurableState
            | StoreFailure::IdentityMismatch
            | StoreFailure::UnsafeStorage
            | StoreFailure::UnknownSchema => {
                self.health = Health::AwaitingRecovery;
            }
            _ => {}
        }
    }
}
impl OrganizationStore for SqliteOrganizationStore {
    fn recover(&mut self) -> Result<RecoveredState, StoreFailure> {
        self.recover_bundle().map(|bundle| bundle.core)
    }
    fn commit(&mut self, proposal: &DurableTransaction) -> CommitDisposition {
        self.commit_with_auxiliary(proposal, &[])
    }
}
