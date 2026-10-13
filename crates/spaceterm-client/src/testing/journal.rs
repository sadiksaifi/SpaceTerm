use super::*;
use crate::journal::*;
use spaceterm_core::{
    authority::StoreRevision, operation_ledger::canonical::CanonicalCodec,
    pairing::PairingKeysParts,
};
use spaceterm_protocol::pairing_adapter::ReceiptCodec;

pub(super) struct Verifier;
impl ReceiptVerifier for Verifier {
    fn verify(&self, pin: ServerPin, receipt: &SignedPairingReceipt) -> Result<(), ClientFailure> {
        ReceiptCodec::new(pin)
            .verify(receipt)
            .map(|_| ())
            .map_err(Into::into)
    }
}
pub(super) struct MemoryJournal(
    Mutex<RecoveredClientState>,
    crate::pairing::credentials::PairingAuthorityFence,
    Arc<std::sync::atomic::AtomicBool>,
);
impl ClientJournal for MemoryJournal {
    fn admission_latch(&self) -> Arc<JournalAdmission> {
        Arc::default()
    }
    fn scheduler_claim(&self) -> Arc<std::sync::atomic::AtomicBool> {
        Arc::clone(&self.2)
    }
    fn authority_fence(&self) -> &crate::pairing::credentials::PairingAuthorityFence {
        &self.1
    }
    fn recover(&self) -> ClientFuture<'_, RecoveredClientState> {
        Box::pin(async { Ok(self.0.lock().map_err(|_| invalid_peer())?.clone()) })
    }
    fn commit<'a>(
        &'a self,
        transaction: &'a ClientJournalTransaction,
    ) -> ClientFuture<'a, JournalCommitDisposition> {
        Box::pin(async move {
            let mut state = self.0.lock().map_err(|_| invalid_peer())?;
            let candidate = state.apply(
                transaction,
                &CanonicalCodec,
                &Verifier,
                JournalLimits::default(),
            )?;
            let receipt = JournalReceipt::new(candidate.revision());
            *state = candidate;
            Ok(JournalCommitDisposition::Durable(receipt))
        })
    }
}
pub(super) async fn controller(
    server: &FakeServer,
    client: u64,
) -> Result<Arc<JournalController>, ClientFailure> {
    let binding = server.add_client(client)?;
    let target = binding.target();
    let ordinary = *server
        .clients
        .lock()
        .map_err(|_| invalid_peer())?
        .get(&target.client())
        .ok_or_else(stale)?;
    let self_revocation = OperationStreamId::new(
        OperationStreamKey::new(
            target.server(),
            ordinary
                .key()
                .local()
                .checked_add(1)
                .ok_or_else(exhausted)?,
        )
        .map_err(|_| invalid_peer())?,
        target.pairing(),
        target.client(),
    )
    .map_err(|_| invalid_peer())?;
    let streams = PairingClientStreams::new(PairingClientStreamsParts {
        client: ClientRecord {
            id: target.client(),
            pairing: target.pairing(),
        },
        ordinary,
        self_revocation,
    })
    .map_err(|_| invalid_peer())?;
    let claims = PairingReceiptBody::ClientBinding {
        target: PairingTarget::new(PairingTargetParts {
            server: target.server(),
            pairing: target.pairing(),
            device: target.device(),
        })
        .map_err(|_| invalid_peer())?,
        keys: PairingKeys::new(PairingKeysParts {
            device_identity_public_key: DeterministicKey::new(100)?.public_key(),
            pairing_authentication_public_key: server.device_signer.public_key(),
        })
        .map_err(|_| invalid_peer())?,
        client: streams.clone(),
        operation: OperationBinding {
            id: OperationId::new(
                ordinary,
                OperationSequence::new(1).map_err(|_| invalid_peer())?,
            ),
            digest: RequestDigest::from_bytes([1; 32]),
        },
        revision: StoreRevision::new(1),
    };
    let proof = ReceiptCodec::new(target.pin()).sign(claims, server.signer.as_ref())?;
    let saved = SavedBinding::verified(binding, streams, proof, &Verifier)?;
    let storage = Arc::new(MemoryJournal(
        Mutex::new(RecoveredClientState::empty()),
        Default::default(),
        Arc::default(),
    ));
    let journal = Arc::new(
        JournalController::recover(
            storage,
            Arc::new(CanonicalCodec),
            Arc::new(Verifier),
            JournalLimits::default(),
        )
        .await?,
    );
    journal
        .transact(vec![ClientJournalChange::SaveBinding(Box::new(saved))])
        .await?;
    Ok(journal)
}
