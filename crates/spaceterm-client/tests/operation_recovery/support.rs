#![allow(
    dead_code,
    reason = "shared frozen-port fixtures cover operations and cleanup"
)]
use spaceterm_client::{
    api::{ConnectionTarget, PairedServerBinding, RouteDescription, RouteParts},
    failure::{ClientFailure, ClientFailureClass},
    journal::*,
    operations::OperationPeer,
    ports::ClientFuture,
};
use spaceterm_core::{
    authority::{FieldVersion, StoreRevision},
    identity::*,
    operation_ledger_contracts::*,
    operations::*,
    organization::NameDraft,
    pairing::*,
};
use spaceterm_protocol::authentication::ServerPin;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};

// Explicit test adapters for the frozen codec/receipt ports. These are not a
// production encoding or signature verifier, and the real composition is separate.
#[derive(Default)]
pub struct Codec {
    requests: Mutex<Vec<(Vec<u8>, OperationId, Command)>>,
    outcomes: Mutex<Vec<(Vec<u8>, Outcome)>>,
}
impl LedgerCodec for Codec {
    fn canonical_request_digest(
        &self,
        _: &StreamBinding,
        _: OperationId,
        command: &Command,
    ) -> Result<RequestDigest, LedgerFailure> {
        RequestDigest::for_command(command)
            .map_err(|_| LedgerFailure::new(LedgerFailureClass::InvalidRequest))
    }
    fn encode_canonical_request(
        &self,
        _: &StreamBinding,
        id: OperationId,
        command: &Command,
    ) -> Result<Vec<u8>, LedgerFailure> {
        let mut records = self.requests.lock().unwrap();
        if let Some((bytes, _, _)) = records
            .iter()
            .find(|(_, original, contents)| *original == id && contents == command)
        {
            return Ok(bytes.clone());
        }
        let bytes = (records.len() as u64 + 1).to_be_bytes().to_vec();
        records.push((bytes.clone(), id, command.clone()));
        Ok(bytes)
    }
    fn decode_canonical_request(
        &self,
        bytes: &[u8],
    ) -> Result<CanonicalOperationRequest, LedgerFailure> {
        let (_, id, command) = self
            .requests
            .lock()
            .unwrap()
            .iter()
            .find(|(b, _, _)| b == bytes)
            .cloned()
            .ok_or(LedgerFailure::new(LedgerFailureClass::InvalidRequest))?;
        CanonicalOperationRequest::new(id, command, self)
    }
    fn encode_compact_outcome(&self, outcome: &Outcome) -> Result<Vec<u8>, LedgerFailure> {
        let mut records = self.outcomes.lock().unwrap();
        if let Some((bytes, _)) = records.iter().find(|(_, value)| value == outcome) {
            return Ok(bytes.clone());
        }
        let bytes = (records.len() as u64 + 1).to_be_bytes().to_vec();
        records.push((bytes.clone(), outcome.clone()));
        Ok(bytes)
    }
    fn decode_compact_outcome(&self, bytes: &[u8]) -> Result<Outcome, LedgerFailure> {
        self.outcomes
            .lock()
            .unwrap()
            .iter()
            .find(|(b, _)| b == bytes)
            .map(|(_, value)| value.clone())
            .ok_or(LedgerFailure::new(
                LedgerFailureClass::InvalidCompactOutcome,
            ))
    }
}
pub struct Verifier;
impl ReceiptVerifier for Verifier {
    fn verify(&self, pin: ServerPin, proof: &SignedPairingReceipt) -> Result<(), ClientFailure> {
        if proof.parts().claims.server().bytes() != pin.identity
            || proof.parts().signature != vec![1; 64]
        {
            return Err(ClientFailure::new(ClientFailureClass::InvalidPeer));
        }
        Ok(())
    }
}
pub struct MemoryJournal {
    state: Mutex<RecoveredClientState>,
    fault: Mutex<Option<JournalCommitDisposition>>,
    applied_uncertainty: AtomicBool,
    codec: Arc<Codec>,
}
impl MemoryJournal {
    pub fn new(codec: Arc<Codec>) -> Self {
        Self {
            state: Mutex::new(RecoveredClientState::empty()),
            fault: Mutex::new(None),
            applied_uncertainty: AtomicBool::new(false),
            codec,
        }
    }
    pub fn apply_then_indeterminate(&self) {
        self.applied_uncertainty.store(true, Ordering::SeqCst);
    }
    pub fn state(&self) -> RecoveredClientState {
        self.state.lock().unwrap().clone()
    }
    pub fn fail_next(&self, outcome: JournalCommitDisposition) {
        *self.fault.lock().unwrap() = Some(outcome);
    }
}
impl ClientJournal for MemoryJournal {
    fn recover(&self) -> ClientFuture<'_, RecoveredClientState> {
        Box::pin(async { Ok(self.state()) })
    }
    fn commit<'a>(
        &'a self,
        tx: &'a ClientJournalTransaction,
    ) -> ClientFuture<'a, JournalCommitDisposition> {
        Box::pin(async move {
            if let Some(fault) = self.fault.lock().unwrap().take() {
                return Ok(fault);
            }
            let mut state = self.state.lock().unwrap();
            match state.apply(tx, self.codec.as_ref(), &Verifier, JournalLimits::default()) {
                Ok(candidate) => {
                    let receipt = JournalReceipt::new(candidate.revision());
                    *state = candidate;
                    if self.applied_uncertainty.swap(false, Ordering::SeqCst) {
                        Ok(JournalCommitDisposition::Indeterminate)
                    } else {
                        Ok(JournalCommitDisposition::Durable(receipt))
                    }
                }
                Err(_) => Ok(JournalCommitDisposition::NotApplied(
                    JournalFailure::InvalidState,
                )),
            }
        })
    }
}
#[derive(Default)]
pub struct Peer {
    pub sent: Mutex<Vec<CanonicalOperationRequest>>,
    pub acks: Mutex<Vec<(OperationStreamId, u64)>>,
    pub lookups: Mutex<Vec<OperationBinding>>,
    pub results: Mutex<Vec<(OperationId, OperationLookup)>>,
    pub fail_ack: AtomicBool,
    pub fail_send: AtomicBool,
}
impl OperationPeer for Peer {
    fn validate_submission(
        &self,
        _: &PairedServerBinding,
        _: &Command,
        _: Option<WorkspaceId>,
    ) -> Result<(), ClientFailure> {
        Ok(())
    }
    fn submit<'a>(&'a self, request: &'a CanonicalOperationRequest) -> ClientFuture<'a, ()> {
        Box::pin(async move {
            self.sent.lock().unwrap().push(request.clone());
            if self.fail_send.load(Ordering::SeqCst) {
                Err(ClientFailure::new(ClientFailureClass::Unavailable))
            } else {
                Ok(())
            }
        })
    }
    fn lookup(&self, binding: OperationBinding) -> ClientFuture<'_, OperationLookup> {
        Box::pin(async move {
            self.lookups.lock().unwrap().push(binding);
            Ok(self
                .results
                .lock()
                .unwrap()
                .iter()
                .find(|(id, _)| *id == binding.id)
                .map(|(_, r)| r.clone())
                .unwrap_or(OperationLookup::Pending))
        })
    }
    fn acknowledge(&self, stream: OperationStreamId, through: u64) -> ClientFuture<'_, ()> {
        Box::pin(async move {
            if self.fail_ack.swap(false, Ordering::SeqCst) {
                return Err(ClientFailure::new(ClientFailureClass::Unavailable));
            }
            self.acks.lock().unwrap().push((stream, through));
            Ok(())
        })
    }
}
pub struct Fixture {
    pub journal: Arc<JournalController>,
    pub storage: Arc<MemoryJournal>,
    pub codec: Arc<Codec>,
    pub binding: PairedServerBinding,
    pub peer: Peer,
}
impl Fixture {
    pub async fn new() -> Self {
        let codec = Arc::new(Codec::default());
        let storage = Arc::new(MemoryJournal::new(codec.clone()));
        let journal = Arc::new(
            JournalController::recover(
                storage.clone(),
                codec.clone(),
                Arc::new(Verifier),
                JournalLimits::default(),
            )
            .await
            .unwrap(),
        );
        let binding = binding(1);
        journal
            .transact(vec![ClientJournalChange::SaveBinding(Box::new(saved(
                binding.clone(),
            )))])
            .await
            .unwrap();
        Self {
            journal,
            storage,
            codec,
            binding,
            peer: Peer::default(),
        }
    }
    pub async fn reopen(&self) -> Arc<JournalController> {
        Arc::new(
            JournalController::recover(
                self.storage.clone(),
                self.codec.clone(),
                Arc::new(Verifier),
                JournalLimits::default(),
            )
            .await
            .unwrap(),
        )
    }
}
pub fn server() -> ServerId {
    ServerId::new([1; 16])
}
pub fn workspace() -> WorkspaceId {
    WorkspaceId::new(server(), 1).unwrap()
}
pub fn rename(version: u64) -> Command {
    Command::RenameWorkspace {
        workspace: workspace(),
        expected_version: FieldVersion::new(version),
        draft: NameDraft::Automatic,
    }
}
pub fn binding(generation: u64) -> PairedServerBinding {
    PairedServerBinding::new(
        ConnectionTarget::new(
            ServerPin {
                identity: server().bytes(),
                public_key: [2; 32],
            },
            DeviceId::new([3; 16]),
            PairingId::new(server(), generation).unwrap(),
            ClientId::new(server(), generation).unwrap(),
            RouteDescription::new(RouteParts::Ipc {
                directory: "/private/client".into(),
                name: "s".into(),
            })
            .unwrap(),
            CredentialReference::new([4; 16]),
            LocalPairingGeneration::new(generation).unwrap(),
        )
        .unwrap(),
    )
}
pub fn saved(binding: PairedServerBinding) -> SavedBinding {
    let t = binding.target();
    let streams = PairingClientStreams::new(PairingClientStreamsParts {
        client: ClientRecord {
            id: t.client(),
            pairing: t.pairing(),
        },
        ordinary: OperationStreamId::new(
            OperationStreamKey::new(t.server(), t.incarnation().get() * 2).unwrap(),
            t.pairing(),
            t.client(),
        )
        .unwrap(),
        self_revocation: OperationStreamId::new(
            OperationStreamKey::new(t.server(), t.incarnation().get() * 2 + 1).unwrap(),
            t.pairing(),
            t.client(),
        )
        .unwrap(),
    })
    .unwrap();
    let proof = SignedPairingReceipt::new(SignedPairingReceiptParts {
        schema: 1,
        claims: PairingReceiptBody::ClientBinding {
            target: PairingTarget::new(PairingTargetParts {
                server: t.server(),
                pairing: t.pairing(),
                device: t.device(),
            })
            .unwrap(),
            keys: PairingKeys::new(PairingKeysParts {
                device_identity_public_key: [5; 32],
                pairing_authentication_public_key: [6; 32],
            })
            .unwrap(),
            client: streams.clone(),
            operation: OperationBinding {
                id: OperationId::new(streams.parts().ordinary, OperationSequence::new(1).unwrap()),
                digest: RequestDigest::from_bytes([7; 32]),
            },
            revision: StoreRevision::new(1),
        },
        canonical_body: vec![8],
        signature: vec![1; 64],
    })
    .unwrap();
    SavedBinding::verified(binding, streams, proof, &Verifier).unwrap()
}
