//! Pairing's desktop facade receives the native client journal at composition.
//! It opens no database, owns no task loop and cannot restore ordinary access.

use std::sync::Arc;
use spaceterm_client::{journal::ClientJournal, pairing::journal::JournalPairingStore};

#[allow(dead_code, reason = "the native client journal and desktop composition are owned by other Wave 2 tracks")]
pub(crate) fn from_client_journal(journal: Arc<dyn ClientJournal>) -> JournalPairingStore<dyn ClientJournal> {
    JournalPairingStore::new(journal)
}
