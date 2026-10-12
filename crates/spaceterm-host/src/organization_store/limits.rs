//! Storage ceilings and checked accounting, independent of domain admission policy.

use spaceterm_core::operation_ledger_contracts::LedgerLimits;
use spaceterm_core::pairing::MAX_PAIRING_LIFETIMES;
use spaceterm_core::retirement::MAX_NATIVE_RETIREMENTS;
use spaceterm_core::worktrees::types::MAX_GIT_INTENTS;
use thiserror::Error;

/// Maximum changed rows in one physical transaction.
pub const MAX_CHANGED_ROWS: usize = 131_072;
/// Maximum auxiliary writes in one physical transaction.
pub const MAX_AUXILIARY_WRITES: usize = 4096;
/// Maximum prepared encodings in one physical transaction.
pub const MAX_TRANSACTION_BYTES: usize = 16 * 1024 * 1024;
/// Maximum retained payload plus recovery collection residency.
pub const MAX_RECOVERY_BYTES: usize = 256 * 1024 * 1024;
/// Storage headroom, not the ledger's smaller compact-outcome admission limit.
pub const MAX_STORED_OUTCOME_BYTES: usize = 64 * 1024;
/// Storage headroom, not remote policy's eight-endpoint admission limit.
pub const MAX_STORED_BIND_ENDPOINTS: usize = 16;

/// Counted durable collections. Ordinals here are not serialized.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Collection {
    /// Shared Workspaces.
    Workspaces,
    /// Shared Tabs and their one-to-one comparison records.
    Tabs,
    /// Shared Panes and their one-to-one comparison records.
    Panes,
    /// One normalized replay instruction per surviving Pane.
    CreationReplay,
    /// Retained Worktree handles, including missing and unlisted handles.
    Worktrees,
    /// Shared repository records.
    Repositories,
    /// All pairing lifetimes, including revoked incarnations.
    Pairings,
    /// Bound Clients. Pairing and ledger quotas can impose lower simultaneous bounds.
    Clients,
    /// Enrollment reservations, including consumed and invalidated records.
    Enrollments,
    /// Public signed receipts.
    Receipts,
    /// Permanent stream headers, including retired streams.
    OperationStreams,
    /// Unacknowledged pending and completed ledger records.
    OperationEntries,
    /// Unfinished Git write intents and operation-bound allocation reservations.
    GitIntents,
    /// Native management retirement descriptors.
    NativeRetirements,
    /// Explicit remote bind endpoints.
    RemoteEndpoints,
}

impl Collection {
    const COUNT: usize = 15;
    const fn index(self) -> usize {
        self as usize
    }
    const fn hard_maximum(self) -> usize {
        match self {
            Self::Workspaces => 4096,
            Self::Tabs | Self::Worktrees => 16_384,
            Self::Panes | Self::CreationReplay | Self::Clients => 65_536,
            Self::Repositories => 8192,
            Self::Pairings | Self::Enrollments => MAX_PAIRING_LIFETIMES,
            Self::Receipts => 65_536 + MAX_PAIRING_LIFETIMES,
            Self::OperationStreams => LedgerLimits::production_v1().max_streams() as usize,
            Self::OperationEntries => LedgerLimits::production_v1().global_slots() as usize,
            Self::GitIntents => MAX_GIT_INTENTS,
            Self::NativeRetirements => MAX_NATIVE_RETIREMENTS,
            Self::RemoteEndpoints => MAX_STORED_BIND_ENDPOINTS,
        }
    }
}

/// Content-free size failure. No record or parameter value is retained.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum LimitFailure {
    /// A count, byte sum, multiplication, or allocation exceeds its bound.
    #[error("storage size limit exceeded")]
    Exceeded,
}

/// Private validated ceilings. Production callers cannot increase any bound.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StoreLimits {
    collections: [usize; Collection::COUNT],
    transaction_bytes: usize,
    recovery_bytes: usize,
}

impl StoreLimits {
    /// Returns the fixed production storage ceilings.
    pub fn production() -> Self {
        let mut collections = [0; Collection::COUNT];
        for collection in [
            Collection::Workspaces,
            Collection::Tabs,
            Collection::Panes,
            Collection::CreationReplay,
            Collection::Worktrees,
            Collection::Repositories,
            Collection::Pairings,
            Collection::Clients,
            Collection::Enrollments,
            Collection::Receipts,
            Collection::OperationStreams,
            Collection::OperationEntries,
            Collection::GitIntents,
            Collection::NativeRetirements,
            Collection::RemoteEndpoints,
        ] {
            collections[collection.index()] = collection.hard_maximum();
        }
        Self {
            collections,
            transaction_bytes: MAX_TRANSACTION_BYTES,
            recovery_bytes: MAX_RECOVERY_BYTES,
        }
    }

    /// Returns a collection's maximum before any row allocation or decoding.
    pub fn maximum(&self, collection: Collection) -> usize {
        self.collections[collection.index()]
    }

    /// Checks a SQL COUNT result without truncating to a native pointer width.
    pub fn check_count(&self, collection: Collection, count: u64) -> Result<(), LimitFailure> {
        if count > self.maximum(collection) as u64 {
            return Err(LimitFailure::Exceeded);
        }
        Ok(())
    }

    /// Checks both transaction count ceilings before encoding or effects.
    pub fn check_transaction_counts(
        &self,
        changed_rows: usize,
        auxiliary_writes: usize,
    ) -> Result<(), LimitFailure> {
        if changed_rows > MAX_CHANGED_ROWS || auxiliary_writes > MAX_AUXILIARY_WRITES {
            return Err(LimitFailure::Exceeded);
        }
        Ok(())
    }

    /// Creates the cumulative budget for prepared transaction encodings.
    pub fn transaction_budget(&self) -> SizeBudget {
        SizeBudget {
            used: 0,
            maximum: self.transaction_bytes,
        }
    }

    /// Creates the cumulative budget for payload bytes and decoded collections.
    pub fn recovery_budget(&self) -> SizeBudget {
        SizeBudget {
            used: 0,
            maximum: self.recovery_bytes,
        }
    }

    /// Lowers one ceiling in test composition. Raising a ceiling is rejected.
    #[cfg(any(test, feature = "test-support"))]
    pub fn with_test_ceiling(
        mut self,
        collection: Collection,
        maximum: usize,
    ) -> Result<Self, LimitFailure> {
        if maximum > self.maximum(collection) {
            return Err(LimitFailure::Exceeded);
        }
        self.collections[collection.index()] = maximum;
        Ok(self)
    }
}

/// Checked cumulative accounting. Failed charges leave the budget unchanged.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SizeBudget {
    used: usize,
    maximum: usize,
}
impl SizeBudget {
    /// Charges bytes before the associated allocation or encoding.
    pub fn charge(&mut self, bytes: usize) -> Result<(), LimitFailure> {
        let next = self
            .used
            .checked_add(bytes)
            .filter(|next| *next <= self.maximum)
            .ok_or(LimitFailure::Exceeded)?;
        self.used = next;
        Ok(())
    }

    /// Charges conservative decoded row residency with checked multiplication.
    pub fn charge_rows(&mut self, count: u64, bytes_per_row: usize) -> Result<(), LimitFailure> {
        let count = usize::try_from(count).map_err(|_| LimitFailure::Exceeded)?;
        self.charge(
            count
                .checked_mul(bytes_per_row)
                .ok_or(LimitFailure::Exceeded)?,
        )
    }

    /// Returns the successfully charged byte count.
    pub const fn used(self) -> usize {
        self.used
    }
}
