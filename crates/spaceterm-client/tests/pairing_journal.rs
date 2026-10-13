use spaceterm_client::{
    journal::{PairingClientJournal, PairingCommitDisposition, PairingJournalReceipt},
    pairing::{
        credentials::{PairingAuthorityFence, PairingFuture},
        journal::{
            JournalPairingStore, LocalPairingStore, PairingJournalChange, PairingJournalSnapshot,
            PairingJournalTransaction,
        },
    },
};
use spaceterm_core::pairing::PairingFailure;
use std::sync::{Arc, Mutex};

struct Journal {
    fence: PairingAuthorityFence,
    calls: Mutex<Vec<PairingJournalTransaction>>,
}
impl PairingClientJournal for Journal {
    fn pairing_authority_fence(&self) -> &PairingAuthorityFence {
        &self.fence
    }
    fn recover_pairings(
        &self,
    ) -> PairingFuture<'_, Result<PairingJournalSnapshot, PairingFailure>> {
        Box::pin(async { Ok(PairingJournalSnapshot::empty(0)) })
    }
    fn commit_pairing<'a>(
        &'a self,
        tx: &'a PairingJournalTransaction,
    ) -> PairingFuture<'a, PairingCommitDisposition> {
        Box::pin(async move {
            self.calls.lock().unwrap().push(tx.clone());
            PairingCommitDisposition::Durable(PairingJournalReceipt::new(
                spaceterm_client::journal::ClientJournalRevision::new(tx.expected_revision + 1),
            ))
        })
    }
}
#[tokio::test]
async fn pairing_store_delegates_notice_to_the_one_client_journal() {
    let journal = Arc::new(Journal {
        fence: PairingAuthorityFence::default(),
        calls: Mutex::new(Vec::new()),
    });
    let store = JournalPairingStore::new(journal.clone());
    let operation = spaceterm_core::operations::OperationId::new(
        spaceterm_core::identity::OperationStreamId::new(
            spaceterm_core::identity::OperationStreamKey::new(
                spaceterm_core::identity::ServerId::new([1; 16]),
                1,
            )
            .unwrap(),
            spaceterm_core::identity::PairingId::new(
                spaceterm_core::identity::ServerId::new([1; 16]),
                1,
            )
            .unwrap(),
            spaceterm_core::identity::ClientId::new(
                spaceterm_core::identity::ServerId::new([1; 16]),
                1,
            )
            .unwrap(),
        )
        .unwrap(),
        spaceterm_core::operations::OperationSequence::new(1).unwrap(),
    );
    assert!(matches!(
        store.dismiss_notice(0, operation).await,
        PairingCommitDisposition::Durable(_)
    ));
    assert!(
        matches!(&journal.calls.lock().unwrap()[0].change, PairingJournalChange::DismissNotice { operation: found } if *found == operation)
    );
}
