//! Client-core's single journal authority. Pairing uses this projection port;
//! the native journal implementation and retry scheduler remain client-core owned.

use crate::pairing::{
    credentials::{PairingAuthorityFence, PairingFuture},
    journal::{PairingJournalSnapshot, PairingJournalTransaction},
};
use spaceterm_core::pairing::PairingFailure;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct JournalReceipt {
    pub revision: u64,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum JournalCommitDisposition {
    Durable(JournalReceipt),
    NotApplied(PairingFailure),
    Indeterminate,
}

/// Pairing-specific projection of the one client journal, not a second store.
/// Commit applies the complete typed change atomically with a revision CAS.
/// Recovery validates bounds, receipts and suppression before exposing bindings.
pub trait ClientJournal: Send + Sync {
    /// One process-level fence for this journal runtime, shared by all facades.
    fn pairing_authority_fence(&self) -> &PairingAuthorityFence;
    fn recover_pairings(&self)
    -> PairingFuture<'_, Result<PairingJournalSnapshot, PairingFailure>>;
    fn commit_pairing<'a>(
        &'a self,
        transaction: &'a PairingJournalTransaction,
    ) -> PairingFuture<'a, JournalCommitDisposition>;
}
