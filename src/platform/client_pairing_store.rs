//! Pairing delegates to the desktop's one journal controller.

use spaceterm_client::{journal::JournalController, pairing::journal::JournalPairingStore};
use std::sync::Arc;

#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "desktop composition installs the canonical pairing facade"
    )
)]
pub(crate) fn from_client_journal(
    journal: Arc<JournalController>,
) -> JournalPairingStore<JournalController> {
    super::client_journal::pairing_store(journal)
}
